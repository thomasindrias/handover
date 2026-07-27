use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HOME is not set and neither HANDOVER_HOME nor XDG_STATE_HOME is available")]
    StateHomeUnavailable,
    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid local state: {0}")]
    InvalidState(String),
    #[error("command failed: {0}")]
    Command(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Error {
    Error::Io {
        path: path.into(),
        source,
    }
}
