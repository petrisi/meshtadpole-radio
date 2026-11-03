//! Semtech SX1262 command interface over CH341 SPI.

use crate::ch341::Ch341;
use crate::{Error, Result};
use std::thread::sleep;
use std::time::{Duration, Instant};

/// CH341 pin assignment for the SX1262.
#[derive(Debug, Clone, Copy)]
pub struct Pins {
    pub nss: u8,
    pub reset: u8,
    pub busy: u8,
    pub dio1: u8,
    pub rxen: Option<u8>,
}

/// MeshTadpole / MeshStick / MeshToad wiring.
pub const MESHTADPOLE_PINS: Pins = Pins { nss: 0, reset: 2, busy: 4, dio1: 6, rxen: Some(1) };

pub mod cmd {
    pub const GET_STATUS: u8 = 0xC0;
    pub const READ_REGISTER: u8 = 0x1D;
    pub const WRITE_REGISTER: u8 = 0x0D;
    pub const READ_BUFFER: u8 = 0x1E;
    pub const WRITE_BUFFER: u8 = 0x0E;
    pub const SET_SLEEP: u8 = 0x84;
    pub const SET_STANDBY: u8 = 0x80;
    pub const SET_RX: u8 = 0x82;
    pub const SET_TX: u8 = 0x83;
    pub const SET_RF_FREQUENCY: u8 = 0x86;
    pub const SET_CAD_PARAMS: u8 = 0x88;
    pub const CALIBRATE: u8 = 0x89;
    pub const SET_PACKET_TYPE: u8 = 0x8A;
    pub const SET_MODULATION_PARAMS: u8 = 0x8B;
    pub const SET_PACKET_PARAMS: u8 = 0x8C;
    pub const SET_TX_PARAMS: u8 = 0x8E;
    pub const SET_BUFFER_BASE_ADDRESS: u8 = 0x8F;
    pub const SET_PA_CONFIG: u8 = 0x95;
    pub const SET_REGULATOR_MODE: u8 = 0x96;
    pub const SET_DIO3_AS_TCXO_CTRL: u8 = 0x97;
    pub const CALIBRATE_IMAGE: u8 = 0x98;
    pub const SET_DIO2_AS_RF_SWITCH_CTRL: u8 = 0x9D;
    pub const STOP_TIMER_ON_PREAMBLE: u8 = 0x9F;
    pub const SET_LORA_SYMB_NUM_TIMEOUT: u8 = 0xA0;
    pub const SET_DIO_IRQ_PARAMS: u8 = 0x08;
    pub const CLEAR_IRQ_STATUS: u8 = 0x02;
    pub const GET_IRQ_STATUS: u8 = 0x12;
    pub const GET_RX_BUFFER_STATUS: u8 = 0x13;
    pub const GET_PACKET_STATUS: u8 = 0x14;
    pub const GET_RSSI_INST: u8 = 0x15;
    pub const GET_DEVICE_ERRORS: u8 = 0x17;
    pub const CLEAR_DEVICE_ERRORS: u8 = 0x07;
    pub const SET_CAD: u8 = 0xC5;
}

pub mod reg {
    pub const VERSION_STRING: u16 = 0x0320;
    pub const LORA_SYNC_WORD: u16 = 0x0740;
    pub const IQ_POLARITY: u16 = 0x0736;
    pub const RX_GAIN: u16 = 0x08AC;
    pub const OCP: u16 = 0x08E7;
    pub const FREQ_ERROR: u16 = 0x076B;
}

pub mod irq {
    pub const TX_DONE: u16 = 1 << 0;
    pub const RX_DONE: u16 = 1 << 1;
    pub const PREAMBLE_DETECTED: u16 = 1 << 2;
    pub const HEADER_VALID: u16 = 1 << 4;
    pub const HEADER_ERR: u16 = 1 << 5;
    pub const CRC_ERR: u16 = 1 << 6;
    pub const CAD_DONE: u16 = 1 << 7;
    pub const CAD_DETECTED: u16 = 1 << 8;
    pub const TIMEOUT: u16 = 1 << 9;
    pub const ALL: u16 = 0x03FF;
}

pub struct Sx1262 {
    pub bus: Ch341,
    pub pins: Pins,
}

impl Sx1262 {
    pub fn new(mut bus: Ch341, pins: Pins) -> Result<Self> {
        bus.set_pin_mode(pins.nss, true);
        bus.set_pin_mode(pins.reset, true);
        bus.set_pin_mode(pins.busy, false);
        bus.set_pin_mode(pins.dio1, false);
        if let Some(rx) = pins.rxen {
            bus.set_pin_mode(rx, true);
        }
        bus.digital_write(pins.reset, true)?;
        bus.digital_write(pins.nss, true)?;
        Ok(Sx1262 { bus, pins })
    }

    /// Hardware reset via NRESET.
    pub fn reset(&mut self) -> Result<()> {
        self.bus.digital_write(self.pins.reset, false)?;
        sleep(Duration::from_millis(2));
        self.bus.digital_write(self.pins.reset, true)?;
        sleep(Duration::from_millis(10));
        self.wait_busy(Duration::from_millis(100))
    }

    pub fn wait_busy(&self, timeout: Duration) -> Result<()> {
        let t0 = Instant::now();
        loop {
            if self.bus.read_inputs()? & (1 << self.pins.busy) == 0 {
                return Ok(());
            }
            if t0.elapsed() > timeout {
                return Err(Error::Timeout("BUSY"));
            }
        }
    }

    /// Input pin snapshot (bit n = CH341 Dn).
    pub fn inputs(&self) -> Result<u8> {
        self.bus.read_inputs()
    }

    pub fn dio1(&self) -> Result<bool> {
        Ok(self.bus.read_inputs()? & (1 << self.pins.dio1) != 0)
    }

    /// Raw transfer: waits BUSY low, writes `out`, returns bytes clocked back.
    pub fn xfer(&mut self, out: &[u8]) -> Result<Vec<u8>> {
        self.wait_busy(Duration::from_millis(1000))?;
        let mut buf = out.to_vec();
        self.bus.spi_transaction(self.pins.nss, &mut buf)?;
        Ok(buf)
    }

    pub fn command(&mut self, op: u8, args: &[u8]) -> Result<()> {
        let mut v = Vec::with_capacity(1 + args.len());
        v.push(op);
        v.extend_from_slice(args);
        self.xfer(&v).map(|_| ())
    }

    /// Read command: opcode, one status byte, then `n` data bytes.
    pub fn read_cmd(&mut self, op: u8, n: usize) -> Result<Vec<u8>> {
        let mut v = vec![0u8; 2 + n];
        v[0] = op;
        let r = self.xfer(&v)?;
        Ok(r[2..].to_vec())
    }

    pub fn get_status(&mut self) -> Result<u8> {
        let r = self.xfer(&[cmd::GET_STATUS, 0])?;
        Ok(r[1])
    }

    pub fn read_registers(&mut self, addr: u16, n: usize) -> Result<Vec<u8>> {
        let mut v = vec![0u8; 4 + n];
        v[0] = cmd::READ_REGISTER;
        v[1..3].copy_from_slice(&addr.to_be_bytes());
        let r = self.xfer(&v)?;
        Ok(r[4..].to_vec())
    }

    pub fn write_registers(&mut self, addr: u16, data: &[u8]) -> Result<()> {
        let mut v = vec![cmd::WRITE_REGISTER];
        v.extend_from_slice(&addr.to_be_bytes());
        v.extend_from_slice(data);
        self.xfer(&v).map(|_| ())
    }

    pub fn read_buffer(&mut self, offset: u8, n: usize) -> Result<Vec<u8>> {
        let mut v = vec![0u8; 3 + n];
        v[0] = cmd::READ_BUFFER;
        v[1] = offset;
        let r = self.xfer(&v)?;
        Ok(r[3..].to_vec())
    }

    pub fn write_buffer(&mut self, offset: u8, data: &[u8]) -> Result<()> {
        let mut v = vec![cmd::WRITE_BUFFER, offset];
        v.extend_from_slice(data);
        self.xfer(&v).map(|_| ())
    }

    pub fn get_irq_status(&mut self) -> Result<u16> {
        let r = self.read_cmd(cmd::GET_IRQ_STATUS, 2)?;
        Ok(u16::from_be_bytes([r[0], r[1]]))
    }

    pub fn clear_irq(&mut self, mask: u16) -> Result<()> {
        self.command(cmd::CLEAR_IRQ_STATUS, &mask.to_be_bytes())
    }

    pub fn get_device_errors(&mut self) -> Result<u16> {
        let r = self.read_cmd(cmd::GET_DEVICE_ERRORS, 2)?;
        Ok(u16::from_be_bytes([r[0], r[1]]))
    }

    pub fn version_string(&mut self) -> Result<String> {
        let r = self.read_registers(reg::VERSION_STRING, 16)?;
        Ok(String::from_utf8_lossy(&r).trim_end_matches('\0').to_string())
    }
}

/// GetStatus byte -> (chip mode, command status).
pub fn status_fields(s: u8) -> (u8, u8) {
    ((s >> 4) & 0x7, (s >> 1) & 0x7)
}

/// LoRa modem and RF configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoraConfig {
    pub freq_hz: u32,
    pub bw_hz: u32,
    pub sf: u8,
    /// Coding rate denominator 5..=8 (4/5..4/8).
    pub cr: u8,
    /// Public sync word byte (e.g. 0x2B Meshtastic, 0x12 MeshCore).
    pub sync_word: u8,
    pub preamble_len: u16,
    pub tx_power_dbm: i8,
    /// DIO3 TCXO voltage in mV (0 = XTAL).
    pub tcxo_mv: u16,
    pub rx_boosted_gain: bool,
    /// Over-current protection limit in mA (2.5 mA steps).
    pub current_limit_ma: u16,
}

impl LoraConfig {
    pub fn bw_code(&self) -> Result<u8> {
        Ok(match self.bw_hz {
            7_800 => 0x00,
            10_400 => 0x08,
            15_600 => 0x01,
            20_800 => 0x09,
            31_250 => 0x02,
            41_700 => 0x0A,
            62_500 => 0x03,
            125_000 => 0x04,
            250_000 => 0x05,
            500_000 => 0x06,
            b => return Err(Error::Radio(format!("unsupported bandwidth {b} Hz"))),
        })
    }

    /// Symbol duration in microseconds.
    pub fn symbol_us(&self) -> f64 {
        (1u64 << self.sf) as f64 * 1e6 / self.bw_hz as f64
    }

    /// Low data rate optimization: required when symbol time >= 16.38 ms.
    pub fn ldro(&self) -> bool {
        self.symbol_us() >= 16_380.0
    }

    /// Sync word register value: `[(sw & 0xF0) | 0x4, ((sw & 0x0F) << 4) | 0x4]`.
    pub fn sync_word_reg(&self) -> [u8; 2] {
        [(self.sync_word & 0xF0) | 0x04, ((self.sync_word & 0x0F) << 4) | 0x04]
    }

    /// Time on air in microseconds for a payload of `len` bytes (explicit header, CRC on).
    pub fn time_on_air_us(&self, len: usize) -> f64 {
        let sf = self.sf as f64;
        let de = if self.ldro() { 1.0 } else { 0.0 };
        let cr = (self.cr - 4) as f64;
        let n_payload = 8.0
            + ((8.0 * len as f64 - 4.0 * sf + 28.0 + 16.0) / (4.0 * (sf - 2.0 * de))).ceil().max(0.0)
                * (cr + 4.0);
        let n_pre = self.preamble_len as f64 + 4.25;
        (n_pre + n_payload) * self.symbol_us()
    }
}

/// Received frame with RF metadata.
#[derive(Debug, Clone)]
pub struct RxFrame {
    pub data: Vec<u8>,
    pub irq: u16,
    pub crc_error: bool,
    /// Average RSSI over the packet, dBm.
    pub rssi_dbm: f32,
    /// SNR estimate, dB.
    pub snr_db: f32,
    /// RSSI of the despread signal, dBm.
    pub signal_rssi_dbm: f32,
    /// Estimated carrier frequency error, Hz.
    pub freq_error_hz: f32,
}

/// Outcome of one `poll` step.
#[derive(Debug, Clone)]
pub enum RxEvent {
    Frame(RxFrame),
    HeaderError { irq: u16, rssi_inst_dbm: f32 },
}

impl Sx1262 {
    /// Reset and fully configure the radio for LoRa. Leaves it in STDBY_XOSC/RC.
    pub fn configure(&mut self, c: &LoraConfig) -> Result<()> {
        self.reset()?;
        self.command(cmd::SET_STANDBY, &[0x00])?;
        self.set_tcxo(c)?;
        self.calibrate_all()?;
        self.apply_settings(c)
    }

    /// Calibrate(0x7F): RC64k, RC13M, PLL, ADC, image (image defaults to 902-928 MHz; `apply_settings`
    /// re-runs image calibration for the operating frequency). Calibration errors are cleared.
    fn calibrate_all(&mut self) -> Result<()> {
        self.command(cmd::CALIBRATE, &[0x7F])?;
        sleep(Duration::from_millis(5));
        self.wait_busy(Duration::from_millis(100))?;
        self.command(cmd::CLEAR_DEVICE_ERRORS, &[0, 0])
    }

    /// Light receiver recovery (warm AGC/calibration reset), as MeshCore `sx126xResetAGC` / Meshtastic
    /// SX126x `resetAGC`: RX off, warm sleep (configuration retained), wake to STDBY_RC, Calibrate(0x7F),
    /// re-apply all settings (incl. image calibration for the operating frequency, DIO2 RF switch, RX gain,
    /// sync word, IQ fix, OCP), back to continuous RX. No NRESET. Fails if the chip reports device errors
    /// afterwards; callers fall back to `configure`.
    ///
    /// Use for the SX1262 out-of-band lockup: after a strong (adjacent-channel) signal the receiver can stay
    /// deaf with idle RSSI reading up to ~35 dB high while the chip remains in RX mode.
    pub fn reset_agc(&mut self, c: &LoraConfig) -> Result<()> {
        self.command(cmd::SET_STANDBY, &[0x00])?;
        self.set_rxen(false)?;
        // Warm start, RTC wake-up off.
        self.command(cmd::SET_SLEEP, &[0x04])?;
        // >= 500 us before the wake-up edge.
        sleep(Duration::from_millis(1));
        self.wake()?;
        self.command(cmd::SET_STANDBY, &[0x00])?;
        // Retained in warm sleep per datasheet; re-sent so calibration never runs without the TCXO.
        self.set_tcxo(c)?;
        self.calibrate_all()?;
        self.apply_settings(c)?;
        self.start_rx()
    }

    /// DIO3 TCXO supply (must precede calibration).
    fn set_tcxo(&mut self, c: &LoraConfig) -> Result<()> {
        if c.tcxo_mv > 0 {
            let code = match c.tcxo_mv {
                0..=1600 => 0x00,
                1601..=1700 => 0x01,
                1701..=1800 => 0x02,
                1801..=2200 => 0x03,
                2201..=2400 => 0x04,
                2401..=2700 => 0x05,
                2701..=3000 => 0x06,
                _ => 0x07,
            };
            // 5 ms start-up delay in 15.625 us steps.
            let d: u32 = 320;
            self.command(
                cmd::SET_DIO3_AS_TCXO_CTRL,
                &[code, (d >> 16) as u8, (d >> 8) as u8, d as u8],
            )?;
        }
        Ok(())
    }

    /// Wake from sleep: NSS falling edge, then wait for BUSY low.
    fn wake(&mut self) -> Result<()> {
        self.bus.digital_write(self.pins.nss, false)?;
        sleep(Duration::from_millis(1));
        self.bus.digital_write(self.pins.nss, true)?;
        self.wait_busy(Duration::from_millis(100))
    }

    /// All modem/RF settings after calibration (shared by `configure` and `reset_agc`). Leaves standby.
    fn apply_settings(&mut self, c: &LoraConfig) -> Result<()> {
        self.command(cmd::SET_REGULATOR_MODE, &[0x01])?;
        self.command(cmd::SET_DIO2_AS_RF_SWITCH_CTRL, &[0x01])?;
        self.command(cmd::SET_PACKET_TYPE, &[0x01])?;
        self.set_frequency(c.freq_hz)?;
        self.calibrate_image(c.freq_hz)?;
        self.command(cmd::SET_PA_CONFIG, &[0x04, 0x07, 0x00, 0x01])?;
        let p = c.tx_power_dbm.clamp(-9, 22);
        self.command(cmd::SET_TX_PARAMS, &[p as u8, 0x04])?;
        let ocp = ((c.current_limit_ma as f32) / 2.5).round().min(255.0) as u8;
        self.write_registers(reg::OCP, &[ocp])?;
        self.command(cmd::SET_BUFFER_BASE_ADDRESS, &[0x00, 0x00])?;
        self.command(
            cmd::SET_MODULATION_PARAMS,
            &[c.sf, c.bw_code()?, c.cr - 4, c.ldro() as u8],
        )?;
        self.set_packet_params(c.preamble_len, 0xFF)?;
        self.write_registers(reg::LORA_SYNC_WORD, &c.sync_word_reg())?;
        // Errata 15.4: optimise IQ for standard (non-inverted) polarity.
        let iq = self.read_registers(reg::IQ_POLARITY, 1)?[0] | 0x04;
        self.write_registers(reg::IQ_POLARITY, &[iq])?;
        self.write_registers(reg::RX_GAIN, &[if c.rx_boosted_gain { 0x96 } else { 0x94 }])?;
        let all = irq::ALL.to_be_bytes();
        let dio1 = (irq::RX_DONE | irq::TX_DONE | irq::HEADER_ERR | irq::CRC_ERR | irq::TIMEOUT
            | irq::CAD_DONE)
            .to_be_bytes();
        self.command(cmd::SET_DIO_IRQ_PARAMS, &[all[0], all[1], dio1[0], dio1[1], 0, 0, 0, 0])?;
        self.clear_irq(irq::ALL)?;
        let errs = self.get_device_errors()?;
        if errs != 0 {
            return Err(Error::Radio(format!("device errors 0x{errs:04x} after configure")));
        }
        Ok(())
    }

    pub fn set_frequency(&mut self, hz: u32) -> Result<()> {
        let f = ((hz as u64) << 25) / 32_000_000;
        self.command(cmd::SET_RF_FREQUENCY, &(f as u32).to_be_bytes())
    }

    fn calibrate_image(&mut self, hz: u32) -> Result<()> {
        let mhz = hz / 1_000_000;
        let (a, b) = match mhz {
            902..=928 => (0xE1, 0xE9),
            863..=870 => (0xD7, 0xDB),
            779..=787 => (0xC1, 0xC5),
            470..=510 => (0x75, 0x81),
            430..=440 => (0x6B, 0x6F),
            _ => {
                // Bracket the frequency with 4 MHz steps.
                let lo = ((mhz.saturating_sub(4)) / 4) as u8;
                let hi = ((mhz + 4).div_ceil(4)) as u8;
                (lo, hi)
            }
        };
        self.command(cmd::CALIBRATE_IMAGE, &[a, b])
    }

    fn set_packet_params(&mut self, preamble: u16, len: u8) -> Result<()> {
        let pb = preamble.to_be_bytes();
        // Explicit header, CRC on, standard IQ.
        self.command(cmd::SET_PACKET_PARAMS, &[pb[0], pb[1], 0x00, len, 0x01, 0x00])
    }

    fn set_rxen(&mut self, on: bool) -> Result<()> {
        if let Some(p) = self.pins.rxen {
            self.bus.digital_write(p, on)?;
        }
        Ok(())
    }

    /// Enter continuous RX.
    pub fn start_rx(&mut self) -> Result<()> {
        self.clear_irq(irq::ALL)?;
        self.set_rxen(true)?;
        self.command(cmd::SET_RX, &[0xFF, 0xFF, 0xFF])
    }

    pub fn rssi_inst(&mut self) -> Result<f32> {
        let r = self.read_cmd(cmd::GET_RSSI_INST, 1)?;
        Ok(-(r[0] as f32) / 2.0)
    }

    fn freq_error_hz(&mut self, bw_hz: u32) -> Result<f32> {
        let r = self.read_registers(reg::FREQ_ERROR, 3)?;
        let mut efe: i32 = (((r[0] & 0x0F) as i32) << 16) | ((r[1] as i32) << 8) | r[2] as i32;
        if efe & 0x80000 != 0 {
            efe -= 0x100000;
        }
        Ok(1.55 * efe as f32 / (1600.0 / (bw_hz as f32 / 1000.0)))
    }

    /// Check DIO1 and, if raised, service the IRQ. Returns None when idle.
    pub fn poll(&mut self, c: &LoraConfig) -> Result<Option<RxEvent>> {
        if !self.dio1()? {
            return Ok(None);
        }
        let st = self.get_irq_status()?;
        if st & irq::RX_DONE != 0 {
            let b = self.read_cmd(cmd::GET_RX_BUFFER_STATUS, 2)?;
            let (len, start) = (b[0] as usize, b[1]);
            let p = self.read_cmd(cmd::GET_PACKET_STATUS, 3)?;
            let fe = self.freq_error_hz(c.bw_hz)?;
            let data = self.read_buffer(start, len)?;
            self.clear_irq(irq::ALL)?;
            return Ok(Some(RxEvent::Frame(RxFrame {
                data,
                irq: st,
                crc_error: st & irq::CRC_ERR != 0,
                rssi_dbm: -(p[0] as f32) / 2.0,
                snr_db: (p[1] as i8) as f32 / 4.0,
                signal_rssi_dbm: -(p[2] as f32) / 2.0,
                freq_error_hz: fe,
            })));
        }
        if st & irq::HEADER_ERR != 0 {
            let r = self.rssi_inst()?;
            self.clear_irq(irq::ALL)?;
            return Ok(Some(RxEvent::HeaderError { irq: st, rssi_inst_dbm: r }));
        }
        self.clear_irq(st)?;
        Ok(None)
    }

    /// Transmit `data` (blocking until TX_DONE), then return to continuous RX.
    pub fn transmit(&mut self, c: &LoraConfig, data: &[u8]) -> Result<Duration> {
        if data.len() > 255 {
            return Err(Error::Radio("payload > 255 bytes".into()));
        }
        self.command(cmd::SET_STANDBY, &[0x01])?;
        self.set_rxen(false)?;
        self.write_buffer(0, data)?;
        self.set_packet_params(c.preamble_len, data.len() as u8)?;
        self.clear_irq(irq::ALL)?;
        let toa = Duration::from_micros(c.time_on_air_us(data.len()) as u64);
        let t0 = Instant::now();
        self.command(cmd::SET_TX, &[0, 0, 0])?;
        let deadline = toa * 2 + Duration::from_millis(500);
        loop {
            if self.dio1()? {
                let st = self.get_irq_status()?;
                if st & irq::TX_DONE != 0 {
                    break;
                }
            }
            if t0.elapsed() > deadline {
                let _ = self.command(cmd::SET_STANDBY, &[0x01]);
                self.set_packet_params(c.preamble_len, 0xFF)?;
                self.start_rx()?;
                return Err(Error::Timeout("TX_DONE"));
            }
            sleep(Duration::from_millis(2));
        }
        let el = t0.elapsed();
        self.set_packet_params(c.preamble_len, 0xFF)?;
        self.start_rx()?;
        Ok(el)
    }
}
