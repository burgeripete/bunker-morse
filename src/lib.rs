//! Decodes the DayZ bunker Morse broadcast from game audio. The GPUI app in
//! `main.rs`, the `bunker-morse-cli` debug tool and the web page in `web/` are
//! thin front ends over this crate.

pub mod bunkers;
pub mod decoder;
pub mod detector;
pub mod listener;
pub mod synth;

#[cfg(feature = "native")]
pub mod capture;
#[cfg(feature = "native")]
pub mod engine;
#[cfg(feature = "native")]
pub mod media;
