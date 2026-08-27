// SPDX-License-Identifier: GPL-3.0-only

use ipp::{
    operation::{IppOperation, cups::CupsGetPrinters},
    prelude::*,
};
use tracing::warn;

use crate::{Error, Printer, Result, lpoptions};

const LOCAL_CUPS: &str = "http://localhost:631";

/// Async client for a CUPS daemon.
pub struct CupsClient {
    inner: AsyncIppClient,
    /// Used only for the raw subscription requests `ipp` cannot express.
    http: reqwest::Client,
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
            http: reqwest::Client::new(),
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
        op: impl IppOperation,
        label: &str,
    ) -> Result<IppRequestResponse> {
        let resp = self
            .inner
            .send(op)
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

        let resp = self
            .inner
            .send(request)
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        Self::check_status(&resp, "CUPS-Get-Default")?;

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

    /// Lists all queues known to CUPS.
    pub async fn printers(&self) -> Result<Vec<Printer>> {
        let default = self.default_printer().await;
        let resp = self.send(CupsGetPrinters::new(), "CUPS-Get-Printers").await?;
        Ok(Self::decode_printers(&resp, default.as_deref()))
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
}
