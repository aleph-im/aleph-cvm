use std::fmt;

use thiserror::Error;

/// The possible states of a VM throughout its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum VmState {
    /// VM has been defined but not yet started.
    Defined,
    /// VM is in the process of booting.
    Booting,
    /// VM is running normally.
    Running,
    /// VM is in the process of shutting down.
    Stopping,
    /// VM has stopped.
    Stopped,
    /// VM has entered an error state.
    Failed,
}

impl fmt::Display for VmState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VmState::Defined => write!(f, "defined"),
            VmState::Booting => write!(f, "booting"),
            VmState::Running => write!(f, "running"),
            VmState::Stopping => write!(f, "stopping"),
            VmState::Stopped => write!(f, "stopped"),
            VmState::Failed => write!(f, "failed"),
        }
    }
}

#[derive(Debug, Error)]
#[error("invalid state transition: {from} -> {to}")]
#[allow(dead_code)]
pub struct InvalidTransition {
    pub from: VmState,
    pub to: VmState,
}

#[allow(dead_code)]
impl VmState {
    /// Check whether a transition from `self` to `target` is valid.
    pub fn can_transition_to(&self, target: VmState) -> bool {
        matches!(
            (self, target),
            (VmState::Defined, VmState::Booting)
                | (VmState::Booting, VmState::Running)
                | (VmState::Booting, VmState::Failed)
                | (VmState::Running, VmState::Stopping)
                | (VmState::Running, VmState::Failed)
                | (VmState::Stopping, VmState::Stopped)
        )
    }

    /// Attempt to transition to a new state, returning the new state on success.
    pub fn transition(self, target: VmState) -> Result<VmState, InvalidTransition> {
        if self.can_transition_to(target) {
            Ok(target)
        } else {
            Err(InvalidTransition {
                from: self,
                to: target,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        assert!(VmState::Defined.can_transition_to(VmState::Booting));
        assert!(VmState::Booting.can_transition_to(VmState::Running));
        assert!(VmState::Booting.can_transition_to(VmState::Failed));
        assert!(VmState::Running.can_transition_to(VmState::Stopping));
        assert!(VmState::Running.can_transition_to(VmState::Failed));
        assert!(VmState::Stopping.can_transition_to(VmState::Stopped));
    }

    #[test]
    fn test_invalid_transitions() {
        // Cannot skip states
        assert!(!VmState::Defined.can_transition_to(VmState::Running));
        // Cannot go backward
        assert!(!VmState::Running.can_transition_to(VmState::Booting));
        assert!(!VmState::Stopped.can_transition_to(VmState::Running));
        // Stopped/Failed are terminal
        assert!(!VmState::Stopped.can_transition_to(VmState::Defined));
        assert!(!VmState::Failed.can_transition_to(VmState::Defined));
        // Defined cannot go to Failed directly
        assert!(!VmState::Defined.can_transition_to(VmState::Failed));
    }

    #[test]
    fn test_transition_returns_new_state() {
        let state = VmState::Defined;
        let new = state.transition(VmState::Booting).unwrap();
        assert_eq!(new, VmState::Booting);

        let new = new.transition(VmState::Running).unwrap();
        assert_eq!(new, VmState::Running);

        let new = new.transition(VmState::Stopping).unwrap();
        assert_eq!(new, VmState::Stopping);

        let new = new.transition(VmState::Stopped).unwrap();
        assert_eq!(new, VmState::Stopped);
    }

    #[test]
    fn test_transition_error_on_invalid() {
        let state = VmState::Defined;
        let err = state.transition(VmState::Running).unwrap_err();
        assert_eq!(err.from, VmState::Defined);
        assert_eq!(err.to, VmState::Running);
        assert!(err.to_string().contains("invalid state transition"));
    }
}
