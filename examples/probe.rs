//! Enumerate CH341 devices, open the first, reset SX1262, dump identification.
use meshtadpole_radio::{ch341, sx1262};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    for d in ch341::list(ch341::VID, ch341::PID)? {
        println!(
            "bus={} addr={} ports={:?} manufacturer={:?} product={:?} serial={:?} open_error={:?}",
            d.bus, d.address, d.port_path, d.manufacturer, d.product, d.serial, d.open_error
        );
        let hex: String = d.product_raw.iter().map(|b| format!("{b:02x}")).collect();
        println!("product_raw_hex={hex}");
        println!("device_id={:02x?}", ch341::device_id(&d.product_raw));
    }
    let bus = ch341::Ch341::open(None)?;
    let mut r = sx1262::Sx1262::new(bus, sx1262::MESHTADPOLE_PINS)?;
    println!("inputs_before_reset={:08b}", r.inputs()?);
    r.reset()?;
    println!("inputs_after_reset={:08b}", r.inputs()?);
    let st = r.get_status()?;
    println!("status=0x{st:02x} mode/cmd={:?}", sx1262::status_fields(st));
    println!("version={:?}", r.version_string()?);
    println!("sync_word={:02x?}", r.read_registers(sx1262::reg::LORA_SYNC_WORD, 2)?);
    println!("ocp={:02x?}", r.read_registers(sx1262::reg::OCP, 1)?);
    println!("device_errors=0x{:04x}", r.get_device_errors()?);
    Ok(())
}
