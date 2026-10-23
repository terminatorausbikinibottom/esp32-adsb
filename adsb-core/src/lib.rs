//! Application logic on top of `modes`.
//!
//! Geo maths, dead reckoning, closest point of approach, and the tracker (aircraft table with
//! ageing, CPR pairing, known-ICAO filter, vicinity and overhead selection, `Snapshot`), and the radar maths (range, projection, blips, sprites).
//! Still to come: the metadata cache. Time is passed in as `now_ms: u64`.
#![cfg_attr(not(test), no_std)]

pub mod geo;
pub mod kinematics;
pub mod meta;
pub mod pipeline;
pub mod radar;
#[cfg(feature = "scenario")]
pub mod scenario;
pub mod tracker;
