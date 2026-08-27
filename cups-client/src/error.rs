// SPDX-License-Identifier: GPL-3.0-only

use std::io;

/// Errors returned by the CUPS client.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The CUPS daemon could not be reached.
    #[error("cannot reach CUPS: {0}")]
    Transport(String),

    /// CUPS answered with a non-success IPP status code.
    #[error("CUPS returned {status} for {operation}")]
    Ipp { operation: String, status: String },

    /// An IPP attribute could not be decoded into a domain type.
    #[error("cannot decode attribute {attribute}: {detail}")]
    Decode { attribute: String, detail: String },

    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub(crate) fn decode(attribute: impl Into<String>, detail: impl Into<String>) -> Self {
        Error::Decode { attribute: attribute.into(), detail: detail.into() }
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
