// SPDX-License-Identifier: GPL-3.0-only

use crate::{Error, Result};

pub type JobId = i32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterState {
    Idle,
    Processing,
    Stopped,
}

impl PrinterState {
    pub fn from_ipp(value: i32) -> Result<Self> {
        match value {
            3 => Ok(PrinterState::Idle),
            4 => Ok(PrinterState::Processing),
            5 => Ok(PrinterState::Stopped),
            other => Err(Error::decode("printer-state", format!("unknown value {other}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Pending,
    PendingHeld,
    Processing,
    ProcessingStopped,
    Canceled,
    Aborted,
    Completed,
}

impl JobState {
    pub fn from_ipp(value: i32) -> Result<Self> {
        match value {
            3 => Ok(JobState::Pending),
            4 => Ok(JobState::PendingHeld),
            5 => Ok(JobState::Processing),
            6 => Ok(JobState::ProcessingStopped),
            7 => Ok(JobState::Canceled),
            8 => Ok(JobState::Aborted),
            9 => Ok(JobState::Completed),
            other => Err(Error::decode("job-state", format!("unknown value {other}"))),
        }
    }

    /// True while the job is still in the queue.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            JobState::Pending
                | JobState::PendingHeld
                | JobState::Processing
                | JobState::ProcessingStopped
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Report,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateReason {
    pub keyword: String,
    pub severity: Severity,
}

impl StateReason {
    pub fn parse(raw: &str) -> Self {
        for (suffix, severity) in [
            ("-error", Severity::Error),
            ("-warning", Severity::Warning),
            ("-report", Severity::Report),
        ] {
            if let Some(keyword) = raw.strip_suffix(suffix) {
                return StateReason { keyword: keyword.to_string(), severity };
            }
        }
        StateReason { keyword: raw.to_string(), severity: Severity::Report }
    }

    /// Parses a `*-state-reasons` list. The `none` keyword means "no reasons".
    pub fn parse_list(raw: &[String]) -> Vec<StateReason> {
        raw.iter()
            .filter(|r| r.as_str() != "none")
            .map(|r| StateReason::parse(r))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_state_maps_known_values() {
        assert_eq!(PrinterState::from_ipp(3).unwrap(), PrinterState::Idle);
        assert_eq!(PrinterState::from_ipp(4).unwrap(), PrinterState::Processing);
        assert_eq!(PrinterState::from_ipp(5).unwrap(), PrinterState::Stopped);
    }

    #[test]
    fn printer_state_rejects_unknown_rather_than_defaulting() {
        assert!(PrinterState::from_ipp(99).is_err());
    }

    #[test]
    fn job_state_maps_known_values() {
        assert_eq!(JobState::from_ipp(3).unwrap(), JobState::Pending);
        assert_eq!(JobState::from_ipp(4).unwrap(), JobState::PendingHeld);
        assert_eq!(JobState::from_ipp(9).unwrap(), JobState::Completed);
    }

    #[test]
    fn active_jobs_are_the_unfinished_ones() {
        assert!(JobState::Processing.is_active());
        assert!(JobState::Pending.is_active());
        assert!(JobState::PendingHeld.is_active());
        assert!(!JobState::Completed.is_active());
        assert!(!JobState::Canceled.is_active());
        assert!(!JobState::Aborted.is_active());
    }

    #[test]
    fn reason_severity_comes_from_the_suffix() {
        let r = StateReason::parse("media-jam-error");
        assert_eq!(r.keyword, "media-jam");
        assert_eq!(r.severity, Severity::Error);

        let r = StateReason::parse("toner-low-warning");
        assert_eq!(r.keyword, "toner-low");
        assert_eq!(r.severity, Severity::Warning);

        let r = StateReason::parse("cover-open-report");
        assert_eq!(r.keyword, "cover-open");
        assert_eq!(r.severity, Severity::Report);
    }

    #[test]
    fn bare_reason_defaults_to_report() {
        let r = StateReason::parse("connecting-to-device");
        assert_eq!(r.keyword, "connecting-to-device");
        assert_eq!(r.severity, Severity::Report);
    }

    #[test]
    fn none_keyword_yields_no_reasons() {
        let list = vec!["none".to_string()];
        assert!(StateReason::parse_list(&list).is_empty());
    }
}
