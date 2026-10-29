#![no_std]

#[cfg(feature = "bounce")]
pub mod bounce;
pub mod ch422g;
pub mod config;
pub mod display;
#[cfg(not(feature = "bounce"))]
pub mod lookup;
#[cfg(not(feature = "bounce"))]
pub mod net;
pub mod restart;
#[cfg(not(feature = "bounce"))]
pub mod tls;
// The old bounce path has no UI and no network (ui needs net and lookup).
#[cfg(not(feature = "bounce"))]
pub mod ui;
