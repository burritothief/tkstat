use thiserror::Error;

#[derive(Debug, Error)]
pub enum TkstatError {
    #[error("no claude data directory found")]
    NoDataDir,
}
