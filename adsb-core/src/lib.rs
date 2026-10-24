//! Application logic on top of `modes`.
//!
//! Geo maths, dead reckoning, closest point of approach, and the tracker (aircraft table with
//! ageing, CPR pairing, known-ICAO filter, vicinity and overhead selection, `Snapshot`), and the radar maths (range, projection, blips, sprites).
//! `wire`: SNTP and HTTP/1.1 reply parsing for the platform network tasks.
//! The metadata model (`meta`: parsers, cache, lookup policy, rate limiter) and, with feature `photo`, the JPEG decoder. Time is passed in as `now_ms: u64`.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod geo;
pub mod kinematics;
pub mod meta;
#[cfg(feature = "photo")]
pub mod photo;
pub mod pipeline;
pub mod radar;
#[cfg(feature = "scenario")]
pub mod scenario;
#[cfg(feature = "tls")]
pub mod tlsauth;
pub mod tracker;
pub mod wire;
pub mod x509;
