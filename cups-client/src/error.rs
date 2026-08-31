// SPDX-License-Identifier: MIT OR Apache-2.0

use std::io;

/// Errors returned by the CUPS client.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The daemon or printer could not be reached.
    ///
    /// Keeps the underlying cause reachable through [`std::error::Error::source`]
    /// rather than flattening it to a string: a caller that needs to tell a
    /// refused connection from a rejected certificate has to be able to look.
    #[error("cannot reach the print service: {message}")]
    Transport {
        /// What went wrong, as reported by the transport.
        message: String,
        /// The underlying cause, where there was one.
        #[source]
        source: Option<Box<dyn std::error::Error + Send + Sync>>,
    },

    /// CUPS answered with a non-success IPP status code.
    #[error("CUPS returned {status} for {operation}")]
    Ipp {
        /// The operation that was refused, by its IPP name.
        operation: String,
        /// The status the peer answered with.
        status: String,
    },

    /// An IPP attribute could not be decoded into a domain type.
    #[error("cannot decode attribute {attribute}: {detail}")]
    Decode {
        /// The IPP attribute that could not be read.
        attribute: String,
        /// Why not.
        detail: String,
    },

    /// Reading a document from disk failed.
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// The result type every fallible operation in this crate returns.
pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn decode(attribute: impl Into<String>, detail: impl Into<String>) -> Self {
        Error::Decode {
            attribute: attribute.into(),
            detail: detail.into(),
        }
    }

    /// A transport failure that keeps its cause.
    pub(crate) fn transport<E>(source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static,
    {
        Error::Transport {
            message: source.to_string(),
            source: Some(Box::new(source)),
        }
    }

    /// A transport failure with nothing underneath it, such as a malformed URI
    /// rejected before any connection was attempted.
    pub(crate) fn transport_msg(message: impl Into<String>) -> Self {
        Error::Transport {
            message: message.into(),
            source: None,
        }
    }

    /// Whether this failed because the peer's TLS certificate was not accepted.
    ///
    /// Printers overwhelmingly ship self-signed certificates, so this is the
    /// error a caller is most likely to want to act on - by pinning the
    /// certificate with [`crate::CupsClientBuilder::ca_cert`], or by telling
    /// the user why the printer cannot be reached.
    pub fn is_certificate_error(&self) -> bool {
        let mut source: Option<&(dyn std::error::Error + 'static)> = Some(self);
        while let Some(e) = source {
            let text = e.to_string();
            if text.contains("certificate") || text.contains("CertificateError") {
                return true;
            }
            source = e.source();
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_error_names_the_offending_attribute() {
        let err = Error::Decode {
            attribute: "marker-levels".into(),
            detail: "length mismatch".into(),
        };
        assert!(err.to_string().contains("marker-levels"));
        assert!(err.to_string().contains("length mismatch"));
    }
}
