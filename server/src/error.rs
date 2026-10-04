use std::{fmt, io};

#[derive(Debug)]
pub enum Error {
    Io {
        context: String,
        source: io::Error,
    },
    /// The data directory holds files but the index is missing or incomplete.
    NeedsReindex(String),
    /// The index no longer matches the data files.
    Mismatch(String),
    /// Another process holds the index.
    Busy(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { context, source } => write!(f, "{context}: {source}"),
            Error::NeedsReindex(message) | Error::Mismatch(message) | Error::Busy(message) => {
                f.write_str(message)
            }
        }
    }
}

impl std::error::Error for Error {}

pub trait Context<T> {
    fn context(self, what: impl FnOnce() -> String) -> Result<T>;
}

impl<T> Context<T> for io::Result<T> {
    fn context(self, what: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|source| Error::Io {
            context: what(),
            source,
        })
    }
}

pub fn ignore_not_found(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}
