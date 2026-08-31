// SPDX-License-Identifier: MIT OR Apache-2.0

use ipp::{
    operation::{
        CreateJob, GetJobAttributes, GetJobs, IppOperation, PrintJob, PurgeJobs, SendDocument,
        cups::CupsGetPrinters,
    },
    prelude::*,
};
use tracing::warn;

use std::sync::Mutex;

use crate::attrs::Attrs;
use crate::subscription::{Notification, Notifications, NotifyEvent, Subscription};
use crate::{Class, Document, Error, Job, JobId, Ppd, Printer, Result, lpoptions};

const LOCAL_CUPS: &str = "http://localhost:631";

/// Async client for a CUPS daemon.
pub struct IppClient {
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

impl IppClient {
    /// Connects to the local CUPS daemon as the current user.
    pub fn local() -> Result<Self> {
        Self::with_uri(LOCAL_CUPS, &default_user())
    }

    /// A handle to any IPP printer, by URI.
    ///
    /// The printer needs no relationship to the daemon this client points at:
    /// this is how to reach a driverless network printer directly, with no
    /// CUPS in the path.
    pub fn at(&self, uri: &str) -> Result<IppPrinter<'_>> {
        let parsed: Uri = uri
            .parse()
            .map_err(|e| Error::transport_msg(format!("bad printer uri {uri}: {e}")))?;
        Ok(IppPrinter {
            client: self,
            uri: parsed,
        })
    }

    /// A handle to a queue on the CUPS daemon this client is connected to.
    ///
    /// A convenience over [`IppClient::at`] that knows CUPS' URI convention,
    /// so a queue can be named rather than spelled out.
    pub fn queue(&self, name: &str) -> Result<IppPrinter<'_>> {
        Ok(IppPrinter {
            client: self,
            uri: self.printer_uri(name)?,
        })
    }

    /// Starts building a client with options the plain constructors do not take.
    ///
    /// ```no_run
    /// # fn main() -> ipp_async::Result<()> {
    /// // A printer's own certificate is normally self-signed, so pin it
    /// // rather than turning verification off.
    /// let client = ipp_async::IppClient::builder("ipps://printer.local:631")
    ///     .user("alice")
    ///     .ca_cert(std::fs::read("printer.pem")?)
    ///     .build()?;
    /// # Ok(()) }
    /// ```
    pub fn builder(uri: &str) -> IppClientBuilder {
        IppClientBuilder::new(uri)
    }

    /// Connects to an IPP endpoint, attributing jobs to `user`.
    ///
    /// For TLS trust, credentials or timeouts, use [`IppClient::builder`].
    pub fn with_uri(uri: &str, user: &str) -> Result<Self> {
        let parsed: Uri = uri
            .parse()
            .map_err(|e| Error::transport_msg(format!("bad CUPS uri {uri}: {e}")))?;

        Ok(IppClient {
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
            .map_err(|e| Error::transport_msg(format!("bad printer uri for {name}: {e}")))
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
            .map_err(Error::transport)?;
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

    /// The default-printer cache, surviving a poisoned lock.
    ///
    /// The cache is an optimisation, not state anything depends on, so a panic
    /// in another thread must not turn every later call into a panic of its
    /// own. The stored value is unaffected by whatever went wrong elsewhere.
    fn cache_lock(&self) -> std::sync::MutexGuard<'_, Option<Option<String>>> {
        self.default_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The server's own default queue, via `CUPS-Get-Default`.
    pub(crate) async fn server_default(&self) -> Result<Option<String>> {
        let mut request =
            IppRequestResponse::new(IppVersion::v1_1(), Operation::CupsGetDefault, None)
                .map_err(Error::transport)?;
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(
                "requested-attributes",
                // Infallible: a 12-byte literal is inside the keyword bound.
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
        if let Some(cached) = self.cache_lock().clone() {
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

        *self.cache_lock() = Some(resolved.clone());
        resolved
    }

    /// Forces the next `default_printer` call to resolve again instead of
    /// reusing the cached value.
    pub(crate) fn invalidate_default_printer_cache(&self) {
        *self.cache_lock() = None;
    }

    /// Attributes `Printer::decode` needs. Without this, `CUPS-Get-Printers`
    /// returns every attribute CUPS knows about the queue.
    const PRINTER_ATTRIBUTES: &'static [&'static str] = &[
        "printer-name",
        "printer-uri-supported",
        "device-uri",
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
        // Job option defaults. Measured against cupsd: CUPS-Get-Printers
        // returns all of these per queue, so no second request is needed.
        "media-supported",
        "media-default",
        "sides-supported",
        "sides-default",
        "print-color-mode-supported",
        "print-color-mode-default",
        "print-quality-supported",
        "print-quality-default",
        "output-bin-supported",
        "output-bin-default",
    ];

    /// Lists all queues known to CUPS.
    ///
    /// A CUPS extension (`CUPS-Get-Printers`), not standard IPP: a printer
    /// reached directly knows only about itself and answers this with an
    /// error. Use [`IppClient::at`] for one of those.
    pub async fn printers(&self) -> Result<Vec<Printer>> {
        let default = self.default_printer().await;

        let mut request = CupsGetPrinters::new().into_ipp_request();
        Self::request_attributes(&mut request, Self::PRINTER_ATTRIBUTES)?;

        let resp = self.inner.send(request).await.map_err(Error::transport)?;
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
        "time-at-completed",
        "job-impressions",
        "job-impressions-completed",
    ];

    /// Builds the `Get-Jobs` request. Split out so its shape can be tested.
    pub(crate) fn jobs_request(&self, which: WhichJobs) -> Result<IppRequestResponse> {
        let root: Uri = self
            .base
            .parse()
            .map_err(|e| Error::transport_msg(format!("bad CUPS uri: {e}")))?;

        let op = GetJobs::new(root, Some(self.user.clone())).map_err(Error::transport)?;

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

    /// Lists the printer classes CUPS knows about.
    ///
    /// A CUPS extension (`CUPS-Get-Classes`), not standard IPP.
    ///
    /// Only reading. Creating or changing a class is administrative, and this
    /// crate deliberately holds no authenticated-admin path: that belongs to
    /// `cups-pk-client`, where polkit does the privilege check.
    pub async fn classes(&self) -> Result<Vec<Class>> {
        let mut request =
            IppRequestResponse::new(IppVersion::v1_1(), Operation::CupsGetClasses, None)
                .map_err(Error::transport)?;
        Self::request_attributes(&mut request, &["printer-name", "member-names"])?;

        let resp = self.inner.send(request).await.map_err(Error::transport)?;
        Self::check_status(&resp, "CUPS-Get-Classes")?;

        Ok(resp
            .attributes()
            .groups_of(DelimiterTag::PrinterAttributes)
            .filter_map(|group| match Class::decode(group) {
                Ok(class) => Some(class),
                Err(e) => {
                    tracing::debug!("skipping undecodable class: {e}");
                    None
                }
            })
            .collect())
    }

    /// Lists the drivers CUPS offers, optionally narrowed by a filter.
    ///
    /// A CUPS extension (`CUPS-Get-PPDs`), not standard IPP. Drivers are a
    /// print server's concern; a printer speaks for itself.
    ///
    /// Filtering is done by cupsd, not here. Measured against the live daemon:
    /// a device id narrows 2325 drivers to the 2 that actually match, while
    /// `Make` alone barely narrows anything. Note that `lpinfo`'s `--device-id`
    /// and `--make-and-model` flags do *not* filter - only the IPP operation
    /// does - so verify against `ipptool` rather than `lpinfo`.
    pub async fn ppds(&self, filter: Option<PpdFilter<'_>>) -> Result<Vec<Ppd>> {
        let mut request = IppRequestResponse::new(IppVersion::v1_1(), Operation::CupsGetPPDs, None)
            .map_err(Error::transport)?;

        if let Some(filter) = filter {
            let (name, value) = filter.as_attribute();
            request.attributes_mut().add(
                DelimiterTag::OperationAttributes,
                IppAttribute::with_name(
                    name,
                    IppValue::TextWithoutLanguage(
                        value
                            .try_into()
                            .map_err(|_| Error::decode(name, "filter value too long"))?,
                    ),
                )
                .map_err(|e| Error::decode(name, e.to_string()))?,
            );
        }
        Self::request_attributes(
            &mut request,
            &["ppd-name", "ppd-make-and-model", "ppd-device-id"],
        )?;

        let resp = self.inner.send(request).await.map_err(Error::transport)?;
        Self::check_status(&resp, "CUPS-Get-PPDs")?;

        Ok(resp
            .attributes()
            .groups_of(DelimiterTag::PrinterAttributes)
            .filter_map(|group| match Ppd::decode(group) {
                Ok(ppd) => Some(ppd),
                Err(e) => {
                    tracing::debug!("skipping undecodable driver: {e}");
                    None
                }
            })
            .collect())
    }

    /// Lists the current user's jobs across every queue.
    pub async fn jobs(&self, which: WhichJobs) -> Result<Vec<Job>> {
        let request = self.jobs_request(which)?;

        let resp = self.inner.send(request).await.map_err(Error::transport)?;
        Self::check_status(&resp, "Get-Jobs")?;

        Ok(Self::decode_jobs(&resp))
    }

    /// Builds the `Get-Job-Attributes` request. Split out so its shape can be tested.
    pub(crate) fn job_request(&self, printer: &str, id: JobId) -> Result<IppRequestResponse> {
        let uri = self.printer_uri(printer)?;

        let op =
            GetJobAttributes::new(uri, id, Some(self.user.clone())).map_err(Error::transport)?;

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

        let resp = self.inner.send(request).await.map_err(Error::transport)?;

        // A job aged out of the history is a normal outcome, not a failure.
        if resp.header().status_code() == StatusCode::ClientErrorNotFound {
            return Ok(None);
        }
        Self::check_status(&resp, "Get-Job-Attributes")?;

        Ok(Self::decode_jobs(&resp).into_iter().next())
    }
}

/// One printer, addressed by its own URI.
///
/// Every operation here is standard IPP, so the printer at the other end can
/// be a CUPS queue, a driverless network printer, or anything else that speaks
/// the protocol. Obtain one with [`IppClient::at`] for an arbitrary printer,
/// or [`IppClient::queue`] for a queue on the CUPS daemon this client is
/// connected to.
///
/// ```no_run
/// # async fn example() -> ipp_async::Result<()> {
/// let client = ipp_async::IppClient::local()?;
///
/// // A queue on the local daemon.
/// let queue = client.queue("Office-Laser")?;
/// println!("{:?}", queue.attributes().await?.state);
///
/// // A printer with no CUPS involved at all.
/// let direct = client.at("ipp://printer.local/ipp/print")?;
/// direct.identify(ipp_async::IdentifyAction::Flash).await?;
/// # Ok(()) }
/// ```
pub struct IppPrinter<'a> {
    client: &'a IppClient,
    uri: Uri,
}

impl IppPrinter<'_> {
    /// The printer's URI.
    pub fn uri(&self) -> &Uri {
        &self.uri
    }

    /// Everything the printer reports about itself: state, reasons, supply
    /// levels and the job defaults it advertises.
    pub async fn attributes(&self) -> Result<Printer> {
        let op = GetPrinterAttributes::new(self.uri.clone()).map_err(Error::transport)?;
        let resp = self.client.send(op, "Get-Printer-Attributes").await?;

        resp.attributes()
            .groups_of(DelimiterTag::PrinterAttributes)
            .next()
            .ok_or_else(|| Error::decode("printer-attributes", "no printer group in response"))
            .and_then(Printer::decode)
    }

    /// Submits a document read from a stream, without holding it in memory.
    ///
    /// See [`IppClient::print_stream`] for why this uses `Create-Job` and
    /// `Send-Document` rather than `Print-Job`.
    pub async fn print_stream<R>(
        &self,
        reader: R,
        document_format: Option<&str>,
        job_name: &str,
    ) -> Result<JobId>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        self.client
            .print_stream_at(self.uri.clone(), reader, document_format, job_name)
            .await
    }

    /// Submits a file, letting the printer or daemon type it.
    pub async fn print_file(&self, path: &std::path::Path) -> Result<JobId> {
        let job_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".to_string());
        let file = tokio::fs::File::open(path).await?;
        self.print_stream(file, Some("application/octet-stream"), &job_name)
            .await
    }

    /// Cancels a job on this printer.
    pub async fn cancel_job(&self, id: JobId) -> Result<()> {
        self.job_action(Operation::CancelJob, "Cancel-Job", id)
            .await
    }

    /// Holds a job on this printer.
    pub async fn hold_job(&self, id: JobId) -> Result<()> {
        self.job_action(Operation::HoldJob, "Hold-Job", id).await
    }

    /// Releases a held job on this printer.
    pub async fn release_job(&self, id: JobId) -> Result<()> {
        self.job_action(Operation::ReleaseJob, "Release-Job", id)
            .await
    }

    /// Reprints a finished job, where the document is still held.
    pub async fn restart_job(&self, id: JobId) -> Result<()> {
        self.job_action(Operation::RestartJob, "Restart-Job", id)
            .await
    }

    /// Removes every job from this printer, including other users' jobs.
    ///
    /// Administrative: an unauthorised caller is refused rather than ignored.
    pub async fn purge_jobs(&self) -> Result<()> {
        let op = PurgeJobs::new(self.uri.clone(), Some(self.client.user.as_str()))
            .map_err(Error::transport)?;
        self.client.send(op, "Purge-Jobs").await?;
        Ok(())
    }

    /// Asks whether a job in this format would be accepted, without sending
    /// one. Worth doing before a long upload.
    pub async fn validate(&self, document_format: &str) -> Result<()> {
        self.client
            .validate_job_at(self.uri.clone(), document_format)
            .await
    }

    /// Makes the printer announce itself, so it can be told apart from an
    /// identical one nearby.
    pub async fn identify(&self, action: IdentifyAction) -> Result<()> {
        self.client
            .identify_printer_at(self.uri.clone(), action)
            .await
    }

    /// Stops the printer processing jobs. Queued jobs stay queued.
    ///
    /// Administrative. This is what `cupsdisable` does to a CUPS queue.
    pub async fn pause(&self) -> Result<()> {
        self.printer_action(Operation::PausePrinter, "Pause-Printer")
            .await
    }

    /// Starts a paused printer processing jobs again.
    pub async fn resume(&self) -> Result<()> {
        self.printer_action(Operation::ResumePrinter, "Resume-Printer")
            .await
    }

    /// Opens a job that documents are added to one at a time.
    ///
    /// Use this to send several documents as a single job - a cover sheet and
    /// a report that must not be separated, say. For one document,
    /// [`IppPrinter::print_stream`] does the same thing in fewer round trips.
    ///
    /// The job stays open, and occupies the printer, until
    /// [`IppJob::close`] is called or the last document is marked as such.
    pub async fn create_job(&self, job_name: &str) -> Result<IppJob<'_>> {
        let op = CreateJob::new(self.uri.clone(), Some(job_name)).map_err(Error::transport)?;
        let resp = self.client.send(op, "Create-Job").await?;
        let id = IppClient::decode_job_id(&resp)?;

        Ok(IppJob {
            printer: self,
            id,
            closed: false,
        })
    }

    /// Lists the documents in a job.
    ///
    /// Part of IPP's Document Object extension, which many printers do not
    /// implement - CUPS included. An unsupporting server answers
    /// `ServerErrorOperationNotSupported`, which arrives as an error rather
    /// than an empty list, so "no documents" and "cannot tell you" stay
    /// distinguishable.
    pub async fn documents(&self, job: JobId) -> Result<Vec<Document>> {
        let mut request = self.raw_request(GET_DOCUMENTS)?;
        Self::add_int(&mut request, "job-id", job)?;
        let resp = self.client.send(request, "Get-Documents").await?;
        Ok(crate::subscription::decode_all(
            &resp,
            DelimiterTag::DocumentAttributes,
            Document::decode,
            "document",
        ))
    }

    /// Asks whether a document in this format would be accepted into an
    /// existing job, without sending it.
    ///
    /// The per-document counterpart to [`IppPrinter::validate`], for jobs
    /// being built up with [`IppPrinter::create_job`].
    pub async fn validate_document(&self, job: JobId, document_format: &str) -> Result<()> {
        let mut request = self.raw_request(VALIDATE_DOCUMENT)?;
        Self::add_int(&mut request, "job-id", job)?;
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(
                "document-format",
                IppValue::MimeMediaType(
                    document_format
                        .try_into()
                        .map_err(|_| Error::decode("document-format", "value too long"))?,
                ),
            )
            .map_err(|e| Error::decode("document-format", e.to_string()))?,
        );
        self.client.send(request, "Validate-Document").await?;
        Ok(())
    }

    /// Stops the printer accepting new jobs. Queued work still prints.
    ///
    /// The counterpart to [`IppPrinter::pause`], which stops printing but
    /// keeps accepting. A queue can be in either state independently.
    pub async fn disable(&self) -> Result<()> {
        self.raw_action(DISABLE_PRINTER, "Disable-Printer").await
    }

    /// Starts accepting new jobs again.
    pub async fn enable(&self) -> Result<()> {
        self.raw_action(ENABLE_PRINTER, "Enable-Printer").await
    }

    /// Accepts new jobs but holds them instead of printing them.
    pub async fn hold_new_jobs(&self) -> Result<()> {
        self.raw_action(HOLD_NEW_JOBS, "Hold-New-Jobs").await
    }

    /// Releases jobs held by [`IppPrinter::hold_new_jobs`].
    pub async fn release_held_new_jobs(&self) -> Result<()> {
        self.raw_action(RELEASE_HELD_NEW_JOBS, "Release-Held-New-Jobs")
            .await
    }

    /// Cancels every job this user owns on the printer.
    pub async fn cancel_my_jobs(&self) -> Result<()> {
        self.raw_action(CANCEL_MY_JOBS, "Cancel-My-Jobs").await
    }

    /// Cancels every job on the printer, whoever owns it. Administrative.
    pub async fn cancel_all_jobs(&self) -> Result<()> {
        self.raw_action(CANCEL_JOBS, "Cancel-Jobs").await
    }

    /// Prints a finished job again as a new job.
    ///
    /// Unlike [`IppPrinter::restart_job`], which revives the original, this
    /// creates a copy and leaves the first alone. Both need the printer to
    /// still hold the document.
    pub async fn resubmit_job(&self, id: JobId) -> Result<JobId> {
        let mut request = self.raw_request(RESUBMIT_JOB)?;
        Self::add_int(&mut request, "job-id", id)?;
        let resp = self.client.send(request, "Resubmit-Job").await?;
        IppClient::decode_job_id(&resp)
    }

    /// Changes attributes of a queued job - its priority, or the options it
    /// will print with.
    ///
    /// Attributes are IPP names and values, as the printer advertises them:
    /// `job-priority` as an integer, `media` as a keyword. Only jobs that
    /// have not started can be changed, and only by their owner or an
    /// administrator.
    pub async fn set_job_attributes(
        &self,
        id: JobId,
        attributes: &[(&str, IppValue)],
    ) -> Result<()> {
        let mut request = self.raw_request(SET_JOB_ATTRIBUTES)?;
        Self::add_int(&mut request, "job-id", id)?;
        for (name, value) in attributes {
            request.attributes_mut().add(
                DelimiterTag::JobAttributes,
                IppAttribute::with_name(*name, value.clone())
                    .map_err(|e| Error::decode(*name, e.to_string()))?,
            );
        }
        self.client.send(request, "Set-Job-Attributes").await?;
        Ok(())
    }

    /// Changes the printer's own attributes, such as the defaults new jobs
    /// inherit. Administrative.
    ///
    /// The standard IPP way to set a queue's defaults. Against CUPS the same
    /// thing can be done through `cups-pk-helper`, which is preferable on a
    /// desktop because polkit asks for the password rather than this process.
    pub async fn set_attributes(&self, attributes: &[(&str, IppValue)]) -> Result<()> {
        let mut request = self.raw_request(SET_PRINTER_ATTRIBUTES)?;
        for (name, value) in attributes {
            request.attributes_mut().add(
                DelimiterTag::PrinterAttributes,
                IppAttribute::with_name(*name, value.clone())
                    .map_err(|e| Error::decode(*name, e.to_string()))?,
            );
        }
        self.client.send(request, "Set-Printer-Attributes").await?;
        Ok(())
    }

    /// Asks the printer to remember these events until they are collected.
    ///
    /// The lease is a request, not a guarantee: a server may grant less, and
    /// [`Subscription::lease`] is what it actually gave. `None` asks for no
    /// expiry, which many servers will not grant.
    pub async fn subscribe(
        &self,
        events: &[NotifyEvent],
        lease: Option<std::time::Duration>,
    ) -> Result<Subscription> {
        let mut request = self.raw_request(CREATE_PRINTER_SUBSCRIPTIONS)?;

        // Subscription attributes go in their own group, not with the
        // operation attributes.
        let keywords: Vec<IppValue> = events
            .iter()
            .map(|e| {
                e.keyword()
                    .try_into()
                    .map(IppValue::Keyword)
                    .map_err(|_| Error::decode("notify-events", "keyword too long"))
            })
            .collect::<Result<_>>()?;

        request.attributes_mut().add(
            DelimiterTag::SubscriptionAttributes,
            IppAttribute::with_name("notify-events", IppValue::Array(keywords))
                .map_err(|e| Error::decode("notify-events", e.to_string()))?,
        );
        request.attributes_mut().add(
            DelimiterTag::SubscriptionAttributes,
            // `ippget` means the client collects; the alternative is the
            // server pushing to a URI, which needs somewhere to push to.
            IppAttribute::with_name(
                "notify-pull-method",
                IppValue::Keyword(
                    "ippget"
                        .try_into()
                        .map_err(|_| Error::decode("notify-pull-method", "keyword too long"))?,
                ),
            )
            .map_err(|e| Error::decode("notify-pull-method", e.to_string()))?,
        );
        if let Some(lease) = lease {
            request.attributes_mut().add(
                DelimiterTag::SubscriptionAttributes,
                IppAttribute::with_name(
                    "notify-lease-duration",
                    IppValue::Integer(lease.as_secs() as i32),
                )
                .map_err(|e| Error::decode("notify-lease-duration", e.to_string()))?,
            );
        }

        let resp = self
            .client
            .send(request, "Create-Printer-Subscriptions")
            .await?;
        crate::subscription::decode_all(
            &resp,
            DelimiterTag::SubscriptionAttributes,
            Subscription::decode,
            "subscription",
        )
        .into_iter()
        .next()
        .ok_or_else(|| crate::subscription::missing("notify-subscription-id"))
    }

    /// Lists the subscriptions this user holds on the printer.
    pub async fn subscriptions(&self) -> Result<Vec<Subscription>> {
        let request = self.raw_request(GET_SUBSCRIPTIONS)?;
        let resp = self.client.send(request, "Get-Subscriptions").await?;
        Ok(crate::subscription::decode_all(
            &resp,
            DelimiterTag::SubscriptionAttributes,
            Subscription::decode,
            "subscription",
        ))
    }

    /// Reads one subscription.
    pub async fn subscription(&self, id: i32) -> Result<Subscription> {
        let mut request = self.raw_request(GET_SUBSCRIPTION_ATTRIBUTES)?;
        Self::add_int(&mut request, "notify-subscription-id", id)?;
        let resp = self
            .client
            .send(request, "Get-Subscription-Attributes")
            .await?;
        crate::subscription::decode_all(
            &resp,
            DelimiterTag::SubscriptionAttributes,
            Subscription::decode,
            "subscription",
        )
        .into_iter()
        .next()
        .ok_or_else(|| crate::subscription::missing("notify-subscription-id"))
    }

    /// Collects what has happened since the last collection.
    ///
    /// Whether this returns at once or waits for something to happen is the
    /// server's choice. See [`Notifications::poll_after`].
    pub async fn notifications(&self, subscription_ids: &[i32]) -> Result<Notifications> {
        let mut request = self.raw_request(GET_NOTIFICATIONS)?;
        let ids: Vec<IppValue> = subscription_ids
            .iter()
            .copied()
            .map(IppValue::Integer)
            .collect();
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name("notify-subscription-ids", IppValue::Array(ids))
                .map_err(|e| Error::decode("notify-subscription-ids", e.to_string()))?,
        );
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name("notify-wait", IppValue::Boolean(false))
                .map_err(|e| Error::decode("notify-wait", e.to_string()))?,
        );

        let resp = self.client.send(request, "Get-Notifications").await?;

        let poll_after = resp
            .attributes()
            .groups_of(DelimiterTag::OperationAttributes)
            .next()
            .and_then(|g| Attrs::new(g).int("notify-get-interval"))
            .filter(|i| *i > 0)
            .map(|i| std::time::Duration::from_secs(i as u64));

        Ok(Notifications {
            events: crate::subscription::decode_all(
                &resp,
                DelimiterTag::EventNotificationAttributes,
                Notification::decode,
                "notification",
            ),
            poll_after,
        })
    }

    /// Extends a subscription's life before it expires.
    pub async fn renew_subscription(
        &self,
        id: i32,
        lease: Option<std::time::Duration>,
    ) -> Result<()> {
        let mut request = self.raw_request(RENEW_SUBSCRIPTION)?;
        Self::add_int(&mut request, "notify-subscription-id", id)?;
        if let Some(lease) = lease {
            request.attributes_mut().add(
                DelimiterTag::SubscriptionAttributes,
                IppAttribute::with_name(
                    "notify-lease-duration",
                    IppValue::Integer(lease.as_secs() as i32),
                )
                .map_err(|e| Error::decode("notify-lease-duration", e.to_string()))?,
            );
        }
        self.client.send(request, "Renew-Subscription").await?;
        Ok(())
    }

    /// Ends a subscription now rather than waiting for its lease to run out.
    pub async fn cancel_subscription(&self, id: i32) -> Result<()> {
        let mut request = self.raw_request(CANCEL_SUBSCRIPTION)?;
        Self::add_int(&mut request, "notify-subscription-id", id)?;
        self.client.send(request, "Cancel-Subscription").await?;
        Ok(())
    }

    async fn printer_action(&self, operation: Operation, label: &str) -> Result<()> {
        let mut request =
            IppRequestResponse::new(IppVersion::v1_1(), operation, Some(self.uri.clone()))
                .map_err(Error::transport)?;
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(
                "requesting-user-name",
                IppValue::NameWithoutLanguage(
                    self.client
                        .user
                        .as_str()
                        .try_into()
                        .map_err(|_| Error::decode("requesting-user-name", "name too long"))?,
                ),
            )
            .map_err(|e| Error::decode("requesting-user-name", e.to_string()))?,
        );
        self.client.send(request, label).await?;
        Ok(())
    }

    /// A request for an operation the `ipp` crate's enum has no variant for.
    ///
    /// Built from a placeholder and overwritten, which is the crate's own
    /// escape hatch. Carries the printer URI and the requesting user, as every
    /// one of these operations needs both.
    fn raw_request(&self, opcode: i16) -> Result<IppRequestResponse> {
        let mut request = IppRequestResponse::new(
            IppVersion::v1_1(),
            Operation::GetPrinterAttributes,
            Some(self.uri.clone()),
        )
        .map_err(Error::transport)?;
        request.header_mut().operation_or_status = opcode;

        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(
                "requesting-user-name",
                IppValue::NameWithoutLanguage(
                    self.client
                        .user
                        .as_str()
                        .try_into()
                        .map_err(|_| Error::decode("requesting-user-name", "name too long"))?,
                ),
            )
            .map_err(|e| Error::decode("requesting-user-name", e.to_string()))?,
        );
        Ok(request)
    }

    fn add_int(request: &mut IppRequestResponse, name: &'static str, value: i32) -> Result<()> {
        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(name, IppValue::Integer(value))
                .map_err(|e| Error::decode(name, e.to_string()))?,
        );
        Ok(())
    }

    /// Runs one of the hand-written-opcode operations that needs nothing but a
    /// printer and a user.
    async fn raw_action(&self, opcode: i16, label: &str) -> Result<()> {
        let request = self.raw_request(opcode)?;
        self.client.send(request, label).await?;
        Ok(())
    }

    async fn job_action(&self, operation: Operation, label: &str, id: JobId) -> Result<()> {
        let request = self
            .client
            .job_action_request(operation, self.uri.clone(), id)?;
        self.client.send(request, label).await?;
        Ok(())
    }
}

/// A job open for documents, created by [`IppPrinter::create_job`].
///
/// Dropping one without closing it leaves the job open on the printer, where
/// it will occupy the queue until the printer's own timeout expires. Call
/// [`IppJob::close`], or mark the final document as last.
pub struct IppJob<'a> {
    printer: &'a IppPrinter<'a>,
    id: JobId,
    closed: bool,
}

impl IppJob<'_> {
    /// The job's id, as assigned by the printer.
    pub fn id(&self) -> JobId {
        self.id
    }

    /// Adds a document to the job.
    ///
    /// Set `last` on the final document, which closes the job in the same
    /// request and saves a round trip over calling [`IppJob::close`].
    pub async fn add_document<R>(
        &mut self,
        reader: R,
        document_format: Option<&str>,
        last: bool,
    ) -> Result<()>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        use tokio_util::compat::TokioAsyncReadCompatExt;

        let op = SendDocument::new(
            self.printer.uri.clone(),
            self.id,
            IppPayload::new_async(reader.compat()),
            Some(self.printer.client.user.as_str()),
            document_format,
            last,
        )
        .map_err(Error::transport)?;

        self.printer.client.send(op, "Send-Document").await?;
        if last {
            self.closed = true;
        }
        Ok(())
    }

    /// Closes the job, so the printer stops waiting for documents and starts
    /// printing.
    ///
    /// Not needed when the last document was added with `last` set.
    pub async fn close(mut self) -> Result<JobId> {
        if self.closed {
            return Ok(self.id);
        }

        let mut request = self.printer.client.job_action_request(
            // The ipp crate's Operation enum has no Close-Job, so the opcode
            // is written directly, as for Identify-Printer.
            Operation::CancelJob,
            self.printer.uri.clone(),
            self.id,
        )?;
        request.header_mut().operation_or_status = CLOSE_JOB;

        self.printer.client.send(request, "Close-Job").await?;
        self.closed = true;
        Ok(self.id)
    }

    /// Abandons the job, removing it from the printer.
    pub async fn cancel(self) -> Result<()> {
        self.printer.cancel_job(self.id).await
    }
}

/// Builds a [`IppClient`] with transport options set.
///
/// The plain constructors cover the common case of an unencrypted local
/// daemon. This is for everything else: TLS trust, timeouts, and talking to a
/// printer directly.
pub struct IppClientBuilder {
    uri: String,
    user: Option<String>,
    basic_auth: Option<(String, String)>,
    request_timeout: Option<std::time::Duration>,
    #[cfg(feature = "tls")]
    ca_certs: Vec<Vec<u8>>,
    #[cfg(feature = "tls")]
    accept_invalid_certs: bool,
}

impl IppClientBuilder {
    fn new(uri: &str) -> Self {
        IppClientBuilder {
            uri: uri.to_string(),
            user: None,
            basic_auth: None,
            request_timeout: None,
            #[cfg(feature = "tls")]
            ca_certs: Vec::new(),
            #[cfg(feature = "tls")]
            accept_invalid_certs: false,
        }
    }

    /// The name sent as `requesting-user-name`.
    ///
    /// Defaults to `$USER`, or `anonymous` where that is unset. CUPS uses it
    /// to decide which jobs are yours.
    pub fn user(mut self, user: &str) -> Self {
        self.user = Some(user.to_string());
        self
    }

    /// How long a single request may take, including the upload of a document.
    ///
    /// There is no timeout by default, because a large print job legitimately
    /// takes a long time and a deadline that cuts one off is worse than none.
    /// Set one for interactive requests, where an unreachable printer would
    /// otherwise hang the caller indefinitely.
    pub fn request_timeout(mut self, timeout: std::time::Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    /// Sends HTTP Basic credentials with every request.
    ///
    /// This is what CUPS asks for by default: an administrative operation on
    /// an unauthenticated connection is refused with `401`, and the daemon
    /// replies `WWW-Authenticate: Basic realm="CUPS"`.
    ///
    /// Basic authentication transmits the password in a trivially reversible
    /// encoding, so anything that can read the connection can read the
    /// password. Over `ipps://` that is fine, because TLS covers it. Over
    /// plain `ipp://` to anything but a loopback address it is not, and this
    /// logs a warning at build time saying so.
    ///
    /// Only Basic is available: the underlying IPP transport does not
    /// implement Digest or Negotiate, so a daemon configured to require either
    /// cannot be satisfied from here.
    pub fn basic_auth(mut self, user: &str, password: &str) -> Self {
        self.basic_auth = Some((user.to_string(), password.to_string()));
        self
    }

    /// Trusts an additional root certificate, in PEM or DER form.
    ///
    /// This is the right way to reach a printer over `ipps://`. Printers, and
    /// CUPS itself, ship self-signed certificates that no public root will
    /// vouch for, so verification fails against them by default with
    /// `UnknownIssuer`. Pinning the certificate keeps verification on.
    #[cfg(feature = "tls")]
    pub fn ca_cert(mut self, certificate: impl AsRef<[u8]>) -> Self {
        self.ca_certs.push(certificate.as_ref().to_vec());
        self
    }

    /// Accepts any certificate, valid or not.
    ///
    /// This disables the check that the peer is who it claims to be, so an
    /// `ipps://` connection becomes encrypted but unauthenticated and offers
    /// no protection against interception. Prefer [`IppClientBuilder::ca_cert`].
    ///
    /// It exists because discovering a printer's certificate is sometimes
    /// impractical, and because a caller who has decided to accept that risk
    /// on a trusted network should not have to abandon this crate to do it.
    #[cfg(feature = "tls")]
    pub fn danger_accept_invalid_certs(mut self, accept: bool) -> Self {
        self.accept_invalid_certs = accept;
        self
    }

    /// Builds the client.
    pub fn build(self) -> Result<IppClient> {
        let parsed: Uri = self
            .uri
            .parse()
            .map_err(|e| Error::transport_msg(format!("bad uri {}: {e}", self.uri)))?;

        let mut builder = AsyncIppClient::builder(parsed.clone());
        if let Some(timeout) = self.request_timeout {
            builder = builder.request_timeout(timeout);
        }
        if let Some((user, password)) = &self.basic_auth {
            if !is_confidential(&parsed) {
                tracing::warn!(
                    uri = %self.uri,
                    "sending Basic credentials over an unencrypted connection to a \
                     non-loopback host; anything on the path can read the password"
                );
            }
            builder = builder.basic_auth(user, password);
        }
        #[cfg(feature = "tls")]
        {
            for certificate in self.ca_certs {
                builder = builder.ca_cert(certificate);
            }
            if self.accept_invalid_certs {
                builder = builder.ignore_tls_errors(true);
            }
        }

        let user = self.user.unwrap_or_else(default_user);
        Ok(IppClient {
            inner: builder.build(),
            base: self.uri.trim_end_matches('/').to_string(),
            user,
            default_cache: Mutex::new(None),
        })
    }
}

/// Whether credentials sent to this URI are protected from onlookers, either
/// by TLS or by never leaving the machine.
fn is_confidential(uri: &Uri) -> bool {
    if uri.scheme_str() == Some("https") || uri.scheme_str() == Some("ipps") {
        return true;
    }
    matches!(
        uri.host(),
        Some("localhost" | "127.0.0.1" | "::1" | "[::1]")
    )
}

/// The user CUPS attributes jobs to when none is given.
fn default_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "anonymous".to_string())
}

/// IPP operations the `ipp` crate's `Operation` enum predates.
///
/// Values from RFC 8011 and the IANA registry, cross-checked against libcups'
/// `ipp.h`. Proposed upstream as ancwrd1/ipp.rs#68; until that lands and is
/// released these go on the header directly.
const CREATE_PRINTER_SUBSCRIPTIONS: i16 = 0x0016;
const GET_SUBSCRIPTION_ATTRIBUTES: i16 = 0x0018;
const GET_SUBSCRIPTIONS: i16 = 0x0019;
const RENEW_SUBSCRIPTION: i16 = 0x001A;
const CANCEL_SUBSCRIPTION: i16 = 0x001B;
const GET_NOTIFICATIONS: i16 = 0x001C;
const SET_PRINTER_ATTRIBUTES: i16 = 0x0013;
const SET_JOB_ATTRIBUTES: i16 = 0x0014;
const ENABLE_PRINTER: i16 = 0x0022;
const DISABLE_PRINTER: i16 = 0x0023;
const HOLD_NEW_JOBS: i16 = 0x0025;
const RELEASE_HELD_NEW_JOBS: i16 = 0x0026;
const CANCEL_JOBS: i16 = 0x0038;
const CANCEL_MY_JOBS: i16 = 0x0039;
const RESUBMIT_JOB: i16 = 0x003A;
const GET_DOCUMENTS: i16 = 0x0035;
const VALIDATE_DOCUMENT: i16 = 0x003D;
const CLOSE_JOB: i16 = 0x003B;
const IDENTIFY_PRINTER: i16 = 0x003C;

/// How a printer should announce itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdentifyAction {
    /// Flash a light.
    Flash,
    /// Make a noise.
    Sound,
    /// Show a message on the printer's own display.
    Display(String),
}

impl IdentifyAction {
    fn keyword(&self) -> &'static str {
        match self {
            IdentifyAction::Flash => "flash",
            IdentifyAction::Sound => "sound",
            IdentifyAction::Display(_) => "display",
        }
    }
}

/// How to narrow a driver search. cupsd does the matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PpdFilter<'a> {
    /// An IEEE-1284 device id, as reported by device discovery. The most
    /// precise filter, and the one worth trying first.
    DeviceId(&'a str),
    /// A description such as `HP OfficeJet Pro 8210`.
    MakeAndModel(&'a str),
    /// A manufacturer. Rarely narrow enough to be useful on its own.
    Make(&'a str),
}

impl<'a> PpdFilter<'a> {
    fn as_attribute(self) -> (&'static str, &'a str) {
        match self {
            PpdFilter::DeviceId(v) => ("ppd-device-id", v),
            PpdFilter::MakeAndModel(v) => ("ppd-make-and-model", v),
            PpdFilter::Make(v) => ("ppd-make", v),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Which jobs [`IppClient::jobs`] should return.
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

impl IppClient {
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
        uri: Uri,
        id: JobId,
    ) -> Result<IppRequestResponse> {
        let mut request = IppRequestResponse::new(IppVersion::v1_1(), operation, Some(uri))
            .map_err(Error::transport)?;

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
        self.job_action_at(operation, label, self.printer_uri(printer)?, id)
            .await
    }

    /// The same, against a queue identified by URI rather than by name.
    async fn job_action_at(
        &self,
        operation: Operation,
        label: &str,
        uri: Uri,
        id: JobId,
    ) -> Result<()> {
        let request = self.job_action_request(operation, uri, id)?;
        self.send(request, label).await?;
        Ok(())
    }

    /// Cancels one of the current user's jobs.
    pub async fn cancel_job(&self, printer: &str, id: JobId) -> Result<()> {
        self.job_action(Operation::CancelJob, "Cancel-Job", printer, id)
            .await
    }

    /// Holds one of the current user's jobs.
    pub async fn hold_job(&self, printer: &str, id: JobId) -> Result<()> {
        self.job_action(Operation::HoldJob, "Hold-Job", printer, id)
            .await
    }

    /// Releases one of the current user's held jobs.
    pub async fn release_job(&self, printer: &str, id: JobId) -> Result<()> {
        self.job_action(Operation::ReleaseJob, "Release-Job", printer, id)
            .await
    }

    /// Reprints a job that has already finished.
    ///
    /// Only works while CUPS still holds the document. `PreserveJobFiles` is
    /// off by default, so a job whose files have been purged cannot be
    /// restarted and the daemon says so.
    pub async fn restart_job(&self, printer: &str, id: JobId) -> Result<()> {
        self.job_action(Operation::RestartJob, "Restart-Job", printer, id)
            .await
    }

    /// Moves a job to another queue.
    ///
    /// A CUPS extension (`CUPS-Move-Job`), not standard IPP, and meaningful
    /// only where there is more than one queue to move between.
    ///
    /// The destination is named as a queue on this daemon, since moving a job
    /// to a printer the daemon does not know about is not a thing CUPS can do.
    pub async fn move_job(&self, printer: &str, id: JobId, destination: &str) -> Result<()> {
        let request = self.move_job_request(printer, id, destination)?;
        self.send(request, "CUPS-Move-Job").await?;
        Ok(())
    }

    /// Builds the `CUPS-Move-Job` request. Split out so its shape can be tested.
    pub(crate) fn move_job_request(
        &self,
        printer: &str,
        id: JobId,
        destination: &str,
    ) -> Result<IppRequestResponse> {
        let mut request =
            self.job_action_request(Operation::CupsMoveJob, self.printer_uri(printer)?, id)?;

        // The destination goes in the job group, not the operation group: it
        // is an attribute of the job being changed, not of the request.
        request.attributes_mut().add(
            DelimiterTag::JobAttributes,
            IppAttribute::with_name(
                "job-printer-uri",
                IppValue::Uri(
                    self.printer_uri(destination)?
                        .to_string()
                        .try_into()
                        .map_err(|_| Error::decode("job-printer-uri", "uri too long"))?,
                ),
            )
            .map_err(|e| Error::decode("job-printer-uri", e.to_string()))?,
        );

        Ok(request)
    }

    /// Removes every job from a queue, including other users' jobs.
    ///
    /// Needs administrative rights on the daemon. Unauthenticated callers get
    /// a `client-error-not-authorized` back rather than a silent no-op.
    pub async fn purge_jobs(&self, printer: &str) -> Result<()> {
        let op = PurgeJobs::new(self.printer_uri(printer)?, Some(self.user.as_str()))
            .map_err(Error::transport)?;
        self.send(op, "Purge-Jobs").await?;
        Ok(())
    }

    /// Makes a printer announce itself, so it can be told apart from an
    /// identical one on the next desk.
    ///
    /// Whether anything happens is up to the hardware: a printer advertises
    /// what it can do in `identify-actions-supported`, and one that cannot do
    /// the requested action says so rather than pretending.
    pub async fn identify_printer(&self, printer: &str, action: IdentifyAction) -> Result<()> {
        let request = self.identify_printer_request(self.printer_uri(printer)?, action)?;
        self.send(request, "Identify-Printer").await?;
        Ok(())
    }

    pub(crate) async fn identify_printer_at(&self, uri: Uri, action: IdentifyAction) -> Result<()> {
        let request = self.identify_printer_request(uri, action)?;
        self.send(request, "Identify-Printer").await?;
        Ok(())
    }

    /// Builds the `Identify-Printer` request. Split out so its shape can be tested.
    pub(crate) fn identify_printer_request(
        &self,
        uri: Uri,
        action: IdentifyAction,
    ) -> Result<IppRequestResponse> {
        // The ipp crate's Operation enum predates Identify-Printer, so the
        // opcode goes on the header directly. `header_mut` is the crate's own
        // escape hatch for this; the placeholder operation is overwritten
        // before the request is ever serialised.
        let mut request = IppRequestResponse::new(
            IppVersion::v1_1(),
            Operation::GetPrinterAttributes,
            Some(uri),
        )
        .map_err(Error::transport)?;
        request.header_mut().operation_or_status = IDENTIFY_PRINTER;

        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(
                "requesting-user-name",
                IppValue::NameWithoutLanguage(
                    self.user
                        .as_str()
                        .try_into()
                        .map_err(|_| Error::decode("requesting-user-name", "name too long"))?,
                ),
            )
            .map_err(|e| Error::decode("requesting-user-name", e.to_string()))?,
        );

        request.attributes_mut().add(
            DelimiterTag::OperationAttributes,
            IppAttribute::with_name(
                "identify-actions",
                IppValue::Keyword(
                    action
                        .keyword()
                        .try_into()
                        .map_err(|_| Error::decode("identify-actions", "keyword too long"))?,
                ),
            )
            .map_err(|e| Error::decode("identify-actions", e.to_string()))?,
        );

        if let IdentifyAction::Display(message) = &action {
            request.attributes_mut().add(
                DelimiterTag::OperationAttributes,
                IppAttribute::with_name(
                    "message",
                    IppValue::TextWithoutLanguage(
                        message
                            .as_str()
                            .try_into()
                            .map_err(|_| Error::decode("message", "message too long"))?,
                    ),
                )
                .map_err(|e| Error::decode("message", e.to_string()))?,
            );
        }

        Ok(request)
    }

    /// Asks whether a job with these characteristics would be accepted,
    /// without submitting one.
    ///
    /// Worth doing before a long upload: it turns "the transfer failed after
    /// two minutes" into an answer before the transfer starts.
    pub async fn validate_job(&self, printer: &str, document_format: &str) -> Result<()> {
        self.validate_job_at(self.printer_uri(printer)?, document_format)
            .await
    }

    pub(crate) async fn validate_job_at(&self, uri: Uri, document_format: &str) -> Result<()> {
        let request = self.validate_job_request(uri, document_format)?;
        self.send(request, "Validate-Job").await?;
        Ok(())
    }

    /// Builds the `Validate-Job` request. Split out so its shape can be tested.
    pub(crate) fn validate_job_request(
        &self,
        uri: Uri,
        document_format: &str,
    ) -> Result<IppRequestResponse> {
        let mut request =
            IppRequestResponse::new(IppVersion::v1_1(), Operation::ValidateJob, Some(uri))
                .map_err(Error::transport)?;

        for (name, value) in [
            (
                "requesting-user-name",
                IppValue::NameWithoutLanguage(
                    self.user
                        .as_str()
                        .try_into()
                        .map_err(|_| Error::decode("requesting-user-name", "name too long"))?,
                ),
            ),
            (
                "document-format",
                IppValue::MimeMediaType(
                    document_format
                        .try_into()
                        .map_err(|_| Error::decode("document-format", "value too long"))?,
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
}

use crate::{
    PrinterEvent,
    events::{POLL_INTERVAL, Snapshot, backoff_after},
};
use futures::Stream;
use ipp::operation::GetPrinterAttributes;
use std::time::Duration;

impl IppClient {
    /// Reads one queue's attributes, addressing it by its full IPP URI.
    /// Reads a printer by URI.
    ///
    /// Equivalent to `client.at(uri)?.attributes()`, kept because reading a
    /// printer's own advertisement is a common one-shot.
    pub async fn printer_at(&self, uri: &str) -> Result<Printer> {
        let parsed: Uri = uri
            .parse()
            .map_err(|e| Error::transport_msg(format!("bad printer uri {uri}: {e}")))?;
        let op = GetPrinterAttributes::new(parsed).map_err(Error::transport)?;
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

impl IppClient {
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

impl IppClient {
    /// Reads the job id CUPS assigned from a `Print-Job` reply.
    pub(crate) fn decode_job_id(resp: &IppRequestResponse) -> Result<JobId> {
        resp.attributes()
            .groups_of(DelimiterTag::JobAttributes)
            .next()
            .and_then(|g| crate::attrs::Attrs::new(g).int("job-id"))
            .ok_or_else(|| Error::decode("job-id", "absent from the Print-Job reply"))
    }

    /// Submits a file to a queue.
    ///
    /// The document format is left to CUPS: `application/octet-stream` makes it
    /// auto-type the file, which handles PDF, PostScript, plain text and images
    /// without this crate having to guess from an extension.
    ///
    /// Printing as yourself needs no authorisation, which is why this lives
    /// here rather than behind the polkit mechanism.
    ///
    /// The file is read into memory before sending. Print jobs are typically
    /// small; a very large document would be held in memory for the duration of
    /// the request.
    /// Submits already-prepared bytes to a queue.
    ///
    /// `document_format` is an IPP MIME type. Pass
    /// `application/octet-stream` to let CUPS auto-type the content, or a
    /// specific type when the caller has produced the document itself and
    /// knows better — rendering text to PDF, for instance, so CUPS' own text
    /// filter never runs.
    pub async fn print_bytes(
        &self,
        printer: &str,
        bytes: Vec<u8>,
        document_format: &str,
        job_name: &str,
    ) -> Result<JobId> {
        let op = PrintJob::new(
            self.printer_uri(printer)?,
            IppPayload::new(std::io::Cursor::new(bytes)),
            Some(self.user.as_str()),
            Some(job_name),
            Some(document_format),
        )
        .map_err(Error::transport)?;

        let resp = self.send(op, "Print-Job").await?;
        Self::decode_job_id(&resp)
    }

    /// Submits a document read from a stream, without holding it in memory.
    ///
    /// Uses `Create-Job` followed by `Send-Document` rather than `Print-Job`,
    /// which is what allows the document to be streamed: `Print-Job` carries
    /// its payload in the same request, so the whole thing has to exist before
    /// the request can be built.
    ///
    /// Takes tokio's `AsyncRead` rather than the futures-io one the underlying
    /// IPP crate wants. Every caller here is a tokio program, and making each
    /// of them wrap their reader to satisfy a dependency's choice would be
    /// leaking an implementation detail into the API.
    pub async fn print_stream<R>(
        &self,
        printer: &str,
        reader: R,
        document_format: Option<&str>,
        job_name: &str,
    ) -> Result<JobId>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        self.print_stream_at(
            self.printer_uri(printer)?,
            reader,
            document_format,
            job_name,
        )
        .await
    }

    pub(crate) async fn print_stream_at<R>(
        &self,
        uri: Uri,
        reader: R,
        document_format: Option<&str>,
        job_name: &str,
    ) -> Result<JobId>
    where
        R: tokio::io::AsyncRead + Send + Unpin + 'static,
    {
        use tokio_util::compat::TokioAsyncReadCompatExt;

        let cleanup_uri = uri.clone();
        let create = CreateJob::new(uri.clone(), Some(job_name)).map_err(Error::transport)?;
        let resp = self.send(create, "Create-Job").await?;
        let job = Self::decode_job_id(&resp)?;

        let send = SendDocument::new(
            uri,
            job,
            IppPayload::new_async(reader.compat()),
            Some(self.user.as_str()),
            document_format,
            true,
        )
        .map_err(Error::transport)?;

        match self.send(send, "Send-Document").await {
            Ok(_) => Ok(job),
            Err(e) => {
                // Create-Job already queued a job. Left alone it would sit
                // there indefinitely holding no document, so take it back out
                // before reporting the failure.
                if let Err(cleanup) = self
                    .job_action_at(Operation::CancelJob, "Cancel-Job", cleanup_uri, job)
                    .await
                {
                    warn!("could not cancel job {job} after Send-Document failed: {cleanup}");
                }
                Err(e)
            }
        }
    }

    /// Submits a file to a queue, letting CUPS auto-type it.
    ///
    /// The file is read into memory before sending. Print jobs are typically
    /// small; a very large document would be held in memory for the duration
    /// of the request.
    pub async fn print_file(&self, printer: &str, path: &std::path::Path) -> Result<JobId> {
        let job_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "document".to_string());

        let file = tokio::fs::File::open(path).await?;
        self.print_stream(printer, file, Some("application/octet-stream"), &job_name)
            .await
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
        let printers = IppClient::decode_printers(&fixture(), None);
        assert!(
            !printers.is_empty(),
            "fixture should contain at least one printer"
        );
        assert!(printers.iter().all(|p| !p.name.is_empty()));
        assert!(
            printers.iter().all(|p| !p.is_default),
            "no default was supplied"
        );
    }

    #[test]
    fn the_named_default_is_the_one_marked_default() {
        let all = IppClient::decode_printers(&fixture(), None);
        let name = all.first().expect("fixture has a printer").name.clone();

        let marked = IppClient::decode_printers(&fixture(), Some(&name));
        assert_eq!(marked.iter().filter(|p| p.is_default).count(), 1);
        assert!(marked.iter().find(|p| p.is_default).unwrap().name == name);
    }

    #[test]
    fn a_submitted_job_reports_the_id_cups_assigned() {
        // The caller needs the id to follow the job in the queue afterwards.
        let mut resp =
            IppRequestResponse::new_response(IppVersion::v1_1(), StatusCode::SuccessfulOk, 1)
                .unwrap();
        resp.attributes_mut().add(
            DelimiterTag::JobAttributes,
            IppAttribute::with_name("job-id", IppValue::Integer(42)).unwrap(),
        );

        assert_eq!(IppClient::decode_job_id(&resp).unwrap(), 42);
    }

    #[test]
    fn a_reply_without_a_job_id_is_a_decode_error() {
        // Rather than inventing an id the caller would then fail to find.
        let resp =
            IppRequestResponse::new_response(IppVersion::v1_1(), StatusCode::SuccessfulOk, 1)
                .unwrap();
        let err = IppClient::decode_job_id(&resp).unwrap_err();
        assert!(err.to_string().contains("job-id"));
    }

    #[test]
    fn success_status_passes_the_check() {
        assert!(IppClient::check_status(&fixture(), "CUPS-Get-Printers").is_ok());
    }

    #[test]
    fn error_status_names_the_operation() {
        let mut resp = fixture();
        // 0x0400 = client-error-bad-request
        resp.header_mut().operation_or_status = 0x0400;
        let err = IppClient::check_status(&resp, "CUPS-Get-Printers").unwrap_err();
        assert!(err.to_string().contains("CUPS-Get-Printers"));
    }

    #[tokio::test]
    #[ignore = "requires a running cupsd"]
    async fn lists_printers_from_the_live_daemon() {
        let client = IppClient::local().unwrap();
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
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();

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
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();
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
        assert!(IppClient::decode_jobs(&fixture()).is_empty());
    }

    #[tokio::test]
    #[ignore = "requires a running cupsd"]
    async fn lists_jobs_from_the_live_daemon() {
        let client = IppClient::local().unwrap();
        let jobs = client.jobs(WhichJobs::NotCompleted).await.unwrap();
        assert!(jobs.iter().all(|j| j.state.is_active()));
    }

    #[test]
    fn the_job_request_asks_for_every_attribute_the_decoder_needs() {
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();
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
        let client = IppClient::local().unwrap();
        // CUPS numbers jobs from 1 upwards; this id will not exist.
        let job = client
            .job("HP-OfficeJet-Pro-8210", 2_000_000_000)
            .await
            .expect("a missing job is not a transport failure");
        assert!(job.is_none());
    }

    #[test]
    fn move_job_names_the_destination_in_the_job_group() {
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client
            .move_job_request("HP-8210", 42, "Office-Laser")
            .unwrap();

        assert_eq!(
            request.header().operation_or_status,
            Operation::CupsMoveJob as i16
        );

        // The source printer and the job are operation attributes; the
        // destination is an attribute of the job itself.
        let operation = crate::attrs::Attrs::new(
            request
                .attributes()
                .first_of(DelimiterTag::OperationAttributes)
                .unwrap(),
        );
        assert_eq!(operation.int("job-id"), Some(42));
        assert!(
            operation
                .text("printer-uri")
                .unwrap()
                .ends_with("/printers/HP-8210")
        );

        let job = crate::attrs::Attrs::new(
            request
                .attributes()
                .first_of(DelimiterTag::JobAttributes)
                .unwrap(),
        );
        assert!(
            job.text("job-printer-uri")
                .unwrap()
                .ends_with("/printers/Office-Laser"),
            "the destination queue is where the job is moved to"
        );
    }

    #[test]
    fn loopback_and_tls_are_confidential_but_plain_remote_is_not() {
        let confidential = |u: &str| super::is_confidential(&u.parse().unwrap());

        // Never leaves the machine.
        assert!(confidential("http://localhost:631"));
        assert!(confidential("http://127.0.0.1:631"));
        // Encrypted on the wire.
        assert!(confidential("ipps://printer.example:631"));
        assert!(confidential("https://printer.example:631"));
        // A password here is readable by anything on the path.
        assert!(!confidential("http://printer.example:631"));
        assert!(!confidential("ipp://192.168.1.10:631"));
    }

    #[test]
    fn identify_printer_uses_the_right_opcode_not_the_placeholder() {
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client
            .identify_printer_request(
                client.printer_uri("HP-8210").unwrap(),
                IdentifyAction::Flash,
            )
            .unwrap();

        // Built from GetPrinterAttributes and overwritten. If the overwrite
        // were ever dropped this would silently query attributes instead.
        assert_eq!(request.header().operation_or_status, 0x003C);
        assert_ne!(
            request.header().operation_or_status,
            Operation::GetPrinterAttributes as i16
        );

        let attrs = crate::attrs::Attrs::new(
            request
                .attributes()
                .first_of(DelimiterTag::OperationAttributes)
                .unwrap(),
        );
        assert_eq!(attrs.text("identify-actions").as_deref(), Some("flash"));
        assert_eq!(attrs.text("message"), None, "flash carries no message");
    }

    #[test]
    fn identifying_by_display_carries_the_message() {
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client
            .identify_printer_request(
                client.printer_uri("HP-8210").unwrap(),
                IdentifyAction::Display("collect your pages".into()),
            )
            .unwrap();

        let attrs = crate::attrs::Attrs::new(
            request
                .attributes()
                .first_of(DelimiterTag::OperationAttributes)
                .unwrap(),
        );
        assert_eq!(attrs.text("identify-actions").as_deref(), Some("display"));
        assert_eq!(
            attrs.text("message").as_deref(),
            Some("collect your pages"),
            "display without a message would say nothing"
        );
    }

    #[test]
    fn validate_job_asks_about_a_document_format() {
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client
            .validate_job_request(client.printer_uri("HP-8210").unwrap(), "application/pdf")
            .unwrap();

        assert_eq!(
            request.header().operation_or_status,
            Operation::ValidateJob as i16
        );
        let attrs = crate::attrs::Attrs::new(
            request
                .attributes()
                .first_of(DelimiterTag::OperationAttributes)
                .unwrap(),
        );
        assert_eq!(
            attrs.text("document-format").as_deref(),
            Some("application/pdf")
        );
        assert_eq!(
            attrs.text("requesting-user-name").as_deref(),
            Some("tester")
        );
    }

    #[test]
    fn job_action_request_carries_id_user_and_operation() {
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client
            .job_action_request(
                Operation::HoldJob,
                client.printer_uri("HP-8210").unwrap(),
                42,
            )
            .unwrap();

        assert_eq!(
            request.header().operation_or_status,
            Operation::HoldJob as i16
        );

        let group = request
            .attributes()
            .first_of(DelimiterTag::OperationAttributes)
            .unwrap();
        let attrs = crate::attrs::Attrs::new(group);
        assert_eq!(attrs.int("job-id"), Some(42));
        assert_eq!(
            attrs.text("requesting-user-name").as_deref(),
            Some("tester")
        );
        assert!(
            attrs
                .text("printer-uri")
                .unwrap()
                .ends_with("/printers/HP-8210")
        );
    }

    #[test]
    fn release_uses_its_own_operation_code() {
        let client = IppClient::with_uri("http://localhost:631", "tester").unwrap();
        let request = client
            .job_action_request(
                Operation::ReleaseJob,
                client.printer_uri("HP-8210").unwrap(),
                7,
            )
            .unwrap();
        assert_eq!(
            request.header().operation_or_status,
            Operation::ReleaseJob as i16
        );
    }
}
