// SPDX-License-Identifier: GPL-3.0-only

//! Async CUPS client speaking IPP to the local print daemon.

mod attrs;
mod error;
mod model;

pub use error::{Error, Result};
pub use model::{JobId, JobState, PrinterState, Severity, StateReason, Supply, SupplyLevel, Printer};
