// SPDX-License-Identifier: MIT OR Apache-2.0

#![deny(missing_docs)]
// The README is the crate documentation, so its examples are compiled and run
// by `cargo test --doc` and cannot quietly rot.
#![doc = include_str!("../README.md")]

mod client;
mod device;
mod error;
mod proxy;

pub use client::{CupsPk, MECHANISM_BUS_NAME, PrinterSpec};
pub use device::Device;
pub use error::{CupsPkError, Result};
