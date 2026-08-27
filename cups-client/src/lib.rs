// SPDX-License-Identifier: GPL-3.0-only

//! Async CUPS client speaking IPP to the local print daemon.

pub(crate) mod attrs;
mod client;
mod error;
mod events;
mod lpoptions;
mod model;

pub use client::{CupsClient, WhichJobs};
pub use events::PrinterEvent;
pub use error::{Error, Result};
pub use lpoptions::{default_printer, default_printer_from};
pub use model::{JobId, JobState, PrinterState, Severity, StateReason, Supply, SupplyLevel, Printer, Job, JobProgress, printer_name_from_uri};
