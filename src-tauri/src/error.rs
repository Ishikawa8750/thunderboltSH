use thiserror::Error;

#[derive(Debug, Error)]
#[allow(dead_code)]
pub enum OpenBoltError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("mdns error: {0}")]
    Mdns(String),

    #[error("invalid path")]
    InvalidPath,

    #[error("service already running: {0}")]
    AlreadyRunning(&'static str),

    #[error("service not running: {0}")]
    NotRunning(&'static str),

    #[error("external command failed: {0}")]
    CommandFailed(String),

    #[error("unsupported platform")]
    UnsupportedPlatform
}

pub type OpenBoltResult<T> = Result<T, OpenBoltError>;
