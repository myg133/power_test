//! Error types for power_test.

use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)] // Some variants are part of the public surface for future milestones.
pub enum Error {
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("histogram error: {0}")]
    Histogram(String),

    #[error("operation not implemented in M1: {0}")]
    NotImplemented(&'static str),

    #[error("run not found: {0}")]
    RunNotFound(String),

    #[error("storage error: {0}")]
    Storage(String),

    #[error("other: {0}")]
    Other(String),
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::Io {
            path: PathBuf::new(),
            source: err,
        }
    }
}

impl Error {
    pub fn io_at(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, Error>;
