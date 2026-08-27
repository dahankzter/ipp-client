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
    pub fn decode_list(
        names: &[String],
        levels: &[i32],
        types: &[String],
        colours: &[String],
        low: &[i32],
    ) -> Result<Vec<Supply>> {
        if names.len() != levels.len() {
            return Err(Error::decode(
                "marker-levels",
                format!("{} levels for {} names", levels.len(), names.len()),
            ));
        }

        Ok(names
            .iter()
            .enumerate()
            .map(|(i, name)| Supply {
                name: name.clone(),
                kind: types.get(i).cloned(),
                colour: colours.get(i).cloned(),
                level: match levels[i] {
                    n if n < 0 => SupplyLevel::Unknown,
                    n => SupplyLevel::Percent(n.min(100) as u8),
                },
                low_threshold: low.get(i).copied(),
            })
            .collect())
    }
}

use crate::attrs::Attrs;
use ipp::prelude::IppAttributeGroup;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printer {
    pub name: String,
    pub uri: String,
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
            uri: a.text("printer-uri-supported").unwrap_or_default(),
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
            )?,
            is_default: false,
        })
    }

    /// The worst severity among the current state reasons, if any.
    pub fn highest_severity(&self) -> Option<Severity> {
        self.reasons.iter().map(|r| r.severity).max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipp::prelude::*;
    // Re-import our types to shadow the conflicting ipp ones
    use super::{PrinterState, JobState, Severity, StateReason, SupplyLevel, Supply};

    fn printer_group(extra: Vec<(&str, IppValue)>) -> IppAttributeGroup {
        let mut g = IppAttributeGroup::new(DelimiterTag::PrinterAttributes);
        let mut base = vec![
            ("printer-name", IppValue::NameWithoutLanguage("HP-8210".try_into().unwrap())),
            ("printer-uri-supported", IppValue::Uri("ipp://localhost/printers/HP-8210".try_into().unwrap())),
            ("printer-state", IppValue::Enum(3)),
            ("printer-is-accepting-jobs", IppValue::Boolean(true)),
        ];
        base.extend(extra);
        for (name, value) in base {
            g.attributes_mut().push(IppAttribute::with_name(name, value).unwrap());
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
            ("marker-names", IppValue::NameWithoutLanguage("Black Ink".try_into().unwrap())),
            ("marker-levels", IppValue::Integer(42)),
            ("printer-info", IppValue::TextWithoutLanguage("Office printer".try_into().unwrap())),
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
        )
        .unwrap();

        assert_eq!(supplies.len(), 2);
        assert_eq!(supplies[0].name, "Black Ink");
        assert_eq!(supplies[0].level, SupplyLevel::Percent(80));
        assert_eq!(supplies[0].colour.as_deref(), Some("#000000"));
        assert_eq!(supplies[1].level, SupplyLevel::Percent(5));
        assert_eq!(supplies[1].low_threshold, Some(10));
    }

    #[test]
    fn negative_level_is_unknown_not_zero() {
        let supplies =
            Supply::decode_list(&s(&["Drum"]), &[-1], &[], &[], &[]).unwrap();
        assert_eq!(supplies[0].level, SupplyLevel::Unknown);
        assert_eq!(supplies[0].kind, None);
        assert_eq!(supplies[0].colour, None);
        assert_eq!(supplies[0].low_threshold, None);
    }

    #[test]
    fn level_above_100_is_clamped() {
        let supplies =
            Supply::decode_list(&s(&["Weird"]), &[150], &[], &[], &[]).unwrap();
        assert_eq!(supplies[0].level, SupplyLevel::Percent(100));
    }

    #[test]
    fn mismatched_names_and_levels_is_a_decode_error() {
        let err = Supply::decode_list(&s(&["A", "B"]), &[50], &[], &[], &[]).unwrap_err();
        assert!(err.to_string().contains("marker-levels"));
    }

    #[test]
    fn no_markers_yields_no_supplies() {
        assert!(Supply::decode_list(&[], &[], &[], &[], &[]).unwrap().is_empty());
    }
}
