mod params;
mod verify;

use std::fmt;

pub use params::{default_params_dir, fetch_params, load_params, ParamPaths};
pub use verify::{verify_transaction, ShieldedParams};

#[derive(Debug)]
pub enum ShieldedError {
    Io(std::io::Error),
    MissingParams(String),
    InvalidParams(String),
    Download(String),
    Sighash(String),
    InvalidTransaction(&'static str),
    UnsupportedProof(&'static str),
}

impl fmt::Display for ShieldedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ShieldedError::Io(err) => write!(f, "{err}"),
            ShieldedError::MissingParams(message) => write!(f, "{message}"),
            ShieldedError::InvalidParams(message) => write!(f, "{message}"),
            ShieldedError::Download(message) => write!(f, "{message}"),
            ShieldedError::Sighash(message) => write!(f, "{message}"),
            ShieldedError::InvalidTransaction(message) => write!(f, "{message}"),
            ShieldedError::UnsupportedProof(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ShieldedError {}

impl From<std::io::Error> for ShieldedError {
    fn from(err: std::io::Error) -> Self {
        ShieldedError::Io(err)
    }
}
