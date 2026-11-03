//! RX diagnostics: polls IRQ status over SPI (not DIO1), reports chip mode, RSSI, pin levels.
//! Usage: diag <freq_hz> <bw_hz> <sf> <cr> <sync_hex> <preamble> <seconds> [agc_reset_every_s]
//! With agc_reset_every_s: call `reset_agc` periodically and print RSSI before/after and its duration.
use meshtadpole_radio::{ch341, sx1262};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let a: Vec<String> = std::env::args().collect();
    let cfg = sx1262::LoraConfig {
        freq_hz: a[1].parse()?,
        bw_hz: a[2].parse()?,
        sf: a[3].parse()?,
        cr: a[4].parse()?,
        sync_word: u8::from_str_radix(&a[5], 16)?,
        preamble_len: a[6].parse()?,
        tx_power_dbm: 0,
        tcxo_mv: 1800,
        rx_boosted_gain: true,
        current_limit_ma: 140,
    };
    let secs: u64 = a[7].parse()?;
    let agc_every: Option<u64> = a.get(8).map(|x| x.parse()).transpose()?;
    let mut last_agc = Instant::now();
    let mut r = sx1262::Sx1262::new(ch341::Ch341::open(None)?, sx1262::MESHTADPOLE_PINS)?;
    r.configure(&cfg)?;
    r.start_rx()?;
    let st = r.get_status()?;
    println!("after start_rx status=0x{st:02x} mode/cmd={:?} errors=0x{:04x}", sx1262::status_fields(st), r.get_device_errors()?);
    let t0 = Instant::now();
    let mut last = Instant::now() - Duration::from_secs(10);
    let (mut rmin, mut rmax, mut rsum, mut n) = (0f32, -200f32, 0f32, 0u32);
    while t0.elapsed().as_secs() < secs {
        let irq = r.get_irq_status()?;
        let rssi = r.rssi_inst()?;
        rmin = rmin.min(rssi);
        rmax = rmax.max(rssi);
        rsum += rssi;
        n += 1;
        if irq != 0 {
            let pins = r.inputs()?;
            println!("t={:.2} irq=0x{irq:04x} pins={pins:08b} rssi={rssi}", t0.elapsed().as_secs_f32());
            if irq & sx1262::irq::RX_DONE != 0 {
                let b = r.read_cmd(sx1262::cmd::GET_RX_BUFFER_STATUS, 2)?;
                let p = r.read_cmd(sx1262::cmd::GET_PACKET_STATUS, 3)?;
                let d = r.read_buffer(b[1], b[0] as usize)?;
                let hex: String = d.iter().map(|b| format!("{b:02x}")).collect();
                println!("  frame len={} rssi={} snr={} hex={hex}", b[0], -(p[0] as f32) / 2.0, (p[1] as i8) as f32 / 4.0);
            }
            r.clear_irq(irq)?;
        }
        if agc_every.is_some_and(|e| last_agc.elapsed() >= Duration::from_secs(e)) {
            let before = r.rssi_inst()?;
            let t = Instant::now();
            r.reset_agc(&cfg)?;
            let ms = t.elapsed().as_millis();
            std::thread::sleep(Duration::from_millis(50));
            let st = r.get_status()?;
            println!(
                "t={:.1} reset_agc {ms} ms rssi before={before} after={} mode/cmd={:?} errors=0x{:04x}",
                t0.elapsed().as_secs_f32(),
                r.rssi_inst()?,
                sx1262::status_fields(st),
                r.get_device_errors()?
            );
            let sync = r.read_registers(sx1262::reg::LORA_SYNC_WORD, 2)?;
            let gain = r.read_registers(sx1262::reg::RX_GAIN, 1)?;
            println!(
                "  sync_reg={:02x}{:02x} (expect {:02x}{:02x}) rx_gain={:02x}",
                sync[0],
                sync[1],
                cfg.sync_word_reg()[0],
                cfg.sync_word_reg()[1],
                gain[0]
            );
            last_agc = Instant::now();
        }
        if last.elapsed() > Duration::from_secs(10) {
            let st = r.get_status()?;
            println!(
                "t={:.0} mode/cmd={:?} rssi_inst min={rmin} avg={:.1} max={rmax} pins={:08b}",
                t0.elapsed().as_secs_f32(),
                sx1262::status_fields(st),
                rsum / n as f32,
                r.inputs()?
            );
            (rmin, rmax, rsum, n) = (0.0, -200.0, 0.0, 0);
            last = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
