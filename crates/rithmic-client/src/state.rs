//! Connection state machine for the Rithmic client.

use crate::error::RithmicError;

/// Connection states for the Rithmic WebSocket client.
///
/// ```text
/// Disconnected → Connecting → Authenticating → Subscribing → Streaming
///                                                                ↓
///                                                            Degraded
///                                                                ↓
///                                                           Disconnected
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Authenticating,
    Subscribing,
    Streaming,
    Degraded,
}

impl ConnectionState {
    /// Attempt to transition to a new state.
    /// Returns `Ok(new_state)` if the transition is valid, or `Err` if not.
    pub fn transition(self, to: ConnectionState) -> Result<ConnectionState, RithmicError> {
        if self.can_transition_to(to) {
            Ok(to)
        } else {
            Err(RithmicError::Config(format!(
                "invalid state transition: {:?} → {:?}",
                self, to
            )))
        }
    }

    fn can_transition_to(self, to: ConnectionState) -> bool {
        use ConnectionState::*;
        matches!(
            (self, to),
            (Disconnected, Connecting)
                | (Connecting, Authenticating)
                | (Connecting, Disconnected) // connection failure
                | (Authenticating, Subscribing)
                | (Authenticating, Disconnected) // auth failure
                | (Subscribing, Streaming)
                | (Subscribing, Disconnected) // sub failure
                | (Streaming, Degraded)
                | (Streaming, Disconnected) // clean shutdown
                | (Degraded, Disconnected) // reconnect
        )
    }
}

impl std::fmt::Display for ConnectionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_forward_transitions() {
        use ConnectionState::*;
        let valid = vec![
            (Disconnected, Connecting),
            (Connecting, Authenticating),
            (Authenticating, Subscribing),
            (Subscribing, Streaming),
            (Streaming, Degraded),
            (Degraded, Disconnected),
        ];
        for (from, to) in valid {
            assert_eq!(
                from.transition(to).unwrap(),
                to,
                "should allow {:?} → {:?}",
                from,
                to
            );
        }
    }

    #[test]
    fn valid_failure_transitions() {
        use ConnectionState::*;
        let valid = vec![
            (Connecting, Disconnected),
            (Authenticating, Disconnected),
            (Subscribing, Disconnected),
            (Streaming, Disconnected),
        ];
        for (from, to) in valid {
            assert!(
                from.transition(to).is_ok(),
                "should allow {:?} → {:?}",
                from,
                to
            );
        }
    }

    #[test]
    fn invalid_transitions_rejected() {
        use ConnectionState::*;
        let invalid = vec![
            (Disconnected, Streaming),
            (Disconnected, Authenticating),
            (Connecting, Streaming),
            (Authenticating, Streaming),
            (Subscribing, Degraded),
            (Degraded, Streaming),
            (Degraded, Connecting),
            (Streaming, Authenticating),
            (Streaming, Subscribing),
        ];
        for (from, to) in invalid {
            assert!(
                from.transition(to).is_err(),
                "should reject {:?} → {:?}",
                from,
                to
            );
        }
    }

    #[test]
    fn self_transitions_rejected() {
        use ConnectionState::*;
        for state in [
            Disconnected,
            Connecting,
            Authenticating,
            Subscribing,
            Streaming,
            Degraded,
        ] {
            assert!(
                state.transition(state).is_err(),
                "self-transition {:?} → {:?} should be rejected",
                state,
                state
            );
        }
    }

    #[test]
    fn display_shows_variant_name() {
        assert_eq!(ConnectionState::Streaming.to_string(), "Streaming");
        assert_eq!(ConnectionState::Degraded.to_string(), "Degraded");
    }
}
