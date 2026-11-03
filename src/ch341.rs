//! WCH CH341 USB-SPI/GPIO bridge (USB 1a86:5512), userspace via libusb.
//!
//! Wire protocol (from libch341-spi-userspace / Meshtastic portduino):
//! - Interface 0, bulk OUT 0x02, bulk IN 0x82.
//! - SPI: command 0xA8 followed by up to 31 data bytes per 32-byte packet. Bytes are
//!   bit-reversed on the wire in both directions. Full duplex: N bytes out, N bytes in.
//! - GPIO D0..D5 output: UIO stream `[0xAB, 0x80|state, 0x40|dir, 0x20]`.
//! - Inputs: `[0xA0]` -> reply, byte 0 = D0..D7 levels.

use crate::{Error, Result};
use rusb::{Context, DeviceHandle, UsbContext};
use std::time::Duration;

pub const VID: u16 = 0x1A86;
pub const PID: u16 = 0x5512;

const EP_OUT: u8 = 0x02;
const EP_IN: u8 = 0x82;
const TIMEOUT: Duration = Duration::from_millis(1000);

const CMD_SPI_STREAM: u8 = 0xA8;
const CMD_UIO_STREAM: u8 = 0xAB;
const UIO_STM_OUT: u8 = 0x80;
const UIO_STM_DIR: u8 = 0x40;
const UIO_STM_END: u8 = 0x20;
const CMD_GET_STATUS: u8 = 0xA0;
const PACKET_LEN: usize = 32;
const DATA_PER_PACKET: usize = PACKET_LEN - 1;

pub const PIN_SCK: u8 = 3;
pub const PIN_MOSI: u8 = 5;

/// USB-level description of an attached CH341.
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub bus: u8,
    pub address: u8,
    pub port_path: Vec<u8>,
    pub vid: u16,
    pub pid: u16,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
    /// Raw product string descriptor bytes as returned by GET_DESCRIPTOR.
    pub product_raw: Vec<u8>,
    pub open_error: Option<String>,
}

fn read_string_raw<T: UsbContext>(h: &DeviceHandle<T>, index: u8) -> Vec<u8> {
    if index == 0 {
        return Vec::new();
    }
    let mut buf = [0u8; 255];
    let value = (0x03u16 * 256) | index as u16;
    match h.read_control(0x80, 0x06, value, 0x0409, &mut buf, TIMEOUT) {
        Ok(n) => buf[..n].to_vec(),
        _ => Vec::new(),
    }
}

/// Decode a string descriptor. Standard descriptors are `[bLength, 0x03, UTF-16LE...]`.
/// CH341F EEPROM-programmed product strings (MeshToad/MeshTadpole) are raw ASCII,
/// NUL-terminated, followed by binary data (16-byte device id).
fn decode_string(raw: &[u8]) -> String {
    if raw.len() >= 2 && raw[1] == 0x03 && raw[0] as usize <= raw.len() {
        let u: Vec<u16> = raw[2..raw[0] as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        return String::from_utf16_lossy(&u).split('\0').next().unwrap_or("").to_string();
    }
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).to_string()
}

/// 16-byte device id following the NUL-terminated ASCII product string, if present and non-blank.
pub fn device_id(product_raw: &[u8]) -> Option<[u8; 16]> {
    if product_raw.len() >= 2 && product_raw[1] == 0x03 {
        return None;
    }
    let end = product_raw.iter().position(|&b| b == 0)?;
    let id: [u8; 16] = product_raw.get(end + 1..end + 17)?.try_into().ok()?;
    (id.iter().any(|&b| b != 0) && id.iter().any(|&b| b != 0xFF)).then_some(id)
}

/// Enumerate attached devices with the given VID/PID (fresh libusb context).
pub fn list(vid: u16, pid: u16) -> Result<Vec<DeviceInfo>> {
    list_in(&Context::new()?, vid, pid)
}

fn list_in<T: UsbContext>(ctx: &T, vid: u16, pid: u16) -> Result<Vec<DeviceInfo>> {
    let mut out = Vec::new();
    for dev in ctx.devices()?.iter() {
        let Ok(desc) = dev.device_descriptor() else { continue };
        if desc.vendor_id() != vid || desc.product_id() != pid {
            continue;
        }
        let mut info = DeviceInfo {
            bus: dev.bus_number(),
            address: dev.address(),
            port_path: dev.port_numbers().unwrap_or_default(),
            vid,
            pid,
            manufacturer: None,
            product: None,
            serial: None,
            product_raw: Vec::new(),
            open_error: None,
        };
        match dev.open() {
            Ok(h) => {
                let m = read_string_raw(&h, desc.manufacturer_string_index().unwrap_or(0));
                let p = read_string_raw(&h, desc.product_string_index().unwrap_or(0));
                let s = read_string_raw(&h, desc.serial_number_string_index().unwrap_or(0));
                info.manufacturer = (!m.is_empty()).then(|| decode_string(&m));
                info.product = (!p.is_empty()).then(|| decode_string(&p));
                info.serial = (!s.is_empty()).then(|| decode_string(&s));
                info.product_raw = p;
            }
            Err(e) => info.open_error = Some(e.to_string()),
        }
        out.push(info);
    }
    Ok(out)
}

/// Open CH341 handle with GPIO shadow state.
pub struct Ch341 {
    h: DeviceHandle<Context>,
    dir: u8,
    state: u8,
    pub info: DeviceInfo,
}

impl Ch341 {
    /// Open the first openable device, or the one with the given USB serial.
    ///
    /// Each call uses a new libusb context: a long-running process must not depend on enumeration
    /// state from before a device was removed (observed on Windows after VMware USB passthrough:
    /// the global context kept failing while a fresh process could open the device).
    pub fn open(serial: Option<&str>) -> Result<Self> {
        let ctx = Context::new()?;
        let infos = list_in(&ctx, VID, PID)?;
        let info = match infos.iter().find(|i| match serial {
            Some(s) => i.serial.as_deref() == Some(s),
            None => i.open_error.is_none(),
        }) {
            Some(i) => i.clone(),
            None => {
                // Present but not openable: report why instead of "not found".
                let errs: Vec<String> =
                    infos.iter().filter_map(|i| i.open_error.as_ref().map(|e| format!("bus {} addr {}: {e}", i.bus, i.address))).collect();
                return Err(if errs.is_empty() {
                    Error::NotFound
                } else {
                    Error::Radio(format!("device present but open failed: {}", errs.join("; ")))
                });
            }
        };
        let dev = ctx.devices()?.iter().find(|d| d.bus_number() == info.bus && d.address() == info.address).ok_or(Error::NotFound)?;
        let h = dev.open()?;
        #[cfg(target_os = "linux")]
        {
            let _ = h.set_auto_detach_kernel_driver(true);
        }
        h.claim_interface(0)?;
        Ok(Ch341 { h, dir: (1 << PIN_SCK) | (1 << PIN_MOSI), state: 0, info })
    }

    fn write(&self, data: &[u8]) -> Result<()> {
        let n = self.h.write_bulk(EP_OUT, data, TIMEOUT)?;
        if n != data.len() {
            return Err(Error::ShortTransfer(format!("out {n}/{}", data.len())));
        }
        Ok(())
    }

    fn read_exact(&self, buf: &mut [u8]) -> Result<()> {
        let mut got = 0;
        while got < buf.len() {
            let mut tmp = [0u8; PACKET_LEN];
            let want = (buf.len() - got).min(PACKET_LEN);
            let n = self.h.read_bulk(EP_IN, &mut tmp[..want], TIMEOUT)?;
            if n == 0 {
                return Err(Error::ShortTransfer(format!("in {got}/{}", buf.len())));
            }
            buf[got..got + n].copy_from_slice(&tmp[..n]);
            got += n;
        }
        Ok(())
    }

    /// Set pin direction (applied on next GPIO write).
    pub fn set_pin_mode(&mut self, pin: u8, output: bool) {
        if output {
            self.dir |= 1 << pin;
        } else {
            self.dir &= !(1 << pin);
        }
    }

    fn uio_out(&self) -> Result<()> {
        self.write(&[CMD_UIO_STREAM, UIO_STM_OUT | (self.state & 0x3F), UIO_STM_DIR | (self.dir & 0x3F), UIO_STM_END])
    }

    /// Drive output pin D0..D5.
    pub fn digital_write(&mut self, pin: u8, high: bool) -> Result<()> {
        if high {
            self.state |= 1 << pin;
        } else {
            self.state &= !(1 << pin);
        }
        self.uio_out()
    }

    /// Read D0..D7 input levels (bit n = Dn).
    pub fn read_inputs(&self) -> Result<u8> {
        self.write(&[CMD_GET_STATUS])?;
        let mut tmp = [0u8; PACKET_LEN];
        let n = self.h.read_bulk(EP_IN, &mut tmp, TIMEOUT)?;
        if n == 0 {
            return Err(Error::ShortTransfer("status".into()));
        }
        Ok(tmp[0])
    }

    /// Full-duplex SPI transfer in place. Caller manages chip select.
    pub fn spi_transfer(&self, buf: &mut [u8]) -> Result<()> {
        for chunk in buf.chunks_mut(DATA_PER_PACKET) {
            let mut pkt = [0u8; PACKET_LEN];
            pkt[0] = CMD_SPI_STREAM;
            for (i, b) in chunk.iter().enumerate() {
                pkt[1 + i] = b.reverse_bits();
            }
            self.write(&pkt[..1 + chunk.len()])?;
            self.read_exact(chunk)?;
            for b in chunk.iter_mut() {
                *b = b.reverse_bits();
            }
        }
        Ok(())
    }

    /// SPI transfer with `cs` held low for its duration.
    pub fn spi_transaction(&mut self, cs: u8, buf: &mut [u8]) -> Result<()> {
        self.set_pin_mode(cs, true);
        self.digital_write(cs, false)?;
        let r = self.spi_transfer(buf);
        let r2 = self.digital_write(cs, true);
        r.and(r2)
    }
}

impl Drop for Ch341 {
    fn drop(&mut self) {
        let _ = self.h.release_interface(0);
    }
}
