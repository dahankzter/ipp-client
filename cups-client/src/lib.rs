// SPDX-License-Identifier: MIT OR Apache-2.0

//! Async CUPS client speaking IPP to the local print daemon.

pub(crate) mod attrs;
mod client;
mod error;
mod events;
mod lpoptions;
mod model;

pub use client::{CupsClient, WhichJobs};
pub use error::{Error, Result};
pub use events::PrinterEvent;
pub use lpoptions::{default_printer, default_printer_from};
pub use model::{
    Job, JobId, JobProgress, JobState, MediaSize, OptionValues, PrintQuality, Printer,
    PrinterOptions, PrinterState, Severity, StateReason, Supply, SupplyLevel,
    printer_name_from_uri,
};
