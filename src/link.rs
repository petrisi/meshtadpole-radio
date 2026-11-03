//! Radio session loop shared by the protocol daemons: open with reconnect/backoff, continuous RX,
//! queued TX with CSMA and duty-cycle limit, idle noise sampling, periodic stats and RX watchdog.
//!
//! Protocol logic lives in a [`Handler`]; this module only moves frames and reports radio lifecycle.

use crate::ch341::Ch341;
use crate::sx1262::{self, LoraConfig, RxEvent, RxFrame, Sx1262};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// Unix time in seconds (host clock).
pub fn now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

/// Small xorshift for TX jitter (not cryptographic).
fn jitter_ms(max: u64) -> u64 {
    use std::cell::Cell;
    thread_local!(static S: Cell<u64> = Cell::new(now().to_bits() | 1));
    S.with(|s| {
        let mut x = s.get();
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        s.set(x);
        if max == 0 { 0 } else { x % max }
    })
}

#[derive(Debug, Clone)]
pub struct LinkConfig {
    pub lora: LoraConfig,
    /// USB serial of the stick; None = first openable.
    pub usb_serial: Option<String>,
    /// Max fraction of airtime over a rolling hour (EU868 g3 sub-band: 0.10).
    pub duty_cycle_limit: f64,
    pub duty_cycle_enforce: bool,
    /// Stats/heartbeat/watchdog interval.
    pub stats_interval: Duration,
    /// Preventive receiver reset (`Sx1262::reset_agc`) interval, MeshCore-style. Skipped while frames are
    /// being received (receiver proven alive) or a reception/TX is in progress. Tradeoff: each reset is a
    /// few tens of ms deaf; without it a lockup the stuck detector misses (e.g. under steady traffic) can
    /// last until the next detection. None = only reactive recovery.
    pub agc_reset_interval: Option<Duration>,
    /// Updated on every loop iteration; watch it with `spawn_watchdog`.
    pub liveness: Arc<Liveness>,
}

/// Time of the last radio loop iteration (connected or not), for detecting a blocked loop.
#[derive(Debug, Default)]
pub struct Liveness(std::sync::atomic::AtomicU64);

impl Liveness {
    pub fn tick(&self) {
        self.0.store((now() * 1000.0) as u64, Ordering::Relaxed);
    }
    /// Seconds since the last iteration (None before the first).
    pub fn age_s(&self) -> Option<f64> {
        match self.0.load(Ordering::Relaxed) {
            0 => None,
            ms => Some(now() - ms as f64 / 1000.0),
        }
    }
}

/// Call `on_stall(age_s)` once if the loop has not iterated for `max`. Checks every 5 s. A blocked loop
/// (e.g. a USB call that never returns) cannot be recovered in-process; daemons log and exit so a
/// service manager restarts them.
pub fn spawn_watchdog(l: Arc<Liveness>, max: Duration, on_stall: impl FnOnce(f64) + Send + 'static) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            if let Some(a) = l.age_s() {
                if a > max.as_secs_f64() {
                    on_stall(a);
                    return;
                }
            }
        }
    });
}

/// Transmit request. `meta` is opaque to the link and handed back in [`Handler::transmitted`].
pub struct TxJob<M> {
    pub frame: Vec<u8>,
    pub meta: M,
    /// Earliest transmit time relative to enqueue (random backoff chosen by the caller).
    pub delay: Duration,
    pub done: Option<Sender<Result<TxReport, String>>>,
}

#[derive(Debug, Clone)]
pub struct TxReport {
    /// Id returned by the handler (e.g. DB row id).
    pub id: i64,
    pub tx_time: f64,
    pub len: usize,
    pub airtime_ms: f64,
}

/// One stats interval.
#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub t_start: f64,
    pub t_end: f64,
    /// Instantaneous RSSI sampled ~1/s while idle (dBm).
    pub noise_samples: u32,
    pub noise_min: Option<f32>,
    pub noise_avg: Option<f32>,
    pub noise_max: Option<f32>,
    pub frames: u32,
    pub crc_errors: u32,
    pub header_errors: u32,
    pub rx_airtime_ms: f64,
}

#[derive(Debug, Clone)]
pub enum Event {
    Connected {
        usb_serial: String,
    },
    Disconnected {
        error: String,
    },
    /// Logged only when the error text changes.
    OpenFailed {
        error: String,
    },
    /// Receiver recovery. `detail` starts with the trigger: `agc_stuck:` (stuck heuristic, light reset),
    /// `agc_periodic:` (interval reset), `mode_watchdog:` (chip not in RX,
    /// full reconfigure). `fallback=configure` in detail: light reset failed, full reconfigure done.
    RxRestored {
        detail: String,
    },
}

impl Event {
    pub fn name(&self) -> &'static str {
        match self {
            Event::Connected { .. } => "connected",
            Event::Disconnected { .. } => "disconnected",
            Event::OpenFailed { .. } => "open_failed",
            Event::RxRestored { .. } => "rx_restored",
        }
    }
    pub fn detail(&self) -> &str {
        match self {
            Event::Connected { usb_serial } => usb_serial,
            Event::Disconnected { error } | Event::OpenFailed { error } => error,
            Event::RxRestored { detail } => detail,
        }
    }
}

/// Rolling one-hour TX airtime log, shareable with API threads.
#[derive(Default)]
pub struct Airtime(Mutex<VecDeque<(Instant, f64)>>);

impl Airtime {
    pub fn last_hour_ms(&self) -> f64 {
        let mut log = self.0.lock().unwrap();
        while log.front().is_some_and(|(t, _)| t.elapsed() > Duration::from_secs(3600)) {
            log.pop_front();
        }
        log.iter().map(|(_, a)| a).sum::<f64>() + 0.0 // empty sum is -0.0
    }
    fn push(&self, ms: f64) {
        self.0.lock().unwrap().push_back((Instant::now(), ms));
    }
}

/// Protocol side of the loop. All calls happen on the link thread. Errors returned by the
/// handler stop the loop (they are not radio errors).
pub trait Handler {
    type Meta;
    fn event(&mut self, ev: &Event) -> Result<(), BoxError>;
    /// A received frame (CRC errors included). Returns frames to transmit in response.
    fn frame(&mut self, t: f64, f: &RxFrame, airtime_ms: f64) -> Result<Vec<TxJob<Self::Meta>>, BoxError>;
    fn header_error(&mut self, t: f64, rssi_inst_dbm: f32) -> Result<(), BoxError>;
    /// Called after every TX attempt (also refused by duty cycle: `error` set, nothing sent). Returns an id for [`TxReport`].
    fn transmitted(&mut self, job: &TxJob<Self::Meta>, t: f64, airtime_ms: f64, error: Option<&str>) -> Result<i64, BoxError>;
    fn stats(&mut self, s: &Stats) -> Result<(), BoxError>;
    /// Called every stats interval, also while disconnected.
    fn heartbeat(&mut self) -> Result<(), BoxError>;
}

/// Open, configure and start RX. Returns the radio and the stick's USB serial.
pub fn open(lora: &LoraConfig, usb_serial: Option<&str>) -> crate::Result<(Sx1262, String)> {
    let bus = Ch341::open(usb_serial)?;
    let serial = bus.info.serial.clone().unwrap_or_default();
    let mut radio = Sx1262::new(bus, sx1262::MESHTADPOLE_PINS)?;
    radio.configure(lora)?;
    radio.start_rx()?;
    Ok((radio, serial))
}

/// Channel busy: preamble/header detected without completion. Stale flags (> 3 s) are cleared.
fn channel_busy(radio: &mut Sx1262, since: &mut Option<Instant>) -> crate::Result<bool> {
    let irq = radio.get_irq_status()?;
    let active = irq & (sx1262::irq::PREAMBLE_DETECTED | sx1262::irq::HEADER_VALID) != 0 && irq & sx1262::irq::RX_DONE == 0;
    if !active {
        *since = None;
        return Ok(false);
    }
    let t = *since.get_or_insert_with(Instant::now);
    if t.elapsed() > Duration::from_secs(3) {
        radio.clear_irq(sx1262::irq::PREAMBLE_DETECTED | sx1262::irq::HEADER_VALID)?;
        *since = None;
        return Ok(false);
    }
    Ok(true)
}

/// Detects the SX1262 out-of-band lockup (RadioLib errata): after a strong (adjacent-channel) signal the
/// receiver stays deaf with idle RSSI flat and inflated by up to ~35 dB while the chip remains in RX mode.
/// Observed on the MeshTadpole at 869.618 MHz with Meshtastic traffic 187 kHz away: flat ~-76 dBm.
struct Stuck {
    floor: Option<f32>,
    floor_t: Instant,
    win: VecDeque<f32>,
    backoff: Duration,
    next_check: Instant,
    last_reset: Option<Instant>,
}

impl Stuck {
    /// Idle samples (~1/s) in the window.
    const WIN: usize = 8;
    /// Window spread (dB) at most, and minimum elevation above the floor (dB).
    const SPREAD: f32 = 3.0;
    const ELEVATION: f32 = 12.0;
    /// A floor never below this is itself suspect (receiver stuck since the session started): a flat
    /// window above it also triggers. Typical MeshTadpole idle floor at 62.5 kHz: -95..-100 dBm.
    const SUSPECT: f32 = -88.0;
    /// Readings at or below this are invalid (chip just entered RX).
    const INVALID: f32 = -125.0;

    fn new() -> Stuck {
        Stuck {
            floor: None,
            floor_t: Instant::now(),
            win: VecDeque::new(),
            backoff: Duration::ZERO,
            next_check: Instant::now(),
            last_reset: None,
        }
    }

    /// CRC-good reception: the receiver works.
    fn activity(&mut self) {
        self.win.clear();
    }

    /// Returns (stuck rssi, floor) when the window shows a stuck receiver.
    fn sample(&mut self, r: f32) -> Option<(f32, f32)> {
        // GetRssiInst reads -127.5 dBm (no valid measurement) right after entering RX; never a floor.
        if r <= Self::INVALID {
            return None;
        }
        // Floor: lowest idle sample; re-estimated from the window every 10 min so it can rise.
        if self.floor_t.elapsed() > Duration::from_secs(600) && self.win.len() == Self::WIN {
            self.floor = self.win.iter().copied().reduce(f32::min);
            self.floor_t = Instant::now();
        }
        if self.floor.is_none_or(|f| r < f) {
            self.floor = Some(r);
        }
        self.win.push_back(r);
        if self.win.len() > Self::WIN {
            self.win.pop_front();
        }
        if self.win.len() < Self::WIN || Instant::now() < self.next_check {
            return None;
        }
        let mn = self.win.iter().copied().reduce(f32::min)?;
        let mx = self.win.iter().copied().reduce(f32::max)?;
        let floor = self.floor?;
        let flat = mx - mn <= Self::SPREAD;
        (flat && (mn > floor + Self::ELEVATION || (floor > Self::SUSPECT && mn > Self::SUSPECT))).then_some((mn, floor))
    }

    /// After a reset: repeated triggers within 2 min back off (constant real interferer), up to 10 min.
    fn reset_done(&mut self) {
        self.backoff = match self.last_reset {
            Some(t) if t.elapsed() < Duration::from_secs(120) => {
                (self.backoff * 2).clamp(Duration::from_secs(30), Duration::from_secs(600))
            }
            _ => Duration::ZERO,
        };
        self.last_reset = Some(Instant::now());
        self.next_check = Instant::now() + self.backoff;
        self.win.clear();
    }
}

/// Light receiver reset; on failure or if the chip is not back in RX, full reconfigure.
/// Returns event detail suffix (duration, fallback).
fn recover(radio: &mut Sx1262, lora: &LoraConfig) -> crate::Result<String> {
    let t0 = Instant::now();
    let light = radio.reset_agc(lora).and_then(|_| radio.get_status()).map(|s| sx1262::status_fields(s).0);
    match light {
        Ok(5) => Ok(format!("reset_agc {} ms", t0.elapsed().as_millis())),
        other => {
            let why = match other {
                Ok(mode) => format!("chip mode {mode} after reset_agc"),
                Err(e) => format!("reset_agc failed: {e}"),
            };
            radio.configure(lora)?;
            radio.start_rx()?;
            Ok(format!("fallback=configure ({why}) {} ms", t0.elapsed().as_millis()))
        }
    }
}

struct Acc {
    t0: f64,
    n: u32,
    nmin: f32,
    nsum: f32,
    nmax: f32,
    frames: u32,
    crc: u32,
    hdr: u32,
    air: f64,
}

impl Acc {
    fn new() -> Acc {
        Acc { t0: now(), n: 0, nmin: f32::MAX, nsum: 0.0, nmax: f32::MIN, frames: 0, crc: 0, hdr: 0, air: 0.0 }
    }
    fn take(&mut self) -> Stats {
        let t1 = now();
        let n = self.n;
        let s = Stats {
            t_start: self.t0,
            t_end: t1,
            noise_samples: n,
            noise_min: (n > 0).then_some(self.nmin),
            noise_avg: (n > 0).then(|| self.nsum / n as f32),
            noise_max: (n > 0).then_some(self.nmax),
            frames: self.frames,
            crc_errors: self.crc,
            header_errors: self.hdr,
            rx_airtime_ms: self.air,
        };
        *self = Acc::new();
        self.t0 = t1;
        s
    }
}

enum Exit {
    Stop,
    Radio(crate::Error),
    Handler(BoxError),
}

impl From<crate::Error> for Exit {
    fn from(e: crate::Error) -> Self {
        Exit::Radio(e)
    }
}

/// Run until `stop` is set or the handler fails. Radio errors cause reconnect (backoff 1..30 s);
/// the loop starts even without a stick.
pub fn run<H: Handler>(
    cfg: &LinkConfig,
    h: &mut H,
    jobs: &Receiver<TxJob<H::Meta>>,
    airtime: &Arc<Airtime>,
    stop: &AtomicBool,
) -> Result<(), BoxError> {
    let lora = cfg.lora;
    let mut radio_opt: Option<Sx1262> = None;
    let mut backoff = Duration::from_secs(1);
    let mut next_try = Instant::now();
    let mut last_open_err: Option<String> = None;
    let mut acc = Acc::new();
    let mut last_stat = Instant::now();
    let mut pending: VecDeque<(TxJob<H::Meta>, Instant)> = VecDeque::new();

    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let Some(radio) = radio_opt.as_mut() else {
            // Disconnected: fail queued TX, keep heartbeat, retry open with backoff.
            while let Ok(job) = jobs.try_recv() {
                fail(job, "radio not connected");
            }
            for (job, _) in pending.drain(..) {
                fail(job, "radio not connected");
            }
            if last_stat.elapsed() >= cfg.stats_interval {
                h.heartbeat()?;
                last_stat = Instant::now();
                acc = Acc::new();
            }
            if Instant::now() >= next_try {
                match open(&lora, cfg.usb_serial.as_deref()) {
                    Ok((r, serial)) => {
                        h.event(&Event::Connected { usb_serial: serial })?;
                        radio_opt = Some(r);
                        backoff = Duration::from_secs(1);
                        last_open_err = None;
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        if last_open_err.as_deref() != Some(msg.as_str()) {
                            h.event(&Event::OpenFailed { error: msg.clone() })?;
                            last_open_err = Some(msg);
                        }
                        next_try = Instant::now() + backoff;
                        backoff = (backoff * 2).min(Duration::from_secs(30));
                    }
                }
            }
            cfg.liveness.tick();
            std::thread::sleep(Duration::from_millis(200));
            continue;
        };
        match connected(cfg, radio, h, jobs, airtime, stop, &mut acc, &mut last_stat, &mut pending) {
            Exit::Stop => return Ok(()),
            Exit::Handler(e) => return Err(e),
            Exit::Radio(e) => {
                h.event(&Event::Disconnected { error: e.to_string() })?;
                radio_opt = None;
                backoff = Duration::from_secs(1);
                next_try = Instant::now() + backoff;
            }
        }
    }
}

fn fail<M>(job: TxJob<M>, msg: &str) {
    if let Some(d) = job.done {
        let _ = d.send(Err(msg.to_string()));
    }
}

#[allow(clippy::too_many_arguments)]
fn connected<H: Handler>(
    cfg: &LinkConfig,
    radio: &mut Sx1262,
    h: &mut H,
    jobs: &Receiver<TxJob<H::Meta>>,
    airtime: &Arc<Airtime>,
    stop: &AtomicBool,
    acc: &mut Acc,
    last_stat: &mut Instant,
    pending: &mut VecDeque<(TxJob<H::Meta>, Instant)>,
) -> Exit {
    let lora = &cfg.lora;
    let mut busy_since: Option<Instant> = None;
    let mut last_noise = Instant::now();
    let mut stuck = Stuck::new();
    let mut last_frame: Option<Instant> = None;
    let mut next_periodic = cfg.agc_reset_interval.map(|i| Instant::now() + i);
    let r: Result<(), Exit> = (|| {
        while !stop.load(Ordering::SeqCst) {
            cfg.liveness.tick();
            while let Ok(job) = jobs.try_recv() {
                let at = Instant::now() + job.delay;
                pending.push_back((job, at));
            }
            let mut did_rx = false;
            match radio.poll(lora)? {
                Some(RxEvent::Frame(f)) => {
                    did_rx = true;
                    // Only a CRC-good frame proves the receiver works; header/CRC errors can come from
                    // strong energy while the front end is partly deaf.
                    if !f.crc_error {
                        stuck.activity();
                        last_frame = Some(Instant::now());
                    }
                    let t = now();
                    let air = lora.time_on_air_us(f.data.len()) / 1000.0;
                    acc.frames += 1;
                    acc.crc += f.crc_error as u32;
                    acc.air += air;
                    for job in h.frame(t, &f, air).map_err(Exit::Handler)? {
                        let at = Instant::now() + job.delay;
                        pending.push_back((job, at));
                    }
                }
                Some(RxEvent::HeaderError { rssi_inst_dbm, .. }) => {
                    did_rx = true;
                    acc.hdr += 1;
                    h.header_error(now(), rssi_inst_dbm).map_err(Exit::Handler)?;
                }
                None => {}
            }
            if !did_rx {
                if pending.front().is_some_and(|(_, at)| Instant::now() >= *at) {
                    if channel_busy(radio, &mut busy_since)? {
                        // Defer the head job by a random 200-800 ms.
                        pending.front_mut().unwrap().1 = Instant::now() + Duration::from_millis(200 + jitter_ms(600));
                    } else {
                        let (job, _) = pending.pop_front().unwrap();
                        transmit(cfg, radio, h, airtime, job)?;
                    }
                } else if pending.is_empty() && last_noise.elapsed() >= Duration::from_secs(1) {
                    let r = radio.rssi_inst()?;
                    last_noise = Instant::now();
                    if r > Stuck::INVALID {
                        acc.n += 1;
                        acc.nmin = acc.nmin.min(r);
                        acc.nmax = acc.nmax.max(r);
                        acc.nsum += r;
                    }
                    if let Some((at, floor)) = stuck.sample(r) {
                        let how = recover(radio, lora)?;
                        stuck.reset_done();
                        let detail = format!("agc_stuck: rssi flat at {at:.0} dBm (floor {floor:.0} dBm), no reception; {how}");
                        h.event(&Event::RxRestored { detail }).map_err(Exit::Handler)?;
                    }
                } else if pending.is_empty() && next_periodic.is_some_and(|t| Instant::now() >= t) {
                    let iv = cfg.agc_reset_interval.unwrap();
                    next_periodic = Some(Instant::now() + iv);
                    let alive = last_frame.is_some_and(|t| t.elapsed() < iv);
                    if !alive && !channel_busy(radio, &mut busy_since)? {
                        let how = recover(radio, lora)?;
                        stuck.activity();
                        h.event(&Event::RxRestored { detail: format!("agc_periodic: {how}") }).map_err(Exit::Handler)?;
                    }
                }
                std::thread::sleep(Duration::from_millis(3));
            }
            if last_stat.elapsed() >= cfg.stats_interval {
                let s = acc.take();
                h.stats(&s).map_err(Exit::Handler)?;
                h.heartbeat().map_err(Exit::Handler)?;
                // Mode watchdog: chip must be in RX (mode 5) when idle; otherwise full reconfigure.
                let (mode, _) = sx1262::status_fields(radio.get_status()?);
                if mode != 5 && pending.is_empty() {
                    radio.configure(lora)?;
                    radio.start_rx()?;
                    h.event(&Event::RxRestored { detail: format!("mode_watchdog: chip mode was {mode}; configure") })
                        .map_err(Exit::Handler)?;
                }
                *last_stat = Instant::now();
            }
        }
        Ok(())
    })();
    match r {
        Ok(()) => Exit::Stop,
        Err(e) => e,
    }
}

fn transmit<H: Handler>(cfg: &LinkConfig, radio: &mut Sx1262, h: &mut H, airtime: &Arc<Airtime>, job: TxJob<H::Meta>) -> Result<(), Exit> {
    let air = cfg.lora.time_on_air_us(job.frame.len()) / 1000.0;
    let budget = cfg.duty_cycle_limit * 3_600_000.0;
    let used = airtime.last_hour_ms();
    let t = now();
    if cfg.duty_cycle_enforce && used + air > budget {
        let msg = format!("duty cycle limit: {used:.0} ms used of {budget:.0} ms in last hour");
        h.transmitted(&job, t, air, Some(&msg)).map_err(Exit::Handler)?;
        fail(job, &msg);
        return Ok(());
    }
    let r = radio.transmit(&cfg.lora, &job.frame);
    let err = r.as_ref().err().map(|e| e.to_string());
    let id = h.transmitted(&job, t, air, err.as_deref()).map_err(Exit::Handler)?;
    match r {
        Ok(_) => {
            airtime.push(air);
            if let Some(d) = &job.done {
                let _ = d.send(Ok(TxReport { id, tx_time: t, len: job.frame.len(), airtime_ms: (air * 10.0).round() / 10.0 }));
            }
            Ok(())
        }
        Err(e) => {
            fail(job, &e.to_string());
            Err(Exit::Radio(e))
        }
    }
}
