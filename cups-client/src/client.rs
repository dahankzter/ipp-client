// SPDX-License-Identifier: GPL-3.0-only

use ipp::{
    operation::{GetJobs, IppOperation, cups::CupsGetPrinters},
    prelude::*,
};
use tracing::warn;

use crate::{Error, Job, JobId, Printer, Result, lpoptions};

const LOCAL_CUPS: &str = "http://localhost:631";

/// Async client for a CUPS daemon.
pub struct CupsClient {
    inner: AsyncIppClient,
    base: String,
    user: String,
}

impl CupsClient {
    /// Connects to the local CUPS daemon as the current user.
    pub fn local() -> Result<Self> {
        let user = std::env::var("USER").unwrap_or_else(|_| "anonymous".to_string());
        Self::with_uri(LOCAL_CUPS, &user)
    }

    pub fn with_uri(uri: &str, user: &str) -> Result<Self> {
        let parsed: Uri = uri
            .parse()
            .map_err(|e| Error::Transport(format!("bad CUPS uri {uri}: {e}")))?;

        Ok(CupsClient {
            inner: AsyncIppClient::new(parsed),
            base: uri.trim_end_matches('/').to_string(),
            user: user.to_string(),
        })
    }

    pub(crate) fn user(&self) -> &str {
        &self.user
    }

    /// The IPP URI for one queue, e.g. `http://localhost:631/printers/HP-8210`.
    pub(crate) fn printer_uri(&self, name: &str) -> Result<Uri> {
        format!("{}/printers/{name}", self.base)
            .parse()
            .map_err(|e| Error::Transport(format!("bad printer uri for {name}: {e}")))
    }

    pub(crate) async fn send(
        &self,
        req: impl Into<IppRequestResponse>,
        label: &str,
    ) -> Result<IppRequestResponse> {
        let resp = self
            .inner
            .send(req.into())
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Self::check_status(&resp, label)?;
        Ok(resp)
    }

    pub(crate) fn check_status(resp: &IppRequestResponse, op: &str) -> Result<()> {
        let status = resp.header().status_code();
        if status.is_success() {
            Ok(())
        } else {
            Err(Error::Ipp {
                operation: op.to_string(),
                status: format!("{status:?}"),
            })
        }
    }

    /// Decodes every printer group, skipping and logging ones that fail.
    pub(crate) fn decode_printers(
        resp: &IppRequestResponse,
        default: Option<&str>,
    ) -> Vec<Printer> {
        resp.attributes()
            .groups_of(DelimiterTag::PrinterAttributes)
            .filter_map(|group| match Printer::decode(group) {
                Ok(mut printer) => {
                    printer.is_default = default == Some(printer.name.as_str());
                    Some(printer)
                }
                Err(e) => {
                    warn!("skipping undecodable printer: {e}");
                    None
                }
            })
            .collect()
    }

    /// The server's own default queue, via `CUPS-Get-Default`.
    pub(crate) async fn server_default(&self) -> Result<Option<String>> {
        let mut request =
            IppRequestResponse::new(IppVersion::v1_1(), Operation::CupsGetDefault, None)
                .map_err(|e| Error::Transport(e.to_string()))?;
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(
                "requested-attributes",
                IppValue::Keyword("printer-name".try_into().unwrap()),
            )
            .map_err(|e| Error::decode("requested-attributes", e.to_string()))?,
        );

        let resp = self.send(request, "CUPS-Get-Default").await?;

        Ok(resp
            .attributes()
            .groups_of(DelimiterTag::PrinterAttributes)
            .next()
            .and_then(|g| crate::attrs::Attrs::new(g).text("printer-name")))
    }

    /// The user's `lpoptions` choice if they made one, else the server's default.
    pub async fn default_printer(&self) -> Option<String> {
        if let Some(chosen) = lpoptions::default_printer() {
            return Some(chosen);
        }
        match self.server_default().await {
            Ok(name) => name,
            Err(e) => {
                warn!("cannot read the default printer: {e}");
                None
            }
        }
    }

    /// Attributes `Printer::decode` needs. Without this, `CUPS-Get-Printers`
    /// returns every attribute CUPS knows about the queue.
    const PRINTER_ATTRIBUTES: &'static [&'static str] = &[
        "printer-name",
        "printer-uri-supported",
        "printer-info",
        "printer-location",
        "printer-state",
        "printer-state-reasons",
        "printer-is-accepting-jobs",
        "marker-names",
        "marker-levels",
        "marker-types",
        "marker-colors",
        "marker-low-levels",
    ];

    /// Lists all queues known to CUPS.
    pub async fn printers(&self) -> Result<Vec<Printer>> {
        let default = self.default_printer().await;

        let mut request = CupsGetPrinters::new().into_ipp_request();

        let mut wanted = Vec::with_capacity(Self::PRINTER_ATTRIBUTES.len());
        for name in Self::PRINTER_ATTRIBUTES {
            wanted.push(IppValue::Keyword(
                (*name)
                    .try_into()
                    .map_err(|_| Error::decode(*name, "keyword too long"))?,
            ));
        }
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name("requested-attributes", IppValue::Array(wanted))
                .map_err(|e| Error::decode("requested-attributes", e.to_string()))?,
        );

        let resp = self
            .inner
            .send(request)
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Self::check_status(&resp, "CUPS-Get-Printers")?;

        Ok(Self::decode_printers(&resp, default.as_deref()))
    }

    pub(crate) fn decode_jobs(resp: &IppRequestResponse) -> Vec<Job> {
        resp.attributes()
            .groups_of(DelimiterTag::JobAttributes)
            .filter_map(|group| match Job::decode(group) {
                Ok(job) => Some(job),
                Err(e) => {
                    warn!("skipping undecodable job: {e}");
                    None
                }
            })
            .collect()
    }

    /// Attributes `Job::decode` needs. CUPS returns only `job-id` without this.
    const JOB_ATTRIBUTES: &'static [&'static str] = &[
        "job-id",
        "job-state",
        "job-state-reasons",
        "job-name",
        "job-printer-uri",
        "job-originating-user-name",
        "time-at-creation",
        "job-impressions",
        "job-impressions-completed",
    ];

    /// Builds the `Get-Jobs` request. Split out so its shape can be tested.
    pub(crate) fn jobs_request(&self, which: WhichJobs) -> Result<IppRequestResponse> {
        let root: Uri = self
            .base
            .parse()
            .map_err(|e| Error::Transport(format!("bad CUPS uri: {e}")))?;

        let op = GetJobs::new(root, Some(self.user.clone()))
            .map_err(|e| Error::Transport(e.to_string()))?;

        let mut request = op.into_ipp_request();

        let mut wanted = Vec::with_capacity(Self::JOB_ATTRIBUTES.len());
        for name in Self::JOB_ATTRIBUTES {
            wanted.push(IppValue::Keyword(
                (*name)
                    .try_into()
                    .map_err(|_| Error::decode(*name, "keyword too long"))?,
            ));
        }
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name("requested-attributes", IppValue::Array(wanted))
                .map_err(|e| Error::decode("requested-attributes", e.to_string()))?,
        );

        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(
                "which-jobs",
                IppValue::Keyword(
                    which
                        .as_keyword()
                        .try_into()
                        .map_err(|_| Error::decode("which-jobs", "keyword too long"))?,
                ),
            )
            .map_err(|e| Error::decode("which-jobs", e.to_string()))?,
        );
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name("my-jobs", IppValue::Boolean(true))
                .map_err(|e| Error::decode("my-jobs", e.to_string()))?,
        );

        Ok(request)
    }

    /// Lists the current user's jobs across every queue.
    pub async fn jobs(&self, which: WhichJobs) -> Result<Vec<Job>> {
        let request = self.jobs_request(which)?;

        let resp = self
            .inner
            .send(request)
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Self::check_status(&resp, "Get-Jobs")?;

        Ok(Self::decode_jobs(&resp))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhichJobs {
    /// Jobs still in the queue.
    NotCompleted,
    /// Jobs that have finished, been cancelled, or aborted.
    Completed,
}

impl WhichJobs {
    pub(crate) fn as_keyword(&self) -> &'static str {
        match self {
            WhichJobs::NotCompleted => "not-completed",
            WhichJobs::Completed => "completed",
        }
    }
}

impl CupsClient {
    /// Builds a `Cancel-Job` / `Hold-Job` / `Release-Job` request.
    pub(crate) fn job_action_request(
        &self,
        operation: Operation,
        printer: &str,
        id: JobId,
    ) -> Result<IppRequestResponse> {
        let uri = self.printer_uri(printer)?;
        let mut request = IppRequestResponse::new(IppVersion::v1_1(), operation, Some(uri))
            .map_err(|e| Error::Transport(e.to_string()))?;

        for (name, value) in [
            ("job-id", IppValue::Integer(id)),
            (
                "requesting-user-name",
                IppValue::NameWithoutLanguage(
                    self.user
                        .as_str()
                        .try_into()
                        .map_err(|_| Error::decode("requesting-user-name", "name too long"))?,
                ),
            ),
        ] {
            request.attributes_mut().add(
                DelimiterTag::OperationAttributes,
                IppAttribute::with_name(name, value)
                    .map_err(|e| Error::decode(name, e.to_string()))?,
            );
        }

        Ok(request)
    }

    async fn job_action(
        &self,
        operation: Operation,
        label: &str,
        printer: &str,
        id: JobId,
    ) -> Result<()> {
        let request = self.job_action_request(operation, printer, id)?;
        self.send(request, label).await?;
        Ok(())
    }

    /// Cancels one of the current user's jobs.
    pub async fn cancel_job(&self, printer: &str, id: JobId) -> Result<()> {
        self.job_action(Operation::CancelJob, "Cancel-Job", printer, id).await
    }

    /// Holds one of the current user's jobs.
    pub async fn hold_job(&self, printer: &str, id: JobId) -> Result<()> {
        self.job_action(Operation::HoldJob, "Hold-Job", printer, id).await
    }

    /// Releases one of the current user's held jobs.
    pub async fn release_job(&self, printer: &str, id: JobId) -> Result<()> {
        self.job_action(Operation::ReleaseJob, "Release-Job", printer, id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ipp::parser::IppParser;

    fn fixture() -> IppRequestResponse {
        let bytes = include_bytes!("../testdata/cups-get-printers.bin");
        IppParser::new(std::io::Cursor::new(bytes.to_vec()))
            .parse()
            .expect("fixture parses")
    }

    #[test]
    fn decodes_every_printer_in_a_real_response() {
        let printers = CupsClient::decode_printers(&fixture(), None);
        assert!(!printers.is_empty(), "fixture should contain at least one printer");
        assert!(printers.iter().all(|p| !p.name.is_empty()));
        assert!(printers.iter().all(|p| !p.is_default), "no default was supplied");
    }

    #[test]
    fn the_named_default_is_the_one_marked_default() {
        let all = CupsClient::decode_printers(&fixture(), None);
        let name = all.first().expect("fixture has a printer").name.clone();

        let marked = CupsClient::decode_printers(&fixture(), Some(&name));
        assert_eq!(marked.iter().filter(|p| p.is_default).count(), 1);
        assert!(marked.iter().find(|p| p.is_default).unwrap().name == name);
    }

    #[test]
    fn success_status_passes_the_check() {
        assert!(CupsClient::check_status(&fixture(), "CUPS-Get-Printers").is_ok());
    }

    #[test]
    fn error_status_names_the_operation() {
        let mut resp = fixture();
        // 0x0400 = client-error-bad-request
        resp.header_mut().operation_or_status = 0x0400;
        let err = CupsClient::check_status(&resp, "CUPS-Get-Printers").unwrap_err();
        assert!(err.to_string().contains("CUPS-Get-Printers"));
    }

    #[tokio::test]
    #[ignore = "requires a running cupsd"]
    async fn lists_printers_from_the_live_daemon() {
        let client = CupsClient::local().unwrap();
        let printers = client.printers().await.unwrap();
        assert!(printers.iter().all(|p| !p.name.is_empty()));
        // The target machine has no lpoptions file, so this exercises the
        // CUPS-Get-Default fallback specifically.
        assert_eq!(
            printers.iter().filter(|p| p.is_default).count(),
            1,
            "exactly one queue should be marked default"
        );
    }

    #[test]
    fn which_jobs_maps_to_the_ipp_keyword() {
        assert_eq!(WhichJobs::NotCompleted.as_keyword(), "not-completed");
        assert_eq!(WhichJobs::Completed.as_keyword(), "completed");
    }

    #[test]
    fn the_jobs_request_asks_for_every_attribute_the_decoder_needs() {
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client.jobs_request(WhichJobs::NotCompleted).unwrap();

        let group = request
            .attributes()
            .first_of(DelimiterTag::OperationAttributes)
            .unwrap();
        let attrs = crate::attrs::Attrs::new(group);

        let requested = attrs.texts("requested-attributes");
        // Without these, CUPS answers with job-id alone and every job is skipped.
        for needed in ["job-id", "job-state", "job-printer-uri", "job-impressions"] {
            assert!(requested.contains(&needed.to_string()), "missing {needed}");
        }
        assert_eq!(attrs.text("which-jobs").as_deref(), Some("not-completed"));
        assert_eq!(attrs.bool("my-jobs"), Some(true));
    }

    #[test]
    fn a_response_with_no_job_groups_yields_no_jobs() {
        // CUPS answers an empty queue with operation attributes only.
        assert!(CupsClient::decode_jobs(&fixture()).is_empty());
    }

    #[tokio::test]
    #[ignore = "requires a running cupsd"]
    async fn lists_jobs_from_the_live_daemon() {
        let client = CupsClient::local().unwrap();
        let jobs = client.jobs(WhichJobs::NotCompleted).await.unwrap();
        assert!(jobs.iter().all(|j| j.state.is_active()));
    }

    #[test]
    fn job_action_request_carries_id_user_and_operation() {
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client
            .job_action_request(Operation::HoldJob, "HP-8210", 42)
            .unwrap();

        assert_eq!(request.header().operation_or_status, Operation::HoldJob as i16);

        let group = request
            .attributes()
            .first_of(DelimiterTag::OperationAttributes)
            .unwrap();
        let attrs = crate::attrs::Attrs::new(group);
        assert_eq!(attrs.int("job-id"), Some(42));
        assert_eq!(attrs.text("requesting-user-name").as_deref(), Some("tester"));
        assert!(attrs.text("printer-uri").unwrap().ends_with("/printers/HP-8210"));
    }

    #[test]
    fn release_uses_its_own_operation_code() {
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client
            .job_action_request(Operation::ReleaseJob, "HP-8210", 7)
            .unwrap();
        assert_eq!(request.header().operation_or_status, Operation::ReleaseJob as i16);
    }
}
