// SPDX-License-Identifier: MIT OR Apache-2.0

use crate::{Error, Result};
use tracing::warn;

/// A job's identifier, unique per printer and assigned on submission.
pub type JobId = i32;

/// What a printer is doing, from IPP `printer-state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterState {
    /// Accepting work and not currently printing.
    Idle,
    /// Working through a job.
    Processing,
    /// Not printing, and will not start until something changes. The reason is
    /// in [`Printer::reasons`] - out of paper, a jam, or deliberately paused.
    Stopped,
}

impl PrinterState {
    /// Reads the IPP enum value. Unknown values are an error rather than a
    /// guess, since treating an unrecognised state as idle would be a lie.
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

/// Where a job has got to, from IPP `job-state`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Queued, waiting its turn.
    Pending,
    /// Held back until released, by [`crate::IppPrinter::release_job`] or by
    /// whatever held it.
    PendingHeld,
    /// Being printed now.
    Processing,
    /// Started, then interrupted - the printer stopped under it.
    ProcessingStopped,
    /// Cancelled by someone.
    Canceled,
    /// Given up on by the printer, rather than cancelled.
    Aborted,
    /// Finished.
    Completed,
}

impl JobState {
    /// Reads the IPP enum value, rejecting anything unrecognised.
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

/// How much a state reason matters. Ordered, so the worst of a set is its
/// maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    /// Worth knowing, but nothing is wrong.
    Report,
    /// Printing continues, but something needs attention soon - ink low.
    Warning,
    /// Printing has stopped or will fail - out of paper, a jam, a door open.
    Error,
}

/// One reason a printer or job is in the state it is in.
///
/// IPP carries these as keywords with an optional severity suffix, as in
/// `media-empty-error`. The suffix is split off into [`StateReason::severity`]
/// so the keyword can be matched on without worrying about which of the three
/// forms a given printer used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateReason {
    /// The reason itself, with any severity suffix removed - `media-empty`,
    /// `toner-low`, `cover-open`.
    pub keyword: String,
    /// How serious it is. Reasons with no suffix are reported as
    /// [`Severity::Report`].
    pub severity: Severity,
}

impl StateReason {
    /// Splits one raw `*-state-reasons` keyword into reason and severity.
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

/// How full a supply is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupplyLevel {
    /// A percentage in 0..=100.
    Percent(u8),
    /// CUPS reports a negative level when it does not know.
    Unknown,
}

/// One consumable the printer reports a level for: a cartridge, a drum, a
/// waste tank.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Supply {
    /// The printer's own name for it, such as `Black Cartridge`.
    pub name: String,
    /// What kind of supply it is - `toner`, `ink`, `wasteToner` - where the
    /// printer says.
    pub kind: Option<String>,
    /// Its colour as an sRGB hex string, where the printer says.
    pub colour: Option<String>,
    /// How much is left.
    pub level: SupplyLevel,
    /// The level at or below which the printer considers this supply low.
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

/// A printer, as it describes itself.
///
/// Whether this is a CUPS queue or a printer reached directly makes no
/// difference to the shape: both answer `Get-Printer-Attributes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Printer {
    /// The queue or printer name.
    pub name: String,
    /// The printer's own address, from `printer-uri-supported`. This is what
    /// [`crate::IppClient::at`] takes to address it directly.
    pub uri: String,
    /// The backend URI CUPS prints through, e.g.
    /// `ipps://HP%20OfficeJet._ipps._tcp.local/`. Distinct from `uri`, which is
    /// the queue's own address on this machine. Empty when CUPS does not say.
    pub device_uri: String,
    /// The human description, as set by whoever configured it.
    pub info: Option<String>,
    /// Where it physically is, as set by whoever configured it.
    pub location: Option<String>,
    /// What it is doing now.
    pub state: PrinterState,
    /// Why it is in that state. Empty when there is nothing to say.
    pub reasons: Vec<StateReason>,
    /// Whether new jobs are being taken. A printer can be stopped and still
    /// accepting, which is how work queues up while paper is replaced.
    pub accepting_jobs: bool,
    /// Consumable levels, where reported. CUPS caches these only after a job
    /// has run, so a freshly added queue reports none.
    pub supplies: Vec<Supply>,
    /// Whether this is the daemon's default destination.
    pub is_default: bool,
    /// Job option defaults the queue advertises.
    pub options: PrinterOptions,
}

/// Print quality, from the IPP `print-quality` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrintQuality {
    /// Fastest, lowest quality.
    Draft,
    /// The printer's usual quality.
    Normal,
    /// Slowest, best quality.
    High,
}

impl PrintQuality {
    /// `None` outside the registered set, so an unexpected value is dropped
    /// rather than silently reported as some other quality.
    pub fn from_ipp(value: i32) -> Option<Self> {
        match value {
            3 => Some(PrintQuality::Draft),
            4 => Some(PrintQuality::Normal),
            5 => Some(PrintQuality::High),
            _ => None,
        }
    }

    /// The IPP enum value to send back.
    pub fn to_ipp(self) -> i32 {
        match self {
            PrintQuality::Draft => 3,
            PrintQuality::Normal => 4,
            PrintQuality::High => 5,
        }
    }
}

/// What a queue advertises for one option, and the value currently set.
///
/// An option the queue says nothing about is empty rather than an error: not
/// every printer offers every option.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionValues<T> {
    /// Every value the queue says it accepts, in the order it listed them.
    pub supported: Vec<T>,
    /// The value currently in effect for new jobs.
    pub default: Option<T>,
}

impl<T> Default for OptionValues<T> {
    fn default() -> Self {
        OptionValues {
            supported: Vec::new(),
            default: None,
        }
    }
}

impl<T> OptionValues<T> {
    /// Whether this is worth offering. One value is not a choice, and a
    /// control that can only be set to what it already says is noise.
    pub fn is_choice(&self) -> bool {
        self.supported.len() >= 2
    }
}

/// The job options a queue advertises defaults for.
///
/// Deliberately the five in the panel design rather than everything a PPD can
/// express: rendering an arbitrary option tree well is its own project.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrinterOptions {
    /// Page sizes, as PWG self-describing names. See [`MediaSize`].
    pub media: OptionValues<String>,
    /// One- or two-sided, as `one-sided`, `two-sided-long-edge`,
    /// `two-sided-short-edge`.
    pub sides: OptionValues<String>,
    /// Colour or monochrome, as `color` and `monochrome`.
    pub color_mode: OptionValues<String>,
    /// Print quality.
    pub quality: OptionValues<PrintQuality>,
    /// Output tray, as `face-up`, `face-down` and vendor-specific names.
    pub output_bin: OptionValues<String>,
}

impl PrinterOptions {
    fn decode(a: &Attrs) -> PrinterOptions {
        PrinterOptions {
            media: OptionValues {
                supported: a.texts("media-supported"),
                default: a.text("media-default"),
            },
            sides: OptionValues {
                supported: a.texts("sides-supported"),
                default: a.text("sides-default"),
            },
            color_mode: OptionValues {
                supported: a.texts("print-color-mode-supported"),
                default: a.text("print-color-mode-default"),
            },
            quality: OptionValues {
                supported: a
                    .ints("print-quality-supported")
                    .into_iter()
                    .filter_map(PrintQuality::from_ipp)
                    .collect(),
                default: a
                    .int("print-quality-default")
                    .and_then(PrintQuality::from_ipp),
            },
            output_bin: OptionValues {
                supported: a.texts("output-bin-supported"),
                default: a.text("output-bin-default"),
            },
        }
    }
}

/// A printer class: several queues addressed as one.
///
/// CUPS serves classes from the same URI space as printers, so a class
/// responds to `Get-Printer-Attributes` like any queue - its state and supply
/// levels come from [`crate::IppClient::printer`]. What is particular to a
/// class is which queues belong to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Class {
    /// The class name, which is addressed exactly like a printer name.
    pub name: String,
    /// The queues in this class, in the order CUPS lists them.
    pub members: Vec<String>,
}

impl Class {
    /// Reads a class from an IPP printer-attributes group.
    pub fn decode(group: &IppAttributeGroup) -> Result<Class> {
        let a = Attrs::new(group);
        Ok(Class {
            name: a.require_text("printer-name")?,
            members: a.texts("member-names"),
        })
    }
}

/// A driver CUPS offers, as returned by `CUPS-Get-PPDs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ppd {
    /// The `ppd-name`, which is what a printer-add operation wants.
    pub name: String,
    /// CUPS' own description, e.g. `HP OfficeJet Pro 8210 Postscript`.
    pub make_and_model: String,
    /// The IEEE-1284 device id this driver claims to drive, if it says.
    pub device_id: String,
}

impl Ppd {
    /// Reads a driver from an IPP printer-attributes group.
    pub fn decode(group: &IppAttributeGroup) -> Result<Ppd> {
        let a = Attrs::new(group);
        Ok(Ppd {
            name: a.require_text("ppd-name")?,
            make_and_model: a.text("ppd-make-and-model").unwrap_or_default(),
            device_id: a.text("ppd-device-id").unwrap_or_default(),
        })
    }
}

/// A media size named with the PWG self-describing scheme, as in
/// `iso_a4_210x297mm` - `class_name_dimensions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSize {
    /// The original keyword, which is what CUPS wants set back.
    pub keyword: String,
    /// The naming authority: `iso`, `na`, `jis`, `om` for vendor-defined,
    /// `custom` for the range markers that are not selectable sizes.
    pub class: String,
    /// The vendor's name for the size, such as `a4` or `letter`. Varies
    /// between printers for the same physical size, which is why
    /// [`MediaSize::dimensions`] is what should be compared.
    pub name: String,
    /// The dimension suffix, e.g. `210x297mm`.
    ///
    /// A size and its borderless twin always share this, while the name part
    /// varies by vendor - `a4` against `a-4`, `hagaki` against `postcard` - so
    /// this is the only reliable way to pair them.
    pub dimensions: String,
    /// Whether this is the borderless variant of the size.
    pub borderless: bool,
}

impl MediaSize {
    /// `None` for anything not shaped like a PWG name.
    pub fn parse(keyword: &str) -> Option<MediaSize> {
        let (head, dimensions) = keyword.rsplit_once('_')?;
        let (class, name) = head.split_once('_')?;
        if class.is_empty() || name.is_empty() || dimensions.is_empty() {
            return None;
        }
        Some(MediaSize {
            keyword: keyword.to_string(),
            class: class.to_string(),
            name: name.to_string(),
            dimensions: dimensions.to_string(),
            borderless: name.contains(".borderless"),
        })
    }
}

impl Printer {
    /// Reads a printer from an IPP printer-attributes group.
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
            options: PrinterOptions::decode(&a),
        })
    }

    /// The worst severity among the current state reasons, if any.
    pub fn highest_severity(&self) -> Option<Severity> {
        self.reasons.iter().map(|r| r.severity).max()
    }
}

use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// How far through a job the printer has got.
pub enum JobProgress {
    /// CUPS did not report a page count. Render an indeterminate bar.
    Indeterminate,
    /// A known page count.
    Pages {
        /// Pages printed so far.
        done: i32,
        /// Pages in the job.
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

/// A print job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The job's id on its printer.
    pub id: JobId,
    /// The printer or queue it was submitted to.
    pub printer: String,
    /// The document name, where one was given. CUPS discards this once a job
    /// finishes, so a completed job usually has none.
    pub name: Option<String>,
    /// Who submitted it. Discarded on completion, like the name.
    pub user: Option<String>,
    /// Where the job has got to.
    pub state: JobState,
    /// Why, where the printer says.
    pub reasons: Vec<StateReason>,
    /// Page progress, where the printer reports it.
    pub progress: JobProgress,
    /// When it was submitted.
    pub created: Option<SystemTime>,
    /// Pages actually printed, when CUPS reports it.
    ///
    /// Separate from `progress` because a finished job keeps
    /// `job-impressions-completed` but loses `job-impressions`, so the pair
    /// that `progress` needs is gone while the count itself survives.
    pub pages_printed: Option<i32>,
    /// When the job reached a terminal state.
    ///
    /// Only completed jobs carry it. Note CUPS discards `job-name` and
    /// `job-originating-user-name` once a job completes - measured against
    /// cupsd 2.4.19 - so a finished job is identifiable by little more than
    /// its id and this time.
    pub completed: Option<SystemTime>,
}

/// Extracts the CUPS queue name from a printer URI.
pub fn printer_name_from_uri(uri: &str) -> Option<String> {
    uri.rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl Job {
    /// Reads a job from an IPP job-attributes group.
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
            pages_printed: a.int("job-impressions-completed").filter(|n| *n > 0),
            completed: a
                .int("time-at-completed")
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
    use super::{
        Class, JobState, MediaSize, OptionValues, Ppd, PrintQuality, PrinterState, Severity,
        StateReason, Supply, SupplyLevel,
    };

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

    fn kw(v: &str) -> IppValue {
        IppValue::Keyword(v.try_into().unwrap())
    }

    fn ppd_group(extra: Vec<(&str, IppValue)>) -> IppAttributeGroup {
        let mut g = IppAttributeGroup::new(DelimiterTag::PrinterAttributes);
        for (name, value) in extra {
            g.attributes_mut()
                .push(IppAttribute::with_name(name, value).unwrap());
        }
        g
    }

    #[test]
    fn decodes_a_class_and_its_members() {
        let mut g = IppAttributeGroup::new(DelimiterTag::PrinterAttributes);
        for (name, value) in [
            (
                "printer-name",
                IppValue::NameWithoutLanguage("Upstairs".try_into().unwrap()),
            ),
            (
                "member-names",
                IppValue::Array(vec![
                    IppValue::NameWithoutLanguage("HP-8210".try_into().unwrap()),
                    IppValue::NameWithoutLanguage("Office-Laser".try_into().unwrap()),
                ]),
            ),
        ] {
            g.attributes_mut()
                .push(IppAttribute::with_name(name, value).unwrap());
        }

        let class = Class::decode(&g).unwrap();
        assert_eq!(class.name, "Upstairs");
        assert_eq!(class.members, vec!["HP-8210", "Office-Laser"]);
    }

    #[test]
    fn a_class_with_no_members_is_not_an_error() {
        // CUPS keeps a class whose printers have all been removed.
        let mut g = IppAttributeGroup::new(DelimiterTag::PrinterAttributes);
        g.attributes_mut().push(
            IppAttribute::with_name(
                "printer-name",
                IppValue::NameWithoutLanguage("Empty".try_into().unwrap()),
            )
            .unwrap(),
        );
        assert!(Class::decode(&g).unwrap().members.is_empty());
    }

    #[test]
    fn decodes_a_driver() {
        let ppd = Ppd::decode(&ppd_group(vec![
            (
                "ppd-name",
                IppValue::NameWithoutLanguage(
                    "lsb/usr/HP/hp-officejet_pro_8210-ps.ppd.gz"
                        .try_into()
                        .unwrap(),
                ),
            ),
            (
                "ppd-make-and-model",
                IppValue::TextWithoutLanguage(
                    "HP OfficeJet Pro 8210 Postscript".try_into().unwrap(),
                ),
            ),
        ]))
        .unwrap();
        assert_eq!(ppd.name, "lsb/usr/HP/hp-officejet_pro_8210-ps.ppd.gz");
        assert_eq!(ppd.make_and_model, "HP OfficeJet Pro 8210 Postscript");
        // Not every driver declares one, and that is not an error.
        assert!(ppd.device_id.is_empty());
    }

    #[test]
    fn a_driver_without_a_name_is_a_decode_error() {
        // ppd-name is the only field a caller cannot do without: it is what
        // gets written when the printer is added.
        assert!(Ppd::decode(&ppd_group(vec![])).is_err());
    }

    #[test]
    fn print_quality_maps_the_ipp_enums() {
        assert_eq!(PrintQuality::from_ipp(3), Some(PrintQuality::Draft));
        assert_eq!(PrintQuality::from_ipp(4), Some(PrintQuality::Normal));
        assert_eq!(PrintQuality::from_ipp(5), Some(PrintQuality::High));
        // Unknown values are dropped rather than guessed at.
        assert_eq!(PrintQuality::from_ipp(9), None);
    }

    #[test]
    fn options_decode_supported_values_and_the_current_default() {
        let p = Printer::decode(&printer_group(vec![
            (
                "media-supported",
                IppValue::Array(vec![kw("iso_a4_210x297mm"), kw("na_letter_8.5x11in")]),
            ),
            ("media-default", kw("iso_a4_210x297mm")),
            (
                "print-quality-supported",
                IppValue::Array(vec![IppValue::Enum(3), IppValue::Enum(4)]),
            ),
            ("print-quality-default", IppValue::Enum(4)),
        ]))
        .unwrap();

        assert_eq!(p.options.media.supported.len(), 2);
        assert_eq!(p.options.media.default.as_deref(), Some("iso_a4_210x297mm"));
        assert_eq!(
            p.options.quality.supported,
            vec![PrintQuality::Draft, PrintQuality::Normal]
        );
        assert_eq!(p.options.quality.default, Some(PrintQuality::Normal));
    }

    #[test]
    fn an_option_the_queue_does_not_advertise_is_empty_not_an_error() {
        // A queue that offers no choice of output bin simply says nothing.
        let p = Printer::decode(&printer_group(vec![])).unwrap();
        assert!(p.options.output_bin.supported.is_empty());
        assert_eq!(p.options.output_bin.default, None);
    }

    #[test]
    fn a_scalar_supported_value_reads_as_one_choice() {
        // Measured against cupsd: output-bin-supported comes back as a bare
        // keyword, not a 1setOf, when the printer has exactly one bin.
        let p = Printer::decode(&printer_group(vec![
            ("output-bin-supported", kw("face-up")),
            ("output-bin-default", kw("face-up")),
        ]))
        .unwrap();
        assert_eq!(p.options.output_bin.supported, vec!["face-up".to_string()]);
        assert!(!p.options.output_bin.is_choice());
    }

    #[test]
    fn an_option_is_a_choice_only_with_two_or_more_values() {
        let one = OptionValues {
            supported: vec!["face-up".to_string()],
            default: None,
        };
        let two = OptionValues {
            supported: vec!["one-sided".to_string(), "two-sided-long-edge".to_string()],
            default: None,
        };
        assert!(!one.is_choice());
        assert!(two.is_choice());
    }

    #[test]
    fn media_names_parse_into_class_dimensions_and_borderless() {
        let a4 = MediaSize::parse("iso_a4_210x297mm").unwrap();
        assert_eq!(a4.class, "iso");
        assert_eq!(a4.dimensions, "210x297mm");
        assert!(!a4.borderless);

        let bl = MediaSize::parse("om_a-4.borderless_210x297mm").unwrap();
        assert!(bl.borderless);
        assert_eq!(bl.dimensions, "210x297mm");
    }

    #[test]
    fn a_size_and_its_borderless_twin_share_dimensions() {
        // Vendors vary the name part - a4 vs a-4, hagaki vs postcard - so the
        // dimension suffix is the only reliable way to pair the two.
        for (plain, borderless) in [
            ("iso_a4_210x297mm", "om_a-4.borderless_210x297mm"),
            ("na_letter_8.5x11in", "oe_letter.borderless_8.5x11in"),
            ("jpn_hagaki_100x148mm", "om_postcard.borderless_100x148mm"),
        ] {
            let a = MediaSize::parse(plain).unwrap();
            let b = MediaSize::parse(borderless).unwrap();
            assert_eq!(a.dimensions, b.dimensions, "{plain} vs {borderless}");
            assert!(!a.borderless && b.borderless);
        }
    }

    #[test]
    fn the_two_16k_sizes_are_not_confused() {
        // Same vendor name, different sizes: pairing on the name would merge
        // them, pairing on dimensions keeps them apart.
        let small = MediaSize::parse("om_16k_184x260mm").unwrap();
        let large = MediaSize::parse("om_16k_195x270mm").unwrap();
        assert_ne!(small.dimensions, large.dimensions);
    }

    #[test]
    fn custom_range_markers_are_identifiable() {
        // custom_min/custom_max describe the range a printer accepts, they are
        // not sizes anyone can pick.
        assert_eq!(
            MediaSize::parse("custom_min_3x5in").unwrap().class,
            "custom"
        );
        assert_eq!(
            MediaSize::parse("custom_max_8.5x14in").unwrap().class,
            "custom"
        );
    }

    #[test]
    fn a_name_without_the_pwg_shape_is_rejected() {
        assert!(MediaSize::parse("A4").is_none());
        assert!(MediaSize::parse("").is_none());
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
    fn a_finished_job_keeps_its_page_count_without_a_total() {
        // CUPS drops job-impressions once a job finishes but keeps the
        // completed count, so progress goes indeterminate while the number of
        // pages actually printed is still known.
        let job = Job::decode(&job_group(vec![
            ("job-state", IppValue::Enum(9)),
            ("job-impressions-completed", IppValue::Integer(2)),
        ]))
        .unwrap();
        assert_eq!(job.pages_printed, Some(2));
        assert_eq!(job.progress, JobProgress::Indeterminate);
    }

    #[test]
    fn a_job_that_printed_nothing_reports_no_page_count() {
        let job = Job::decode(&job_group(vec![(
            "job-impressions-completed",
            IppValue::Integer(0),
        )]))
        .unwrap();
        assert_eq!(job.pages_printed, None);
    }

    #[test]
    fn a_completed_job_carries_its_completion_time() {
        let job = Job::decode(&job_group(vec![
            ("job-state", IppValue::Enum(9)),
            ("time-at-completed", IppValue::Integer(1787935946)),
        ]))
        .unwrap();
        assert_eq!(
            job.completed,
            Some(UNIX_EPOCH + Duration::from_secs(1787935946))
        );
    }

    #[test]
    fn a_job_still_running_has_no_completion_time() {
        let job = Job::decode(&job_group(vec![])).unwrap();
        assert_eq!(job.completed, None);
    }

    #[test]
    fn a_zero_completion_time_reads_as_absent() {
        // CUPS uses 0 for "not yet", which as an epoch would render as 1970.
        let job = Job::decode(&job_group(vec![(
            "time-at-completed",
            IppValue::Integer(0),
        )]))
        .unwrap();
        assert_eq!(job.completed, None);
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
