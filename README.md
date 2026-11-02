# meshtadpole-radio

Rust library: host-side driver for the MeshTadpole SX1262 USB stick. Shared by `meshtadpole-meshtastic-cli` and `meshtadpole-meshcore-cli`.

## Hardware

| Item | Value |
|---|---|
| Transceiver | Semtech SX1262 (Elecrow LR1262 module), 850–930 MHz, +22 dBm max |
| Host interface | WCH CH341F USB-SPI/GPIO bridge, USB `1a86:5512`, full speed |
| USB product string | Raw ASCII `MESHTOAD\0` + 16-byte device id (CH341F EEPROM, non-standard descriptor). meshtasticd maps `MESHTOAD` → `lora-usb-meshtoad-e22.yaml` |
| USB serial | 8 ASCII digits, e.g. `20002049` |
| TCXO | 1.8 V via SX1262 DIO3 |
| RF switch | SX1262 DIO2 + RXEN on CH341 D1 |
| Firmware | None. All protocol logic runs on the host. |

## CH341 pin map

| Pin | Dir | Function |
|---|---|---|
| D0 | out | SX1262 NSS |
| D1 | out | RXEN (high in RX) |
| D2 | out | SX1262 NRESET |
| D3 | out | SCK |
| D4 | in | SX1262 BUSY |
| D5 | out | MOSI |
| D6 | in | SX1262 DIO1 (IRQ) |
| D7 | in | MISO |

## CH341 wire protocol

Interface 0, bulk OUT `0x02`, bulk IN `0x82`.

| Op | OUT | IN |
|---|---|---|
| SPI | `A8` + ≤31 bytes, each bit-reversed | same count, bit-reversed |
| GPIO write | `AB, 80\|state[5:0], 40\|dir[5:0], 20` | — |
| Input read | `A0` | byte 0 = D7..D0 |

## Examples

```
cargo run --example probe                                  # enumerate, reset, identify
cargo run --example rx -- 869431250 62500 8 8 2b 16 [s]    # raw RX, NDJSON (Meshtastic EFL)
cargo run --example rx -- 869618000 62500 8 8 12 16 [s]    # raw RX (MeshCore EU/UK Narrow)
cargo run --example diag -- <same args> <s>                # IRQ/RSSI/mode trace, SPI-polled
```

Sync word byte `sw` maps to register `0x0740` as `[(sw&0xF0)|4, ((sw&0x0F)<<4)|4]`.

Schematic: https://github.com/LongMesh/MeshHardware (MeshTadpoleStick).

## Scope

- CH341: device enumeration, SPI transfers, GPIO (NSS, RESET, BUSY, DIO1, RF switch).
- SX1262: init, LoRa modem parameters, TX, continuous RX, IRQ handling, RSSI/SNR/frequency error, CAD.

## Platform setup

- Windows: bind WinUSB to `1a86:5512` (Zadig). Vendor CH341PAR driver is not used.
- Linux: libusb access via udev rule; `ch341`/`spi-ch341-usb` kernel modules must not claim the device.

## Status

- CH341 enumeration, descriptor decode, SPI, GPIO: working (Windows/WinUSB).
- SX1262 configure, continuous RX, IRQ via DIO1, packet status, frequency error: working.
- TX: implemented, untested.
- Linux: untested.

## License

MIT
