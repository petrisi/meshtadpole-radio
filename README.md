# meshtadpole-radio

Rust library: host-side driver for the MeshTadpole SX1262 USB stick. Shared by `meshtadpole-meshtastic-cli` and `meshtadpole-meshcore-cli`.

## Hardware

| Item | Value |
|---|---|
| Transceiver | Semtech SX1262 (Elecrow LR1262 module), 850–930 MHz, +22 dBm max |
| Host interface | WCH CH341 USB-SPI/GPIO bridge, USB `1a86:5512` (to be verified on device) |
| EEPROM | Onboard; auto-configuration data used by meshtasticd ≥ 2.6.5 |
| Firmware | None. All protocol logic runs on the host. |

Schematic: https://github.com/LongMesh/MeshHardware (MeshTadpoleStick).

## Scope

- CH341: device enumeration, SPI transfers, GPIO (NSS, RESET, BUSY, DIO1, RF switch).
- SX1262: init, LoRa modem parameters, TX, continuous RX, IRQ handling, RSSI/SNR/frequency error, CAD.

## Platform setup

- Windows: bind WinUSB to `1a86:5512` (Zadig). Vendor CH341PAR driver is not used.
- Linux: libusb access via udev rule; `ch341`/`spi-ch341-usb` kernel modules must not claim the device.

## Status

Skeleton.

## License

MIT
