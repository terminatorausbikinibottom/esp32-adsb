//! Shared UI/app layer for firmware and simulator (ADR-002): the `.slint` files, the `App`
//! view-model with diffed property updates, pointer mapping and the line-render helper.
//!
//! Anything that decides what the UI shows lives here; platform crates only supply time, pixels
//! sinks and pointer events. Slint is configured single-threaded and `no_std`: use `App` from one
//! thread only, and create one `App` per process (the Slint platform can be set once).
#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod app;
mod render;

pub use app::{App, CardKind, PointerEvent, PointerPhase, Status, Ui};
pub use render::{HEIGHT, WIDTH, render_by_line};
pub use slint::platform::software_renderer::Rgb565Pixel;

mod ui {
    slint::include_modules!();
}
pub use ui::Badge;
