//! CH341 USB-SPI/GPIO bridge and SX1262 LoRa transceiver driver for the MeshTadpole USB stick.

pub mod ch341;
pub mod sx1262;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("device not found")]
    NotFound,
    #[error("usb: {0}")]
    Usb(#[from] rusb::Error),
    #[error("usb short transfer: {0}")]
    ShortTransfer(String),
    #[error("radio: {0}")]
    Radio(String),
    #[error("timeout: {0}")]
    Timeout(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
