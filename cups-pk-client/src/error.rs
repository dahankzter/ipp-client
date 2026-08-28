// SPDX-License-Identifier: MIT OR Apache-2.0

/// Errors from the `cups-pk-helper` mechanism.
#[derive(Debug, thiserror::Error)]
pub enum CupsPkError {
    /// The caller is not permitted, or dismissed the polkit prompt.
    ///
    /// This is deliberately not a [`CupsPkError::Mechanism`]: a user closing an
    /// authentication dialog has not hit a failure, and a UI must be able to
    /// tell the two apart.
    #[error("not authorized")]
    AuthorizationFailed,

    /// The mechanism reported a failure, in its own words.
    #[error("{0}")]
    Mechanism(String),

    /// The mechanism could not be reached.
    #[error("cannot reach cups-pk-helper: {0}")]
    Transport(String),
}

pub type Result<T> = std::result::Result<T, CupsPkError>;

/// Translates the mechanism's error convention.
///
/// `cups-pk-helper` raises no D-Bus errors. Every method returns an `error`
/// out-parameter which is empty on success and a human-readable message
/// otherwise. Ignoring it reports success for every failure, so every call in
/// this crate goes through here.
pub(crate) fn translate(error: String) -> Result<()> {
    if error.is_empty() {
        Ok(())
    } else {
        Err(CupsPkError::Mechanism(error))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_error_string_is_success() {
        assert!(translate(String::new()).is_ok());
    }

    #[test]
    fn a_non_empty_error_string_is_a_mechanism_failure() {
        let err = translate("\"nope\" is not a valid printer name.".to_string()).unwrap_err();
        match &err {
            CupsPkError::Mechanism(msg) => {
                assert!(msg.contains("not a valid printer name"));
            }
            other => panic!("expected Mechanism, got {other:?}"),
        }
    }

    #[test]
    fn the_mechanisms_own_wording_is_preserved_verbatim() {
        // CUPS' messages are more precise than any paraphrase, and users
        // search for them.
        let raw = "Cannot open \"/etc/cups/ppd/x.ppd\": Permission denied";
        let err = translate(raw.to_string()).unwrap_err();
        assert_eq!(err.to_string(), raw);
    }

    #[test]
    fn whitespace_only_is_a_failure_not_a_success() {
        // Defensive: only a genuinely empty string means success.
        assert!(translate(" ".to_string()).is_err());
    }

    #[test]
    fn authorization_failure_is_its_own_variant() {
        // Dismissing a polkit prompt must never read as breakage.
        let err = CupsPkError::AuthorizationFailed;
        assert!(!matches!(err, CupsPkError::Mechanism(_)));
        assert!(err.to_string().to_lowercase().contains("authoriz"));
    }
}
