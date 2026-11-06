# meshtadpole-radio

Rust library: host-side driver for the MeshTadpole SX1262 USB stick. Shared by [meshtadpole-meshtastic-cli](https://github.com/petrisi/meshtadpole-meshtastic-cli) and [meshtadpole-meshcore-cli](https://github.com/petrisi/meshtadpole-meshcore-cli).

## Hardware

<img width="1700" height="492" alt="image" src="https://github.com/user-attachments/assets/5569561f-7569-4061-95a8-a9be983c979b" />

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
cargo run --example rx -- 869618000 62500 8 8 12 32 [s]    # raw RX (MeshCore EU/UK Narrow)
cargo run --example diag -- <same args> <s> [agc_s]        # IRQ/RSSI/mode trace, SPI-polled; reset_agc every agc_s
cargo run --example linktest -- 869618000 10 120 [agc_s]   # link loop, printing handler: <freq> <stats_s> [run_s] [agc_reset_interval_s]
```

Sync word byte `sw` maps to register `0x0740` as `[(sw&0xF0)|4, ((sw&0x0F)<<4)|4]`.

Schematic: https://github.com/LongMesh/MeshHardware (MeshTadpoleStick).

## Scope

- CH341: device enumeration, SPI transfers, GPIO (NSS, RESET, BUSY, DIO1, RF switch).
- SX1262: init, LoRa modem parameters, TX, continuous RX, IRQ handling, RSSI/SNR/frequency error, CAD.
- `link`: protocol-independent radio session loop used by the tool daemons. Open with reconnect (backoff 1-30 s, starts without a stick), continuous RX, TX queue with CSMA (busy = preamble/header detected; random 200-800 ms deferral), rolling-hour duty-cycle limit, idle noise sampling ~1/s (readings <= -125 dBm, i.e. the chip's -127.5 'no measurement' value right after entering RX, are discarded), periodic stats. Protocol logic is a `link::Handler`. `LinkConfig::liveness` is ticked every loop iteration; `link::spawn_watchdog` reports a loop blocked longer than a limit (daemons log and exit, for a service manager to restart). Receiver recovery, all reported as `rx_restored` events with the trigger as detail prefix:
  - `agc_stuck`: SX1262 out-of-band lockup (RadioLib errata: after a strong adjacent-channel signal the receiver stays deaf, idle RSSI flat and up to ~35 dB high, chip still in RX mode). Detected when 8 idle RSSI samples (~8 s) are within 3 dB and >= 12 dB above the session floor, or above -88 dBm while the floor never went below -88 dBm (receiver stuck since open). Only CRC-good frames reset the window; header and CRC errors do not count as proof of a working receiver. Recovery: `Sx1262::reset_agc` (light, ~25 ms). Repeated triggers within 2 min back off 30 s-10 min (steady real interferer).
    Measured 2026-11 (MeshCore, 869.618 MHz, boosted gain off; 27.7 h with recovery disabled, branch `agc-observe`): flat at -77 dBm, 7.2 episodes/h (mostly at night), stuck 31.5% of the time, each clearing by itself after median 103 s (max 586 s) or within 1-2 s of our own TX. While stuck no frame weaker than -70 dBm was received (outside: down to -96 dBm), i.e. ~25 dB sensitivity loss. With recovery on, stuck time is ~8-10 s per detection. Meshtastic at 869.431 MHz (boosted on): 2 detections in 11.7 h.
  - `agc_periodic`: optional preventive reset every `LinkConfig::agc_reset_interval` (default off in both tools; config key `radio.agc_reset_interval_s`); skipped if a CRC-good frame arrived within the interval or the channel is busy.
  - `mode_watchdog`: chip not in RX mode when idle (checked every stats interval): full `configure`.
  - `fallback=configure` in detail: light reset failed or chip not in RX afterwards; full reconfigure done.
- `Sx1262::reset_agc`: warm AGC/calibration reset as MeshCore `sx126xResetAGC`: RX off, warm sleep, wake to STDBY_RC, Calibrate(0x7F), image calibration for the operating frequency, re-apply all modem/RF settings (DIO2 RF switch, RX gain, sync word, IQ fix, OCP, ...), continuous RX. No NRESET. `configure` (NRESET + full init) is for open and hard failures.
- `api`: daemon API transport (TCP, NDJSON lines, subscriber broadcast).
- `store` (feature `store`): SQLite helpers and common tables `meta`, `sessions`, `radio_stats`, `radio_events`, `requests`; `query_json`.

## Platform setup

- Windows: bind WinUSB to `1a86:5512` (Zadig). Vendor CH341PAR driver is not used.
- Linux: in-tree kernel driver `spi_ch341` (kernel ≥ 6.x, verified 7.0 / Ubuntu 26.04) binds `1a86:5512` on plug-in. The library sets libusb auto-detach: the driver is detached on open and re-attached on release; no blacklist needed. Running as root works as is. Non-root: udev rule granting access, e.g. `/etc/udev/rules.d/99-meshtadpole.rules`:
  `SUBSYSTEM=="usb", ATTRS{idVendor}=="1a86", ATTRS{idProduct}=="5512", MODE="0660", GROUP="plugdev"` (then `udevadm control --reload && udevadm trigger`). Optionally blacklist `spi_ch341` (`/etc/modprobe.d/meshtadpole.conf`: `blacklist spi_ch341`) if other software must not see the device as SPI master.

## Status

- CH341 enumeration, descriptor decode, SPI, GPIO: working (Windows/WinUSB).
- SX1262 configure, continuous RX, IRQ via DIO1, packet status, frequency error: working.
- TX with CSMA (caller) and TX_DONE wait: working.
- Linux (Ubuntu 26.04.1, kernel 7.0, VMware USB passthrough): probe, RX, TX verified.

## License

MIT
