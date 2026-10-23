#![no_std]

#[cfg(feature = "bounce")]
pub mod bounce;
pub mod ch422g;
pub mod config;
pub mod display;
#[cfg(not(feature = "bounce"))]
pub mod net;
pub mod restart;
pub mod ui;
