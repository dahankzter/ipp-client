// SPDX-License-Identifier: GPL-3.0-only

use ipp::{
    operation::{GetJobAttributes, GetJobs, IppOperation, cups::CupsGetPrinters},
    prelude::*,
};
use tracing::warn;

use std::sync::Mutex;

use crate::{Error, Job, JobId, Printer, Result, lpoptions};

const LOCAL_CUPS: &str = "http://localhost:631";

/// Async client for a CUPS daemon.
pub struct CupsClient {
    inner: AsyncIppClient,
    base: String,
    user: String,
    /// Cache for `default_printer`. `None` means "not yet resolved this
    /// cycle"; `Some(None)` means "resolved, and there is no default".
    /// Populated lazily and cleared by `invalidate_default_printer_cache`,
    /// so a fresh `Default` per poll never costs a `CUPS-Get-Default`
    /// round trip unless the caller explicitly asks to resync.
    default_cache: Mutex<Option<Option<String>>>,
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
            default_cache: Mutex::new(None),
        })
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

    /// Adds `requested-attributes` to an operation group. Without it CUPS
    /// answers with its own idea of a default set, which omits most of what
    /// the decoders need.
    pub(crate) fn request_attributes(
        request: &mut IppRequestResponse,
        names: &[&str],
    ) -> Result<()> {
        let mut wanted = Vec::with_capacity(names.len());
        for name in names {
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
        Ok(())
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
    ///
    /// Cached after the first resolution so the 3-second poll loop does not
    /// pay for a `CUPS-Get-Default` round trip every cycle when there is no
    /// `lpoptions` file. Call `invalidate_default_printer_cache` to force a
    /// fresh lookup (the poll loop does this on every resynchronisation).
    pub async fn default_printer(&self) -> Option<String> {
        if let Some(cached) = self.default_cache.lock().unwrap().clone() {
            return cached;
        }

        let resolved = match lpoptions::default_printer() {
            Some(chosen) => Some(chosen),
            None => match self.server_default().await {
                Ok(name) => name,
                Err(e) => {
                    warn!("cannot read the default printer: {e}");
                    None
                }
            },
        };

        *self.default_cache.lock().unwrap() = Some(resolved.clone());
        resolved
    }

    /// Forces the next `default_printer` call to resolve again instead of
    /// reusing the cached value.
    pub(crate) fn invalidate_default_printer_cache(&self) {
        *self.default_cache.lock().unwrap() = None;
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
        Self::request_attributes(&mut request, Self::PRINTER_ATTRIBUTES)?;

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
        Self::request_attributes(&mut request, Self::JOB_ATTRIBUTES)?;

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

    /// Builds the `Get-Job-Attributes` request. Split out so its shape can be tested.
    pub(crate) fn job_request(&self, printer: &str, id: JobId) -> Result<IppRequestResponse> {
        let uri = self.printer_uri(printer)?;

        let op = GetJobAttributes::new(uri, id, Some(self.user.clone()))
            .map_err(|e| Error::Transport(e.to_string()))?;

        let mut request = op.into_ipp_request();
        Self::request_attributes(&mut request, Self::JOB_ATTRIBUTES)?;

        Ok(request)
    }

    /// Reads one job by id, including jobs that have already left the queue.
    ///
    /// `Get-Jobs` with `which-jobs=not-completed` drops a job the instant it
    /// reaches a terminal state, so the only way to learn *how* a job ended is
    /// to ask for it by id. CUPS retains finished jobs for `PreserveJobHistory`
    /// (a minute by default, often much longer), which is ample for the applet
    /// to read the outcome of a job it just watched leave the queue.
    ///
    /// Returns `Ok(None)` when CUPS no longer knows the job, so a caller can
    /// tell "gone" from "the daemon is unreachable" and stay silent rather than
    /// guess at an outcome.
    pub async fn job(&self, printer: &str, id: JobId) -> Result<Option<Job>> {
        let request = self.job_request(printer, id)?;

        let resp = self
            .inner
            .send(request)
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;

        // A job aged out of the history is a normal outcome, not a failure.
        if resp.header().status_code() == StatusCode::ClientErrorNotFound {
            return Ok(None);
        }
        Self::check_status(&resp, "Get-Job-Attributes")?;

        Ok(Self::decode_jobs(&resp).into_iter().next())
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
    ///
    /// Addresses the job via `<base>/printers/<printer>`, which assumes
    /// `printer` names a CUPS printer, not a CUPS *class*. A job queued
    /// against a class would decode with `job.printer` set from
    /// `job-printer-uri` (the member printer that actually picked it up, per
    /// `Job::decode`), so a class job's controls would point at the wrong
    /// resource and the action would fail against that URI. v1 has no UI for
    /// classes, so this is not exercised in practice, but a future class
    /// integration must resolve the class URI here rather than reusing this
    /// printer-shaped path unchanged.
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

use crate::{
    PrinterEvent,
    events::{POLL_INTERVAL, Snapshot, backoff_after},
};
use futures::Stream;
use ipp::operation::GetPrinterAttributes;
use std::time::Duration;

impl CupsClient {
    /// Reads one queue's attributes, addressing it by its full IPP URI.
    pub async fn printer_at(&self, uri: &str) -> Result<Printer> {
        let parsed: Uri = uri
            .parse()
            .map_err(|e| Error::Transport(format!("bad printer uri {uri}: {e}")))?;
        let op = GetPrinterAttributes::new(parsed)
            .map_err(|e| Error::Transport(e.to_string()))?;
        let resp = self.send(op, "Get-Printer-Attributes").await?;

        resp.attributes()
            .groups_of(DelimiterTag::PrinterAttributes)
            .next()
            .ok_or_else(|| Error::decode("printer-attributes", "no printer group in response"))
            .and_then(Printer::decode)
    }

    /// Reads one CUPS queue by name.
    pub async fn printer(&self, name: &str) -> Result<Printer> {
        let uri = self.printer_uri(name)?;
        self.printer_at(&uri.to_string()).await
    }
}

impl CupsClient {
    /// Reads printers and active jobs in one pass.
    pub(crate) async fn snapshot(&self) -> Result<Snapshot> {
        Ok(Snapshot {
            printers: self.printers().await?,
            jobs: self.jobs(WhichJobs::NotCompleted).await?,
        })
    }

    /// Polls CUPS and yields the changes. Never ends; retries with backoff on error.
    ///
    /// This is the applet's event source. v1 does not use IPP subscriptions:
    /// CUPS's `notify-wait` does not block and the daemon asks clients to poll
    /// once a minute, far worse than the 3-second poll here.
    ///
    /// Requires a tokio runtime with the `time` driver enabled (this crate's
    /// `tokio` dependency only pulls in the `time` feature, not a full
    /// runtime): the returned stream calls `tokio::time::sleep` between polls
    /// and between retries. A caller polling this crate from a non-tokio
    /// executor — the portal-backed backend this may grow later, for
    /// instance — will need to drive it from inside a tokio context, e.g. via
    /// a dedicated `tokio::runtime::Runtime` bridged into that executor.
    pub fn events(&self) -> impl Stream<Item = Result<PrinterEvent>> + '_ {
        async_stream::stream! {
            let mut previous: Option<Snapshot> = None;
            let mut backoff = Duration::ZERO;

            loop {
                if previous.is_none() {
                    // Starting up, or recovering after a failure: re-resolve
                    // the default printer rather than trusting a cache that
                    // may predate the outage.
                    self.invalidate_default_printer_cache();
                }

                match self.snapshot().await {
                    Ok(current) => {
                        backoff = Duration::ZERO;

                        match previous.take() {
                            // First poll, or first poll after a failure.
                            None => yield Ok(PrinterEvent::Resynchronised {
                                printers: current.printers.clone(),
                                jobs: current.jobs.clone(),
                            }),
                            Some(before) => {
                                for event in before.diff(&current) {
                                    yield Ok(event);
                                }
                            }
                        }

                        previous = Some(current);
                        tokio::time::sleep(POLL_INTERVAL).await;
                    }
                    Err(e) => {
                        // Drop the stale snapshot so recovery re-emits Resynchronised.
                        previous = None;
                        yield Err(e);
                        backoff = backoff_after(backoff);
                        tokio::time::sleep(backoff).await;
                    }
                }
            }
        }
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

    #[tokio::test]
    async fn default_printer_is_cached_until_invalidated() {
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();

        // Prime the cache directly, bypassing lpoptions/network resolution
        // entirely. If `default_printer` reused the cache correctly, it
        // returns this value without touching the filesystem or the
        // network; if the cache were skipped it would fall through to
        // `server_default`, which would fail fast against nothing
        // listening on this URI in the test environment.
        *client.default_cache.lock().unwrap() = Some(Some("Cached".to_string()));
        assert_eq!(client.default_printer().await.as_deref(), Some("Cached"));

        // A second call must not re-resolve either.
        assert_eq!(client.default_printer().await.as_deref(), Some("Cached"));

        client.invalidate_default_printer_cache();
        assert!(client.default_cache.lock().unwrap().is_none());
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
    fn the_job_request_asks_for_every_attribute_the_decoder_needs() {
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client.job_request("HP-8210", 42).unwrap();

        assert_eq!(
            request.header().operation_or_status,
            Operation::GetJobAttributes as i16
        );

        let group = request
            .attributes()
            .first_of(DelimiterTag::OperationAttributes)
            .unwrap();
        let attrs = crate::attrs::Attrs::new(group);

        assert_eq!(attrs.int("job-id"), Some(42));
        assert!(
            attrs
                .text("printer-uri")
                .unwrap()
                .ends_with("/printers/HP-8210")
        );

        let requested = attrs.texts("requested-attributes");
        // job-state is the whole point: it is how a finished job's outcome is read.
        for needed in ["job-id", "job-state", "job-name", "job-printer-uri"] {
            assert!(requested.contains(&needed.to_string()), "missing {needed}");
        }
        // The two requests must agree, or a job would decode differently
        // depending on which call produced it.
        let listed = {
            let listing = client.jobs_request(WhichJobs::NotCompleted).unwrap();
            let group = listing
                .attributes()
                .first_of(DelimiterTag::OperationAttributes)
                .unwrap();
            crate::attrs::Attrs::new(group).texts("requested-attributes")
        };
        assert_eq!(requested, listed);
    }

    #[tokio::test]
    #[ignore = "requires a running cupsd"]
    async fn a_job_id_the_daemon_never_heard_of_is_none_not_an_error() {
        let client = CupsClient::local().unwrap();
        // CUPS numbers jobs from 1 upwards; this id will not exist.
        let job = client
            .job("HP-OfficeJet-Pro-8210", 2_000_000_000)
            .await
            .expect("a missing job is not a transport failure");
        assert!(job.is_none());
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
