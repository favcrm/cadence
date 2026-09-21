use std::time::Duration;

/// The observable facts needed to decide whether a paste became a turn.
/// Screen capture and provider-specific input analysis stay outside this
/// state machine so the decision can be tested without a PTY or subprocess.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderObservation {
    NotVisible,
    Visible { input_nonempty: bool },
}

/// The terminal-side outcome after a paste and Enter attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RenderOutcome {
    Submitted,
    Staged,
    NotRendered,
}

/// Decide a render outcome from timestamped screen observations.
pub(crate) struct RenderDecision {
    deadline: Duration,
    rendered: bool,
}

impl RenderDecision {
    pub(crate) fn new(deadline: Duration) -> Self {
        Self {
            deadline,
            rendered: false,
        }
    }

    /// Observe one capture at `elapsed` time after the Enter attempt.
    ///
    /// The observation is processed before the deadline check. This keeps
    /// the existing boundary behavior: a capture taken at or after the
    /// deadline can still prove submission when it shows rendered text and
    /// an empty input line; otherwise the deadline classifies the evidence.
    pub(crate) fn observe(
        &mut self,
        elapsed: Duration,
        observation: RenderObservation,
    ) -> Option<RenderOutcome> {
        if let RenderObservation::Visible { input_nonempty } = observation {
            self.rendered = true;
            if !input_nonempty {
                return Some(RenderOutcome::Submitted);
            }
        }

        if elapsed >= self.deadline {
            return Some(if self.rendered {
                RenderOutcome::Staged
            } else {
                RenderOutcome::NotRendered
            });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::{RenderDecision, RenderObservation, RenderOutcome};
    use std::time::Duration;

    const DEADLINE: Duration = Duration::from_secs(4);

    #[test]
    fn rendered_with_empty_input_is_submitted() {
        let mut decision = RenderDecision::new(DEADLINE);

        assert_eq!(
            decision.observe(
                Duration::from_millis(150),
                RenderObservation::Visible {
                    input_nonempty: false,
                },
            ),
            Some(RenderOutcome::Submitted)
        );
    }

    #[test]
    fn rendered_with_nonempty_input_expires_as_staged() {
        let mut decision = RenderDecision::new(DEADLINE);

        assert_eq!(
            decision.observe(
                Duration::from_millis(150),
                RenderObservation::Visible {
                    input_nonempty: true,
                },
            ),
            None
        );
        assert_eq!(
            decision.observe(DEADLINE, RenderObservation::NotVisible),
            Some(RenderOutcome::Staged)
        );
    }

    #[test]
    fn never_rendered_expires_as_not_rendered() {
        let mut decision = RenderDecision::new(DEADLINE);

        assert_eq!(
            decision.observe(DEADLINE, RenderObservation::NotVisible),
            Some(RenderOutcome::NotRendered)
        );
    }

    #[test]
    fn deadline_sample_is_processed_before_expiry() {
        let mut submitted = RenderDecision::new(DEADLINE);
        assert_eq!(
            submitted.observe(
                DEADLINE,
                RenderObservation::Visible {
                    input_nonempty: false,
                },
            ),
            Some(RenderOutcome::Submitted)
        );

        let mut staged = RenderDecision::new(DEADLINE);
        assert_eq!(
            staged.observe(
                DEADLINE,
                RenderObservation::Visible {
                    input_nonempty: true,
                },
            ),
            Some(RenderOutcome::Staged)
        );
    }

    #[test]
    fn late_empty_input_sample_still_proves_submission() {
        let mut decision = RenderDecision::new(DEADLINE);

        assert_eq!(
            decision.observe(
                DEADLINE + Duration::from_millis(150),
                RenderObservation::Visible {
                    input_nonempty: false,
                },
            ),
            Some(RenderOutcome::Submitted)
        );
    }
}
