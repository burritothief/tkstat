use thiserror::Error;

#[derive(Debug, Error)]
pub enum TkstatError {
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("no claude data directory found")]
    NoDataDir,
}
