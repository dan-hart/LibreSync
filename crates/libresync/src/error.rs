use std::{error::Error as StdError, fmt, io};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Serde(serde_json::Error),
    Protocol(String),
    Crypto(String),
    /// A linked device presented a certificate whose fingerprint does not
    /// match the pinned one. The app should ask the user before re-linking.
    FingerprintMismatch {
        device_id: String,
        expected: String,
        actual: String,
    },
    /// The operation was cancelled through a [`crate::CancelToken`].
    Cancelled,
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(formatter, "io error: {error}"),
            Error::Serde(error) => write!(formatter, "serialization error: {error}"),
            Error::Protocol(message) => write!(formatter, "protocol error: {message}"),
            Error::Crypto(message) => write!(formatter, "crypto error: {message}"),
            Error::FingerprintMismatch {
                device_id,
                expected,
                actual,
            } => write!(
                formatter,
                "fingerprint mismatch for {device_id}: pinned {expected}, presented {actual}"
            ),
            Error::Cancelled => write!(formatter, "operation cancelled"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            Error::Serde(error) => Some(error),
            Error::Protocol(_) => None,
            Error::Crypto(_) => None,
            Error::FingerprintMismatch { .. } => None,
            Error::Cancelled => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Self {
        Error::Io(error)
    }
}

impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Error::Serde(error)
    }
}

impl From<rcgen::Error> for Error {
    fn from(error: rcgen::Error) -> Self {
        Error::Crypto(error.to_string())
    }
}

impl From<rustls::Error> for Error {
    fn from(error: rustls::Error) -> Self {
        Error::Crypto(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, Result};
    use std::error::Error as StdError;
    use std::io;

    #[test]
    fn error_display_includes_context() {
        let error = Error::Protocol("missing hello".to_string());
        assert!(error.to_string().contains("missing hello"));
    }

    #[test]
    fn error_result_type_alias_works() {
        fn helper() -> Result<()> {
            Ok(())
        }

        assert!(helper().is_ok());
    }

    #[test]
    fn error_source_is_set_for_io() {
        let io_error = io::Error::other("boom");
        let error = Error::from(io_error);
        assert!(StdError::source(&error).is_some());
    }

    #[test]
    fn fingerprint_mismatch_display_names_both_fingerprints() {
        let error = Error::FingerprintMismatch {
            device_id: "d".to_string(),
            expected: "aa".to_string(),
            actual: "bb".to_string(),
        };
        let text = error.to_string();
        assert!(text.contains("aa") && text.contains("bb") && text.contains('d'));
        assert!(StdError::source(&error).is_none());
        assert!(Error::Cancelled.to_string().contains("cancelled"));
    }

    #[test]
    fn error_source_is_none_for_protocol() {
        let error = Error::Protocol("missing".to_string());
        assert!(StdError::source(&error).is_none());
    }
}
