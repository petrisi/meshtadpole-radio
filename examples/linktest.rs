//! Run link::run with a printing handler (MeshCore modem settings). Usage: linktest <freq_hz> <stats_s> [run_s] [agc_reset_interval_s]
use meshtadpole_radio::link::{self, BoxError, Event, Handler, Stats, TxJob};
use meshtadpole_radio::sx1262::{LoraConfig, RxFrame};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, mpsc};
use std::time::Duration;

struct H;
impl Handler for H {
    type Meta = ();
    fn event(&mut self, ev: &Event) -> Result<(), BoxError> {
        println!("event {} {}", ev.name(), ev.detail());
        Ok(())
    }
    fn frame(&mut self, _t: f64, f: &RxFrame, _a: f64) -> Result<Vec<TxJob<()>>, BoxError> {
        println!("frame len={} rssi={} crc_err={}", f.data.len(), f.rssi_dbm, f.crc_error);
        Ok(vec![])
    }
    fn header_error(&mut self, _t: f64, r: f32) -> Result<(), BoxError> {
        println!("header_error {r}");
        Ok(())
    }
    fn transmitted(&mut self, _j: &TxJob<()>, _t: f64, _a: f64, _e: Option<&str>) -> Result<i64, BoxError> {
        Ok(0)
    }
    fn stats(&mut self, s: &Stats) -> Result<(), BoxError> {
        println!("t={:.0} noise {:?}/{:?}/{:?} n={} frames={}", s.t_end, s.noise_min, s.noise_avg, s.noise_max, s.noise_samples, s.frames);
        Ok(())
    }
    fn heartbeat(&mut self) -> Result<(), BoxError> {
        Ok(())
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let cfg = link::LinkConfig {
        lora: LoraConfig {
            freq_hz: a[1].parse().unwrap(),
            bw_hz: 62_500,
            sf: 8,
            cr: 8,
            sync_word: 0x12,
            preamble_len: 32,
            tx_power_dbm: 14,
            tcxo_mv: 1800,
            rx_boosted_gain: true,
            current_limit_ma: 140,
        },
        usb_serial: None,
        duty_cycle_limit: 0.1,
        duty_cycle_enforce: true,
        stats_interval: Duration::from_secs(a[2].parse().unwrap()),
        agc_reset_interval: a.get(4).map(|x| Duration::from_secs(x.parse().unwrap())),
        liveness: Default::default(),
    };
    let (_tx, rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let s2 = stop.clone();
    let secs: u64 = a.get(3).map(|x| x.parse().unwrap()).unwrap_or(120);
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        s2.store(true, std::sync::atomic::Ordering::SeqCst);
    });
    link::run(&cfg, &mut H, &rx, &Arc::new(link::Airtime::default()), &stop).unwrap();
}
