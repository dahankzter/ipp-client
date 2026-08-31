// SPDX-License-Identifier: MIT OR Apache-2.0

//! An async IPP client, in Rust, with no `libcups`.
//!
//! IPP is the protocol printers and print servers speak. CUPS is one server
//! that speaks it, so this talks to CUPS, to a driverless network printer
//! directly, or to anything else implementing the standard.
//!
//! Most of this crate is standard IPP and works against anything. A few
//! methods on [`IppClient`] are CUPS extensions - listing every queue on a
//! server, its classes, its drivers, its default - because those are questions
//! only a print server can answer. Each says so. Everything on [`IppPrinter`]
//! is standard.
//!
//! # Listing what a daemon knows about
//!
//! ```no_run
//! # async fn example() -> ipp_async::Result<()> {
//! let client = ipp_async::IppClient::local()?;
//!
//! for printer in client.printers().await? {
//!     println!("{}: {:?}", printer.name, printer.state);
//! }
//! # Ok(()) }
//! ```
//!
//! # Working with one printer
//!
//! [`IppClient::queue`] names a queue on the daemon; [`IppClient::at`] takes
//! any printer's URI, with no CUPS in the path. Both give an [`IppPrinter`]
//! carrying the same operations.
//!
//! ```no_run
//! # async fn example() -> ipp_async::Result<()> {
//! let client = ipp_async::IppClient::local()?;
//! let printer = client.at("ipp://printer.local/ipp/print")?;
//!
//! // Ask before uploading, so a rejected format costs nothing.
//! printer.validate("application/pdf").await?;
//! printer.print_file(std::path::Path::new("report.pdf")).await?;
//! # Ok(()) }
//! ```
//!
//! Documents are streamed rather than read into memory, so printing a large
//! file does not hold it all at once.
//!
//! # Reaching a printer over TLS
//!
//! `ipps://` needs the `tls` feature, which is on by default. Printers almost
//! always present self-signed certificates, so verification fails against them
//! unless the certificate is trusted explicitly:
//!
//! ```no_run
//! # fn example() -> ipp_async::Result<()> {
//! let client = ipp_async::IppClient::builder("ipps://printer.local:631")
//!     .ca_cert(std::fs::read("printer.pem")?)
//!     .build()?;
//! # Ok(()) }
//! ```
//!
//! [`Error::is_certificate_error`] identifies the failure when it has not been.
//!
//! # Administration
//!
//! Operations such as pausing a queue need authorisation, and an
//! unauthenticated caller is refused with `401`. Either supply credentials
//! with [`IppClientBuilder::basic_auth`], or - on a desktop, where a password
//! prompt belongs to the system rather than to your process - drive
//! `cups-pk-helper` over D-Bus with the companion `cups-pk-client` crate, and
//! let polkit ask.
//!
//! # C dependencies
//!
//! With `default-features = false` there are none: no `-sys` crate, no `cc`,
//! no `cmake`, nothing to install before building. That build cannot speak
//! `ipps://`. The default build can, and pays for it with rustls' `aws-lc-rs`
//! provider, which compiles a vendored BoringSSL at build time. Nothing is
//! needed at runtime either way.

#![deny(missing_docs)]

pub(crate) mod attrs;
mod client;
mod error;
mod events;
mod lpoptions;
mod model;
mod subscription;

pub use client::{IdentifyAction, IppClient, IppClientBuilder, IppPrinter, PpdFilter, WhichJobs};
pub use error::{Error, Result};
pub use events::PrinterEvent;
pub use lpoptions::{default_printer, default_printer_from};
pub use model::{
    Class, Document, Job, JobId, JobProgress, JobState, MediaSize, OptionValues, Ppd, PrintQuality,
    Printer, PrinterOptions, PrinterState, Severity, StateReason, Supply, SupplyLevel,
    printer_name_from_uri,
};
pub use subscription::{Notification, Notifications, NotifyEvent, Subscription};
