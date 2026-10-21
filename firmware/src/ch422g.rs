//! Minimal CH422G IO-expander driver. Each register has its own 7-bit I2C "address" and a write is
//! a single data byte, so there is no register-pointer byte.

use esp_hal::Async;
use esp_hal::i2c::master::{Error, I2c};

const WR_SET: u8 = 0x24;
const WR_IO: u8 = 0x38;
/// WR_SET bit 0: IO0..IO7 are outputs.
const IO_OE: u8 = 0x01;

// Waveshare ESP32-S3-Touch-LCD-7 output map: EXIOn is bit n of WR_IO.
pub const TP_RST: u8 = 1;
pub const BACKLIGHT: u8 = 2;
pub const LCD_RST: u8 = 3;
pub const SD_CS: u8 = 4;
/// Low = USB, high = CAN. Must stay low or USB-Serial-JTAG goes away.
pub const USB_CAN_SEL: u8 = 5;

pub struct Ch422g<'d> {
    i2c: I2c<'d, Async>,
    outputs: u8,
}

impl<'d> Ch422g<'d> {
    pub fn new(i2c: I2c<'d, Async>) -> Self {
        Self { i2c, outputs: 0 }
    }

    pub async fn set_mode_output(&mut self) -> Result<(), Error> {
        self.i2c.write_async(WR_SET, &[IO_OE]).await
    }

    /// Writes all outputs and updates the shadow only once the write succeeded.
    pub async fn write_outputs(&mut self, value: u8) -> Result<(), Error> {
        self.i2c.write_async(WR_IO, &[value]).await?;
        self.outputs = value;
        Ok(())
    }

    pub async fn set_pin(&mut self, pin: u8, level: bool) -> Result<(), Error> {
        debug_assert!(pin < 8);
        let v = if level {
            self.outputs | 1 << pin
        } else {
            self.outputs & !(1 << pin)
        };
        self.write_outputs(v).await
    }
}
