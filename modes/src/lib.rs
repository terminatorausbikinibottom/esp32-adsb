//! General Mode S / ADS-B library, independent of any runtime or display.
//!
//! Will hold: `RawFrame`, the `FrameSource` trait, the `AvrSource` adapter, CRC/parity,
//! stateless `decode()` (DF0/4/5/11/16/17/18/20/21), CPR primitives, and
//! altitude/squawk/Gillham decoding.
#![cfg_attr(not(test), no_std)]

mod adsb;
mod altitude;
mod basics;
mod cpr;
mod frame;
mod source;

pub use adsb::{CategorySet, EmitterCategory, Identification};
pub use cpr::{CprPosition, cpr_global, cpr_local, nl};
pub use frame::{ClockKind, FrameData, RawFrame, SignalLevel, Timestamp};
pub use source::{AvrSource, Counters, FrameSource};
