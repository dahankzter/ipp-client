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
    pub async fn is_available() -> bool {
        Self::connect().await.is_ok()
    }

    /// Classifies a D-Bus-level failure.
    ///
    /// polkit refusals arrive as an access-denied D-Bus error rather than
    /// through the mechanism's own error string, so they are caught here and
    /// turned into [`CupsPkError::AuthorizationFailed`].
    pub(crate) fn call_failed(e: zbus::Error) -> CupsPkError {
        let text = e.to_string();
        if text.contains("AccessDenied") || text.contains("not authorized") {
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
