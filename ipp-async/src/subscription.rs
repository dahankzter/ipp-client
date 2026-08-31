// SPDX-License-Identifier: MIT OR Apache-2.0

//! Asking a printer to report what happens, instead of asking repeatedly.
//!
//! A subscription tells a printer which events to remember for you; a later
//! `Get-Notifications` collects them. This is IPP's alternative to polling, and
//! how much it saves depends entirely on the server: a printer may hold a
//! request open until something happens, while CUPS answers immediately and
//! tells you how long to wait before asking again. [`Notifications::poll_after`]
//! carries that advice when it is given.

use std::time::Duration;

use ipp::prelude::*;

use crate::attrs::Attrs;
use crate::{Error, JobId, Result};

/// An event worth being told about.
///
/// IPP names these as keywords and servers accept more than are listed here,
/// so [`NotifyEvent::Other`] carries anything unrecognised rather than
/// discarding it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotifyEvent {
    /// The printer's state changed.
    PrinterStateChanged,
    /// The printer stopped.
    PrinterStopped,
    /// A job was submitted.
    JobCreated,
    /// A job's state changed.
    JobStateChanged,
    /// A job finished, whether it printed or not.
    JobCompleted,
    /// A job stopped part way.
    JobStopped,
    /// Every event the printer offers.
    All,
    /// A keyword this crate does not name.
    Other(String),
}

impl NotifyEvent {
    /// The IPP keyword for this event.
    pub fn keyword(&self) -> &str {
        match self {
            NotifyEvent::PrinterStateChanged => "printer-state-changed",
            NotifyEvent::PrinterStopped => "printer-stopped",
            NotifyEvent::JobCreated => "job-created",
            NotifyEvent::JobStateChanged => "job-state-changed",
            NotifyEvent::JobCompleted => "job-completed",
            NotifyEvent::JobStopped => "job-stopped",
            NotifyEvent::All => "all",
            NotifyEvent::Other(keyword) => keyword,
        }
    }

    /// Reads an IPP keyword, keeping unrecognised ones intact.
    pub fn from_keyword(keyword: &str) -> Self {
        match keyword {
            "printer-state-changed" => NotifyEvent::PrinterStateChanged,
            "printer-stopped" => NotifyEvent::PrinterStopped,
            "job-created" => NotifyEvent::JobCreated,
            "job-state-changed" => NotifyEvent::JobStateChanged,
            "job-completed" => NotifyEvent::JobCompleted,
            "job-stopped" => NotifyEvent::JobStopped,
            "all" => NotifyEvent::All,
            other => NotifyEvent::Other(other.to_string()),
        }
    }
}

/// A standing request to be told about events.
///
/// Subscriptions expire. A server may shorten a requested lease or refuse to
/// let one last forever, so [`Subscription::lease`] is what was actually
/// granted rather than what was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// The server's identifier for it, needed to collect or cancel.
    pub id: i32,
    /// The events it covers. Servers commonly widen this beyond what was
    /// requested.
    pub events: Vec<NotifyEvent>,
    /// How long it lasts from when it was granted. `None` means no expiry.
    pub lease: Option<Duration>,
    /// Who it belongs to.
    pub subscriber: Option<String>,
}

impl Subscription {
    pub(crate) fn decode(group: &IppAttributeGroup) -> Result<Subscription> {
        let a = Attrs::new(group);
        Ok(Subscription {
            id: a.require_int("notify-subscription-id")?,
            events: a
                .texts("notify-events")
                .iter()
                .map(|k| NotifyEvent::from_keyword(k))
                .collect(),
            // Zero means "no expiry" in IPP, not "already expired".
            lease: a
                .int("notify-lease-duration")
                .filter(|d| *d > 0)
                .map(|d| Duration::from_secs(d as u64)),
            subscriber: a.text("notify-subscriber-user-name"),
        })
    }
}

/// One thing that happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    /// The subscription that captured it.
    pub subscription_id: i32,
    /// What happened.
    pub event: NotifyEvent,
    /// The job it concerns, for job events.
    pub job_id: Option<JobId>,
    /// The server's own description, where it gives one.
    pub text: Option<String>,
    /// Position in the subscription's sequence, for spotting gaps.
    pub sequence: Option<i32>,
}

impl Notification {
    pub(crate) fn decode(group: &IppAttributeGroup) -> Result<Notification> {
        let a = Attrs::new(group);
        Ok(Notification {
            subscription_id: a.require_int("notify-subscription-id")?,
            event: a
                .text("notify-subscribed-event")
                .map(|k| NotifyEvent::from_keyword(&k))
                .unwrap_or_else(|| NotifyEvent::Other(String::new())),
            job_id: a.int("notify-job-id"),
            text: a.text("notify-text"),
            sequence: a.int("notify-sequence-number"),
        })
    }
}

/// What a `Get-Notifications` call returned.
pub struct Notifications {
    /// The events waiting, oldest first.
    pub events: Vec<Notification>,
    /// How long the server suggests waiting before asking again.
    ///
    /// CUPS answers immediately and sets this, which is what makes its
    /// subscriptions a polling mechanism with a server-chosen interval rather
    /// than a push one. A printer that holds the request open may omit it.
    pub poll_after: Option<Duration>,
}

/// Turns a decode failure into a warning and a skip, since one unreadable
/// group is not a reason to lose the rest.
pub(crate) fn decode_all<T>(
    resp: &IppRequestResponse,
    tag: DelimiterTag,
    decode: impl Fn(&IppAttributeGroup) -> Result<T>,
    what: &str,
) -> Vec<T> {
    resp.attributes()
        .groups_of(tag)
        .filter_map(|group| match decode(group) {
            Ok(value) => Some(value),
            Err(e) => {
                tracing::debug!("skipping undecodable {what}: {e}");
                None
            }
        })
        .collect()
}

/// `Error` helper kept here so the operations read as one piece.
pub(crate) fn missing(what: &'static str) -> Error {
    Error::decode(what, "not present in the response")
}
