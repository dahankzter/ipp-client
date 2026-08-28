// SPDX-License-Identifier: MIT OR Apache-2.0

//! Async Rust client for the [`cups-pk-helper`] D-Bus mechanism.
//!
//! CUPS administration — adding printers, enabling queues, setting the system
//! default — requires authorisation. `cups-pk-helper` performs those operations
//! behind polkit, so the desktop's own authentication agent prompts the user and
//! no password ever reaches this process.
//!
//! [`cups-pk-helper`]: https://www.freedesktop.org/software/cups-pk-helper/releases/

mod client;
mod error;
mod proxy;

pub use client::{CupsPk, MECHANISM_BUS_NAME};
pub use error::{CupsPkError, Result};
