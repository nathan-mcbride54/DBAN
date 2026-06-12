use thiserror::Error;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("disk {0} is locked ({1}) and can never be wiped in this session")]
    DiskLocked(String, String),

    #[error("no disks selected")]
    NothingSelected,

    #[error("confirmation phrase does not match")]
    PhraseMismatch,

    #[error("operation not valid in the current state")]
    InvalidState,
}
