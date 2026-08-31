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

use crate::{Class, Error, Job, JobId, Ppd, Printer, Result, lpoptions};

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
        Self::with_uri(LOCAL_CUPS, &default_user())
    }

    /// Starts building a client with options the plain constructors do not take.
    ///
    /// ```no_run
    /// # fn main() -> cups_client::Result<()> {
    /// // A printer's own certificate is normally self-signed, so pin it
    /// // rather than turning verification off.
    /// let client = cups_client::CupsClient::builder("ipps://printer.local:631")
    ///     .user("alice")
    ///     .ca_cert(std::fs::read("printer.pem")?)
    ///     .build()?;
    /// # Ok(()) }
    /// ```
    pub fn builder(uri: &str) -> CupsClientBuilder {
        CupsClientBuilder::new(uri)
    }

    pub fn with_uri(uri: &str, user: &str) -> Result<Self> {
        let parsed: Uri = uri
            .parse()
            .map_err(|e| Error::transport_msg(format!("bad CUPS uri {uri}: {e}")))?;

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

    /// The server's own default queue, via `CUPS-Get-Default`.
    pub(crate) async fn server_default(&self) -> Result<Option<String>> {
        let mut request =
            IppRequestResponse::new(IppVersion::v1_1(), Operation::CupsGetDefault, None)
                .map_err(Error::transport)?;
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

/// Builds a [`CupsClient`] with transport options set.
///
/// The plain constructors cover the common case of an unencrypted local
/// daemon. This is for everything else: TLS trust, timeouts, and talking to a
/// printer directly.
pub struct CupsClientBuilder {
    uri: String,
    user: Option<String>,
    request_timeout: Option<std::time::Duration>,
    #[cfg(feature = "tls")]
    ca_certs: Vec<Vec<u8>>,
    #[cfg(feature = "tls")]
    accept_invalid_certs: bool,
}

impl CupsClientBuilder {
    fn new(uri: &str) -> Self {
        CupsClientBuilder {
            uri: uri.to_string(),
            user: None,
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
    /// no protection against interception. Prefer [`CupsClientBuilder::ca_cert`].
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
    pub fn build(self) -> Result<CupsClient> {
        let parsed: Uri = self
            .uri
            .parse()
            .map_err(|e| Error::transport_msg(format!("bad uri {}: {e}", self.uri)))?;

        let mut builder = AsyncIppClient::builder(parsed);
        if let Some(timeout) = self.request_timeout {
            builder = builder.request_timeout(timeout);
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
        Ok(CupsClient {
            inner: builder.build(),
            base: self.uri.trim_end_matches('/').to_string(),
            user,
            default_cache: Mutex::new(None),
        })
    }
}

/// The user CUPS attributes jobs to when none is given.
fn default_user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "anonymous".to_string())
}

/// IPP `Identify-Printer`, which the `ipp` crate's `Operation` enum predates.
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

    /// The same, against a printer identified by URI.
    pub async fn identify_printer_at(&self, uri: Uri, action: IdentifyAction) -> Result<()> {
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

    /// The same, against a printer identified by URI.
    ///
    /// Pairs with [`CupsClient::print_stream_at`]: ask first, then stream, so a
    /// remote printer rejects the format before the upload rather than after.
    pub async fn validate_job_at(&self, uri: Uri, document_format: &str) -> Result<()> {
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

impl CupsClient {
    /// Reads one queue's attributes, addressing it by its full IPP URI.
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

impl CupsClient {
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

    /// The same, against a printer identified by URI rather than by queue name.
    ///
    /// A queue on this daemon is addressable either way, but an IPP printer
    /// reached directly - a driverless network printer, or one on another host
    /// - has no local queue name to give.
    pub async fn print_stream_at<R>(
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
        let printers = CupsClient::decode_printers(&fixture(), None);
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
        let all = CupsClient::decode_printers(&fixture(), None);
        let name = all.first().expect("fixture has a printer").name.clone();

        let marked = CupsClient::decode_printers(&fixture(), Some(&name));
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

        assert_eq!(CupsClient::decode_job_id(&resp).unwrap(), 42);
    }

    #[test]
    fn a_reply_without_a_job_id_is_a_decode_error() {
        // Rather than inventing an id the caller would then fail to find.
        let resp =
            IppRequestResponse::new_response(IppVersion::v1_1(), StatusCode::SuccessfulOk, 1)
                .unwrap();
        let err = CupsClient::decode_job_id(&resp).unwrap_err();
        assert!(err.to_string().contains("job-id"));
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
    fn move_job_names_the_destination_in_the_job_group() {
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
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
    fn identify_printer_uses_the_right_opcode_not_the_placeholder() {
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
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
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
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
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
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
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
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
        let client = CupsClient::with_uri("http://localhost:631", "tester").unwrap();
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
