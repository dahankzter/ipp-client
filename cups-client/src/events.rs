// SPDX-License-Identifier: GPL-3.0-only

use crate::{Job, JobId, Printer};

/// A change in the print system, emitted identically by the push and poll paths.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrinterEvent {
    JobAdded(Job),
    JobChanged(Job),
    JobRemoved(JobId),
    PrinterChanged(Printer),
    /// A queue CUPS no longer knows about, by name. Without this a deleted
    /// queue would sit in the panel forever, permanently idle.
    PrinterRemoved(String),
    /// A full state reload. Emitted on connect and after any recovery, so the
    /// consumer always has a way back to a known-good state.
    Resynchronised { printers: Vec<Printer>, jobs: Vec<Job> },
}

/// A point-in-time view of the print system.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snapshot {
    pub printers: Vec<Printer>,
    pub jobs: Vec<Job>,
}

impl Snapshot {
    /// Events that turn `self` into `next`. Removals come first so a consumer
    /// replacing a job id sees the old one leave before the new one arrives.
    pub(crate) fn diff(&self, next: &Snapshot) -> Vec<PrinterEvent> {
        let mut events = Vec::new();

        for job in &self.jobs {
            if !next.jobs.iter().any(|j| j.id == job.id) {
                events.push(PrinterEvent::JobRemoved(job.id));
            }
        }

        for job in &next.jobs {
            match self.jobs.iter().find(|j| j.id == job.id) {
                None => events.push(PrinterEvent::JobAdded(job.clone())),
                Some(previous) if previous != job => {
                    events.push(PrinterEvent::JobChanged(job.clone()))
                }
                Some(_) => {}
            }
        }

        for printer in &self.printers {
            if !next.printers.iter().any(|p| p.name == printer.name) {
                events.push(PrinterEvent::PrinterRemoved(printer.name.clone()));
            }
        }

        for printer in &next.printers {
            match self.printers.iter().find(|p| p.name == printer.name) {
                Some(previous) if previous == printer => {}
                _ => events.push(PrinterEvent::PrinterChanged(printer.clone())),
            }
        }

        events
    }
}

use std::time::Duration;

/// How often the poll path re-reads the queue.
pub const POLL_INTERVAL: Duration = Duration::from_secs(3);

const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Doubles the retry delay, capped at 30 s.
pub(crate) fn backoff_after(previous: Duration) -> Duration {
    if previous.is_zero() {
        Duration::from_secs(1)
    } else {
        (previous * 2).min(MAX_BACKOFF)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{JobProgress, JobState, PrinterState};

    fn job(id: i32, state: JobState) -> Job {
        Job {
            id,
            printer: "HP-8210".into(),
            name: Some("doc.pdf".into()),
            user: Some("tester".into()),
            state,
            reasons: Vec::new(),
            progress: JobProgress::Indeterminate,
            created: None,
        }
    }

    fn printer(state: PrinterState) -> Printer {
        Printer {
            name: "HP-8210".into(),
            uri: "ipp://localhost/printers/HP-8210".into(),
            info: None,
            location: None,
            state,
            reasons: Vec::new(),
            accepting_jobs: true,
            supplies: Vec::new(),
            is_default: true,
        }
    }

    fn snapshot(printers: Vec<Printer>, jobs: Vec<Job>) -> Snapshot {
        Snapshot { printers, jobs }
    }

    #[test]
    fn identical_snapshots_produce_no_events() {
        let s = snapshot(vec![printer(PrinterState::Idle)], vec![job(1, JobState::Pending)]);
        assert!(s.diff(&s).is_empty());
    }

    #[test]
    fn a_new_job_is_reported_as_added() {
        let before = snapshot(vec![], vec![]);
        let after = snapshot(vec![], vec![job(1, JobState::Pending)]);
        let events = before.diff(&after);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], PrinterEvent::JobAdded(j) if j.id == 1));
    }

    #[test]
    fn a_changed_job_is_reported_as_changed() {
        let before = snapshot(vec![], vec![job(1, JobState::Pending)]);
        let after = snapshot(vec![], vec![job(1, JobState::Processing)]);
        let events = before.diff(&after);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], PrinterEvent::JobChanged(j) if j.state == JobState::Processing));
    }

    #[test]
    fn a_vanished_job_is_reported_as_removed() {
        let before = snapshot(vec![], vec![job(1, JobState::Pending)]);
        let after = snapshot(vec![], vec![]);
        assert_eq!(before.diff(&after), vec![PrinterEvent::JobRemoved(1)]);
    }

    #[test]
    fn a_changed_printer_is_reported_once() {
        let before = snapshot(vec![printer(PrinterState::Idle)], vec![]);
        let after = snapshot(vec![printer(PrinterState::Processing)], vec![]);
        let events = before.diff(&after);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], PrinterEvent::PrinterChanged(p) if p.state == PrinterState::Processing));
    }

    #[test]
    fn a_new_printer_is_reported_as_changed() {
        let before = snapshot(vec![], vec![]);
        let after = snapshot(vec![printer(PrinterState::Idle)], vec![]);
        assert_eq!(before.diff(&after).len(), 1);
    }

    #[test]
    fn a_vanished_printer_is_reported_as_removed() {
        let before = snapshot(vec![printer(PrinterState::Idle)], vec![]);
        let after = snapshot(vec![], vec![]);
        assert_eq!(
            before.diff(&after),
            vec![PrinterEvent::PrinterRemoved("HP-8210".into())]
        );
    }

    #[test]
    fn a_replaced_printer_reports_removal_before_addition() {
        let mut other = printer(PrinterState::Idle);
        other.name = "Other".into();

        let before = snapshot(vec![printer(PrinterState::Idle)], vec![]);
        let after = snapshot(vec![other], vec![]);
        let events = before.diff(&after);

        assert_eq!(events.len(), 2);
        assert!(matches!(&events[0], PrinterEvent::PrinterRemoved(n) if n == "HP-8210"));
        assert!(matches!(&events[1], PrinterEvent::PrinterChanged(p) if p.name == "Other"));
    }

    #[test]
    fn removals_are_emitted_before_additions() {
        let before = snapshot(vec![], vec![job(1, JobState::Pending)]);
        let after = snapshot(vec![], vec![job(2, JobState::Pending)]);
        let events = before.diff(&after);
        assert_eq!(events.len(), 2);
        assert!(matches!(events[0], PrinterEvent::JobRemoved(1)));
        assert!(matches!(&events[1], PrinterEvent::JobAdded(j) if j.id == 2));
    }

    #[test]
    fn backoff_grows_then_caps() {
        assert_eq!(backoff_after(Duration::ZERO), Duration::from_secs(1));
        assert_eq!(backoff_after(Duration::from_secs(1)), Duration::from_secs(2));
        assert_eq!(backoff_after(Duration::from_secs(16)), Duration::from_secs(30));
        assert_eq!(backoff_after(Duration::from_secs(30)), Duration::from_secs(30));
    }

    #[test]
    fn poll_interval_is_three_seconds() {
        assert_eq!(POLL_INTERVAL, Duration::from_secs(3));
    }
}
