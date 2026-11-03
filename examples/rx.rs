//! Continuous raw LoRa receive. Usage: rx <freq_hz> <bw_hz> <sf> <cr> <sync_hex> <preamble> [seconds]
//! EFL (Meshtastic):   rx 869431250 62500 8 8 2b 16
//! EU/UK Narrow (MC):  rx 869618000 62500 8 8 12 16
use meshtadpole_radio::{ch341, sx1262};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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
    let secs: u64 = a.get(7).map(|s| s.parse()).transpose()?.unwrap_or(u64::MAX);
    let mut r = sx1262::Sx1262::new(ch341::Ch341::open(None)?, sx1262::MESHTADPOLE_PINS)?;
    r.configure(&cfg)?;
    r.start_rx()?;
    eprintln!("rx {cfg:?}");
    let t0 = Instant::now();
    while t0.elapsed().as_secs() < secs {
        match r.poll(&cfg)? {
            Some(ev) => {
                let ts = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs_f64();
                match ev {
                    sx1262::RxEvent::Frame(f) => {
                        let hex: String = f.data.iter().map(|b| format!("{b:02x}")).collect();
                        println!(
                            "{{\"ts\":{ts:.3},\"len\":{},\"crc_error\":{},\"rssi\":{},\"snr\":{},\"signal_rssi\":{},\"freq_error_hz\":{:.0},\"hex\":\"{hex}\"}}",
                            f.data.len(),
                            f.crc_error,
                            f.rssi_dbm,
                            f.snr_db,
                            f.signal_rssi_dbm,
                            f.freq_error_hz
                        );
                    }
                    sx1262::RxEvent::HeaderError { irq, rssi_inst_dbm } => {
                        println!("{{\"ts\":{ts:.3},\"header_error\":true,\"irq\":{irq},\"rssi_inst\":{rssi_inst_dbm}}}");
                    }
                }
            }
            None => std::thread::sleep(Duration::from_millis(3)),
        }
    }
    Ok(())
}
