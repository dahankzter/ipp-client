// SPDX-License-Identifier: MIT OR Apache-2.0

use std::time::Duration;

use crate::{
    device::{Device, decode_devices},
    error::{CupsPkError, Result, translate},
    proxy::MechanismProxy,
};

/// The mechanism's well-known bus name.
///
/// The `org.opensuse` prefix is historical — the project began at openSUSE and
/// is now hosted on freedesktop.org.
pub const MECHANISM_BUS_NAME: &str = "org.opensuse.CupsPkHelper.Mechanism";

/// A connection to the `cups-pk-helper` mechanism.
pub struct CupsPk {
    proxy: MechanismProxy<'static>,
}

impl CupsPk {
    /// Connects to the mechanism on the system bus.
    ///
    /// The service is D-Bus activated, so this starts it on demand; there is
    /// nothing to launch or supervise beforehand.
    pub async fn connect() -> Result<Self> {
        let connection = zbus::Connection::system()
            .await
            .map_err(|e| CupsPkError::Transport(e.to_string()))?;
        Self::build(connection, MECHANISM_BUS_NAME).await
    }

    /// Connects to a named service on the *session* bus.
    ///
    /// This exists so callers can test against a fake serving the same
    /// interface, as this crate's own test suite does. It is not how you talk
    /// to the real mechanism — use [`CupsPk::connect`] for that.
    pub async fn connect_to(bus_name: &str) -> Result<Self> {
        let connection = zbus::Connection::session()
            .await
            .map_err(|e| CupsPkError::Transport(e.to_string()))?;
        Self::build(connection, bus_name).await
    }

    async fn build(connection: zbus::Connection, bus_name: &str) -> Result<Self> {
        let proxy = MechanismProxy::builder(&connection)
            .destination(bus_name.to_string())
            .map_err(|e| CupsPkError::Transport(e.to_string()))?
            .build()
            .await
            .map_err(|e| CupsPkError::Transport(e.to_string()))?;
        Ok(CupsPk { proxy })
    }

    /// Whether the mechanism can be reached at all.
    ///
    /// A front-end uses this to decide, at startup, whether to open in a
    /// read-only mode: `cups-pk-helper` is packaged nearly everywhere but is
    /// not guaranteed to be installed.
    ///
    /// This pings the service rather than merely constructing a proxy.
    /// Constructing one performs no I/O and succeeds even when nothing is
    /// listening, which would report the mechanism as present on a machine
    /// that does not have it installed. `Ping` is part of
    /// `org.freedesktop.DBus.Peer`, so it needs no authorisation and has no
    /// side effects.
    pub async fn is_available() -> bool {
        let Ok(client) = Self::connect().await else {
            return false;
        };
        client.ping().await.is_ok()
    }

    /// Round-trips `org.freedesktop.DBus.Peer.Ping` against the mechanism.
    async fn ping(&self) -> Result<()> {
        let peer = zbus::fdo::PeerProxy::builder(self.proxy.inner().connection())
            .destination(self.proxy.inner().destination().to_owned())
            .map_err(|e| CupsPkError::Transport(e.to_string()))?
            .path(self.proxy.inner().path().to_owned())
            .map_err(|e| CupsPkError::Transport(e.to_string()))?
            .build()
            .await
            .map_err(|e| CupsPkError::Transport(e.to_string()))?;

        peer.ping()
            .await
            .map_err(|e| CupsPkError::Transport(e.to_string()))
    }

    /// Classifies a D-Bus-level failure.
    ///
    /// polkit refusals arrive as an access-denied D-Bus error rather than
    /// through the mechanism's own error string, so they are caught here and
    /// turned into [`CupsPkError::AuthorizationFailed`].
    pub(crate) fn call_failed(e: zbus::Error) -> CupsPkError {
        let text = e.to_string();
        // NotPrivileged is what this mechanism actually raises when polkit
        // refuses; the generic names are kept for other D-Bus peers.
        if text.contains("NotPrivileged")
            || text.contains("AccessDenied")
            || text.contains("not authorized")
        {
            CupsPkError::AuthorizationFailed
        } else {
            CupsPkError::Transport(text)
        }
    }

    /// Sets the system-wide default printer.
    pub async fn printer_set_default(&self, name: &str) -> Result<()> {
        let error = self
            .proxy
            .printer_set_default(name)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }
}

impl CupsPk {
    /// Discovers connected and network printers.
    ///
    /// `timeout` bounds how long CUPS scans; a few seconds is typical. The
    /// mechanism gates this behind its `devices-get` polkit action, so the
    /// caller may be prompted.
    pub async fn devices_get(&self, timeout: Duration, limit: u32) -> Result<Vec<Device>> {
        let (error, raw) = self
            .proxy
            .devices_get(timeout.as_secs() as i32, limit as i32, Vec::new(), Vec::new())
            .await
            .map_err(Self::call_failed)?;

        translate(error)?;
        Ok(decode_devices(raw))
    }
}

impl CupsPk {
    /// Enables or disables a queue. A disabled queue still accepts jobs but
    /// holds them rather than printing.
    pub async fn printer_set_enabled(&self, name: &str, enabled: bool) -> Result<()> {
        let error = self
            .proxy
            .printer_set_enabled(name, enabled)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }

    /// Sets whether a queue accepts new jobs.
    ///
    /// `reason` is shown by CUPS to anyone who tries to print while the queue
    /// is rejecting; pass `""` when there is nothing useful to say.
    pub async fn printer_set_accept_jobs(
        &self,
        name: &str,
        accept: bool,
        reason: &str,
    ) -> Result<()> {
        let error = self
            .proxy
            .printer_set_accept_jobs(name, accept, reason)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }

    /// Sets the human-readable description.
    pub async fn printer_set_info(&self, name: &str, info: &str) -> Result<()> {
        let error = self
            .proxy
            .printer_set_info(name, info)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }

    /// Sets the location string.
    pub async fn printer_set_location(&self, name: &str, location: &str) -> Result<()> {
        let error = self
            .proxy
            .printer_set_location(name, location)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }
}

/// Everything `PrinterAdd` needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrinterSpec {
    /// The queue name. CUPS rejects names containing spaces, `/` or `#`.
    pub name: String,
    /// The device URI, typically one discovered by [`CupsPk::devices_get`].
    pub uri: String,
    /// A PPD name, or `"everywhere"` for driverless IPP.
    pub ppd: String,
    pub info: String,
    pub location: String,
}

impl PrinterSpec {
    /// A driverless printer, using CUPS' built-in IPP Everywhere support.
    ///
    /// This is the path worth defaulting to: any printer advertising IPP
    /// Everywhere works with no downloaded driver at all.
    pub fn driverless(name: impl Into<String>, uri: impl Into<String>) -> Self {
        PrinterSpec {
            name: name.into(),
            uri: uri.into(),
            ppd: "everywhere".to_string(),
            info: String::new(),
            location: String::new(),
        }
    }
}

impl CupsPk {
    /// Adds a printer.
    pub async fn printer_add(&self, spec: &PrinterSpec) -> Result<()> {
        let error = self
            .proxy
            .printer_add(&spec.name, &spec.uri, &spec.ppd, &spec.info, &spec.location)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }

    /// Removes a printer. Its queued jobs go with it.
    pub async fn printer_delete(&self, name: &str) -> Result<()> {
        let error = self
            .proxy
            .printer_delete(name)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }

    /// Renames a printer.
    pub async fn printer_rename(&self, from: &str, to: &str) -> Result<()> {
        let error = self
            .proxy
            .printer_rename(from, to)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }

    /// Sets a default value for one of a printer's options.
    pub async fn printer_add_option_default(
        &self,
        name: &str,
        option: &str,
        values: &[String],
    ) -> Result<()> {
        let error = self
            .proxy
            .printer_add_option_default(name, option, values.to_vec())
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }

    /// Cancels a job, optionally purging its data.
    ///
    /// This is the authorised path, able to cancel jobs the caller does not own
    /// via the mechanism's `job-not-owned-edit` action. `JobCancel` exists too
    /// but is annotated `Deprecated` in the interface, so this binds
    /// `JobCancelPurge`.
    pub async fn job_cancel_purge(&self, job: i32, purge: bool) -> Result<()> {
        let error = self
            .proxy
            .job_cancel_purge(job, purge)
            .await
            .map_err(Self::call_failed)?;
        translate(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_driverless_spec_asks_cups_for_everywhere() {
        // "everywhere" is CUPS' name for its built-in IPP Everywhere driver,
        // which is what makes a modern network printer work with no PPD.
        let spec = PrinterSpec::driverless("Office", "ipp://printer.local/ipp/print");
        assert_eq!(spec.ppd, "everywhere");
        assert_eq!(spec.name, "Office");
        assert_eq!(spec.uri, "ipp://printer.local/ipp/print");
        assert_eq!(spec.info, "");
    }
}
