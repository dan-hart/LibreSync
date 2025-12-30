use std::{error::Error as StdError, fmt, io};

#[derive(Debug)]
pub enum Error {
    Io(io::Error),
    Serde(serde_json::Error),
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io(error) => write!(formatter, "io error: {error}"),
            Error::Serde(error) => write!(formatter, "serialization error: {error}"),
            Error::Protocol(message) => write!(formatter, "protocol error: {message}"),
        }
    }
}

impl StdError for Error {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Error::Io(error) => Some(error),
            Error::Serde(error) => Some(error),
            Error::Protocol(_) => None,
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

#[cfg(test)]
mod tests {
    use super::{Error, Result};

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
}
