//! General Mode S / ADS-B library, independent of any runtime or display.
//! `decode()` is the stateless entry point (DF0/4/5/11/16/17/18/20/21); `FrameData` methods give
//! the individual field decoders (CRC, altitude, CPR fields, velocity, squawk).
#![cfg_attr(not(test), no_std)]

mod adsb;
mod altitude;
mod basics;
mod cpr;
mod decode;
mod frame;
mod source;
mod velocity;

pub use adsb::{CategorySet, EmitterCategory, Identification};
pub use cpr::{CprPosition, cpr_global, cpr_local, nl};
pub use decode::{Address, Body, Message, decode};
pub use frame::{ClockKind, FrameData, RawFrame, SignalLevel, Timestamp};
pub use source::{AvrSource, Counters, FrameSource};
pub use velocity::{Horizontal, Velocity};
