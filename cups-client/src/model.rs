// SPDX-License-Identifier: GPL-3.0-only

use crate::{Error, Result};
use tracing::warn;

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
            other => Err(Error::decode(
                "printer-state",
                format!("unknown value {other}"),
            )),
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
                return StateReason {
                    keyword: keyword.to_string(),
                    severity,
                };
            }
        }
        StateReason {
            keyword: raw.to_string(),
            severity: Severity::Report,
        }
    }

    /// Parses a `*-state-reasons` list. The `none` keyword means "no reasons".
    pub fn parse_list(raw: &[String]) -> Vec<StateReason> {
        raw.iter()
            .filter(|r| r.as_str() != "none")
            .map(|r| StateReason::parse(r))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplyLevel {
    /// A percentage in 0..=100.
    Percent(u8),
    /// CUPS reports a negative level when it does not know.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supply {
    pub name: String,
    pub kind: Option<String>,
    pub colour: Option<String>,
    pub level: SupplyLevel,
    pub low_threshold: Option<i32>,
}

impl Supply {
    /// Pairs `names` with `levels` up to the shorter list's length.
    ///
    /// A length mismatch means cupsd sent a malformed marker attribute for
    /// this one queue. Spec ¤9 promises that one bad attribute never blanks
    /// the popup, so this logs and degrades to the overlapping prefix rather
    /// than failing the whole printer.
    pub fn decode_list(
        names: &[String],
        levels: &[i32],
        types: &[String],
        colours: &[String],
        low: &[i32],
    ) -> Vec<Supply> {
        if names.len() != levels.len() {
            warn!(
                "marker-levels: {} levels for {} names; truncating to the shorter list",
                levels.len(),
                names.len()
            );
        }

        names
            .iter()
            .zip(levels.iter())
            .enumerate()
            .map(|(i, (name, level))| Supply {
                name: name.clone(),
                kind: types.get(i).cloned(),
                colour: colours.get(i).cloned(),
                level: match *level {
                    n if n < 0 => SupplyLevel::Unknown,
                    n => SupplyLevel::Percent(n.min(100) as u8),
                },
                low_threshold: low.get(i).copied(),
            })
            .collect()
    }
}

use crate::attrs::Attrs;
use ipp::prelude::IppAttributeGroup;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    pub uri: String,
    /// The backend URI CUPS prints through, e.g.
    /// `ipps://HP%20OfficeJet._ipps._tcp.local/`. Distinct from `uri`, which is
    /// the queue's own address on this machine. Empty when CUPS does not say.
    pub device_uri: String,
    pub info: Option<String>,
    pub location: Option<String>,
    pub state: PrinterState,
    pub reasons: Vec<StateReason>,
    pub accepting_jobs: bool,
    pub supplies: Vec<Supply>,
    pub is_default: bool,
}

impl Printer {
    pub fn decode(group: &IppAttributeGroup) -> Result<Printer> {
        let a = Attrs::new(group);

        Ok(Printer {
            name: a.require_text("printer-name")?,
            // `ipp` returns an `Array` (not a scalar) when cupsd advertises more
            // than one URI (e.g. both ipp and ipps), and `Attrs::text` reads
            // only scalars. Take the first of the possibly-many values instead.
            device_uri: a.text("device-uri").unwrap_or_default(),
            uri: a
                .texts("printer-uri-supported")
                .into_iter()
                .next()
                .unwrap_or_default(),
            info: a.text("printer-info"),
            location: a.text("printer-location"),
            state: PrinterState::from_ipp(a.require_int("printer-state")?)?,
            reasons: StateReason::parse_list(&a.texts("printer-state-reasons")),
            accepting_jobs: a.bool("printer-is-accepting-jobs").unwrap_or(true),
            supplies: Supply::decode_list(
                &a.texts("marker-names"),
                &a.ints("marker-levels"),
                &a.texts("marker-types"),
                &a.texts("marker-colors"),
                &a.ints("marker-low-levels"),
            ),
            is_default: false,
        })
    }

    /// The worst severity among the current state reasons, if any.
    pub fn highest_severity(&self) -> Option<Severity> {
        self.reasons.iter().map(|r| r.severity).max()
    }
}

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobProgress {
    /// CUPS did not report a page count. Render an indeterminate bar.
    Indeterminate,
    Pages {
        done: i32,
        total: i32,
    },
}

impl JobProgress {
    /// Completion in 0.0..=1.0, or `None` when indeterminate.
    pub fn fraction(&self) -> Option<f32> {
        match self {
            JobProgress::Indeterminate => None,
            JobProgress::Pages { done, total } if *total > 0 => {
                Some((*done as f32 / *total as f32).clamp(0.0, 1.0))
            }
            JobProgress::Pages { .. } => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub id: JobId,
    pub printer: String,
    pub name: Option<String>,
    pub user: Option<String>,
    pub state: JobState,
    pub reasons: Vec<StateReason>,
    pub progress: JobProgress,
    pub created: Option<SystemTime>,
}

/// Extracts the CUPS queue name from a printer URI.
pub fn printer_name_from_uri(uri: &str) -> Option<String> {
    uri.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Job {
    pub fn decode(group: &IppAttributeGroup) -> Result<Job> {
        let a = Attrs::new(group);

        let progress = match (a.int("job-impressions"), a.int("job-impressions-completed")) {
            (Some(total), Some(done)) if total > 0 => JobProgress::Pages { done, total },
            _ => JobProgress::Indeterminate,
        };

        Ok(Job {
            id: a.require_int("job-id")?,
            printer: a
                .text("job-printer-uri")
                .and_then(|uri| printer_name_from_uri(&uri))
                .unwrap_or_default(),
            name: a.text("job-name"),
            user: a.text("job-originating-user-name"),
            state: JobState::from_ipp(a.require_int("job-state")?)?,
            reasons: StateReason::parse_list(&a.texts("job-state-reasons")),
            progress,
            created: a
                .int("time-at-creation")
                .filter(|t| *t > 0)
                .map(|t| UNIX_EPOCH + Duration::from_secs(t as u64)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipp::prelude::*;
    // Re-import our types to shadow the conflicting ipp ones
    use super::{JobState, PrinterState, Severity, StateReason, Supply, SupplyLevel};

    fn printer_group(extra: Vec<(&str, IppValue)>) -> IppAttributeGroup {
        let mut g = IppAttributeGroup::new(DelimiterTag::PrinterAttributes);
        let mut base = vec![
            (
                "printer-name",
                IppValue::NameWithoutLanguage("HP-8210".try_into().unwrap()),
            ),
            (
                "printer-uri-supported",
                IppValue::Uri("ipp://localhost/printers/HP-8210".try_into().unwrap()),
            ),
            ("printer-state", IppValue::Enum(3)),
            ("printer-is-accepting-jobs", IppValue::Boolean(true)),
        ];
        base.extend(extra);
        for (name, value) in base {
            g.attributes_mut()
                .push(IppAttribute::with_name(name, value).unwrap());
        }
        g
    }

    #[test]
    fn decodes_a_minimal_printer() {
        let p = Printer::decode(&printer_group(vec![])).unwrap();
        assert_eq!(p.name, "HP-8210");
        assert_eq!(p.state, PrinterState::Idle);
        assert!(p.accepting_jobs);
        assert!(p.reasons.is_empty());
        assert!(p.supplies.is_empty());
        assert_eq!(p.info, None);
        assert!(!p.is_default);
    }

    #[test]
    fn decodes_reasons_and_supplies() {
        let p = Printer::decode(&printer_group(vec![
            (
                "printer-state-reasons",
                IppValue::Array(vec![
                    IppValue::Keyword("toner-low-warning".try_into().unwrap()),
                    IppValue::Keyword("media-jam-error".try_into().unwrap()),
                ]),
            ),
            (
                "marker-names",
                IppValue::NameWithoutLanguage("Black Ink".try_into().unwrap()),
            ),
            ("marker-levels", IppValue::Integer(42)),
            (
                "printer-info",
                IppValue::TextWithoutLanguage("Office printer".try_into().unwrap()),
            ),
        ]))
        .unwrap();

        assert_eq!(p.reasons.len(), 2);
        assert_eq!(p.reasons[1].severity, Severity::Error);
        assert_eq!(p.supplies.len(), 1);
        assert_eq!(p.supplies[0].level, SupplyLevel::Percent(42));
        assert_eq!(p.info.as_deref(), Some("Office printer"));
    }

    #[test]
    fn missing_printer_name_is_a_decode_error() {
        let mut g = IppAttributeGroup::new(DelimiterTag::PrinterAttributes);
        g.attributes_mut()
            .push(IppAttribute::with_name("printer-state", IppValue::Enum(3)).unwrap());
        let err = Printer::decode(&g).unwrap_err();
        assert!(err.to_string().contains("printer-name"));
    }

    #[test]
    fn highest_severity_reports_the_worst_reason() {
        let p = Printer::decode(&printer_group(vec![(
            "printer-state-reasons",
            IppValue::Array(vec![
                IppValue::Keyword("cover-open-report".try_into().unwrap()),
                IppValue::Keyword("media-empty-warning".try_into().unwrap()),
            ]),
        )]))
        .unwrap();
        assert_eq!(p.highest_severity(), Some(Severity::Warning));

        let quiet = Printer::decode(&printer_group(vec![])).unwrap();
        assert_eq!(quiet.highest_severity(), None);
    }

    fn job_group(extra: Vec<(&str, IppValue)>) -> IppAttributeGroup {
        let mut g = IppAttributeGroup::new(DelimiterTag::JobAttributes);
        let mut base = vec![
            ("job-id", IppValue::Integer(42)),
            ("job-state", IppValue::Enum(5)),
            (
                "job-printer-uri",
                IppValue::Uri("ipp://localhost:631/printers/HP-8210".try_into().unwrap()),
            ),
        ];
        base.extend(extra);
        for (name, value) in base {
            g.attributes_mut()
                .push(IppAttribute::with_name(name, value).unwrap());
        }
        g
    }

    #[test]
    fn decodes_a_minimal_job() {
        let j = Job::decode(&job_group(vec![])).unwrap();
        assert_eq!(j.id, 42);
        assert_eq!(j.state, JobState::Processing);
        assert_eq!(j.printer, "HP-8210");
        assert_eq!(j.name, None);
        assert_eq!(j.progress, JobProgress::Indeterminate);
    }

    #[test]
    fn absent_impressions_means_indeterminate_not_zero() {
        let j = Job::decode(&job_group(vec![(
            "job-impressions-completed",
            IppValue::Integer(3),
        )]))
        .unwrap();
        assert_eq!(j.progress, JobProgress::Indeterminate);
    }

    #[test]
    fn both_impression_counts_give_page_progress() {
        let j = Job::decode(&job_group(vec![
            ("job-impressions", IppValue::Integer(10)),
            ("job-impressions-completed", IppValue::Integer(3)),
        ]))
        .unwrap();
        assert_eq!(j.progress, JobProgress::Pages { done: 3, total: 10 });
        assert_eq!(j.progress.fraction(), Some(0.3));
    }

    #[test]
    fn zero_total_impressions_is_indeterminate() {
        let j = Job::decode(&job_group(vec![
            ("job-impressions", IppValue::Integer(0)),
            ("job-impressions-completed", IppValue::Integer(0)),
        ]))
        .unwrap();
        assert_eq!(j.progress, JobProgress::Indeterminate);
    }

    #[test]
    fn printer_name_is_the_last_uri_segment() {
        assert_eq!(
            printer_name_from_uri("ipp://localhost:631/printers/HP-8210").as_deref(),
            Some("HP-8210")
        );
        assert_eq!(printer_name_from_uri("").as_deref(), None);
    }

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

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn supplies_pair_names_with_levels() {
        let supplies = Supply::decode_list(
            &s(&["Black Ink", "Cyan Ink"]),
            &[80, 5],
            &s(&["ink-cartridge", "ink-cartridge"]),
            &s(&["#000000", "#00FFFF"]),
            &[10, 10],
        );

        assert_eq!(supplies.len(), 2);
        assert_eq!(supplies[0].name, "Black Ink");
        assert_eq!(supplies[0].level, SupplyLevel::Percent(80));
        assert_eq!(supplies[0].colour.as_deref(), Some("#000000"));
        assert_eq!(supplies[1].level, SupplyLevel::Percent(5));
        assert_eq!(supplies[1].low_threshold, Some(10));
    }

    #[test]
    fn negative_level_is_unknown_not_zero() {
        let supplies = Supply::decode_list(&s(&["Drum"]), &[-1], &[], &[], &[]);
        assert_eq!(supplies[0].level, SupplyLevel::Unknown);
        assert_eq!(supplies[0].kind, None);
        assert_eq!(supplies[0].colour, None);
        assert_eq!(supplies[0].low_threshold, None);
    }

    #[test]
    fn level_above_100_is_clamped() {
        let supplies = Supply::decode_list(&s(&["Weird"]), &[150], &[], &[], &[]);
        assert_eq!(supplies[0].level, SupplyLevel::Percent(100));
    }

    #[test]
    fn mismatched_names_and_levels_truncates_rather_than_erroring() {
        // Spec ¤9: one malformed attribute never blanks the whole queue.
        let supplies = Supply::decode_list(&s(&["A", "B"]), &[50], &[], &[], &[]);
        assert_eq!(supplies.len(), 1);
        assert_eq!(supplies[0].name, "A");
        assert_eq!(supplies[0].level, SupplyLevel::Percent(50));
    }

    #[test]
    fn no_markers_yields_no_supplies() {
        assert!(Supply::decode_list(&[], &[], &[], &[], &[]).is_empty());
    }

    #[test]
    fn multi_valued_printer_uri_takes_the_first_value() {
        // `printer_group` already seeds a scalar printer-uri-supported, and
        // IppAttributeGroup::get returns the first match by name, so this
        // builds the group by hand to put the Array value where the decoder
        // will actually see it.
        let mut g = IppAttributeGroup::new(DelimiterTag::PrinterAttributes);
        for (name, value) in [
            (
                "printer-name",
                IppValue::NameWithoutLanguage("HP-8210".try_into().unwrap()),
            ),
            (
                "printer-uri-supported",
                IppValue::Array(vec![
                    IppValue::Uri("ipp://localhost/printers/HP-8210".try_into().unwrap()),
                    IppValue::Uri("ipps://localhost/printers/HP-8210".try_into().unwrap()),
                ]),
            ),
            ("printer-state", IppValue::Enum(3)),
            ("printer-is-accepting-jobs", IppValue::Boolean(true)),
        ] {
            g.attributes_mut()
                .push(IppAttribute::with_name(name, value).unwrap());
        }

        let p = Printer::decode(&g).unwrap();
        assert_eq!(p.uri, "ipp://localhost/printers/HP-8210");
    }
}
