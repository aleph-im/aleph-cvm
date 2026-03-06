use std::fmt;

/// The possible states of a VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    /// VM is running normally.
    Running,
    /// VM has stopped.
    Stopped,
}

impl fmt::Display for VmState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmState::Running => write!(f, "running"),
            VmState::Stopped => write!(f, "stopped"),
        }
    }
}
