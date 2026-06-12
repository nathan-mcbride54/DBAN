use thiserror::Error;

/// Errors surfaced by the core engine, device discovery, and safety gate.
#[derive(Debug, Error)]
pub enum CoreError {
    /// An underlying I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A locked disk was passed to the engine (name, reason).
    #[error("disk {0} is locked ({1}) and can never be wiped in this session")]
    DiskLocked(String, String),

    /// The safety gate was built with no disks selected.
    #[error("no disks selected")]
    NothingSelected,

    /// The typed confirmation phrase did not match.
    #[error("confirmation phrase does not match")]
    PhraseMismatch,

    /// A state-machine method was called from an invalid state.
    #[error("operation not valid in the current state")]
    InvalidState,
}
