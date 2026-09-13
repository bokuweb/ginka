//! Composer action decisions that stay independent of GPUI rendering.

/// The one primary action shown at the trailing edge of the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryAction {
    /// Submit the draft, including while another turn is running.
    Send,
    /// Stop the running turn when there is no draft to submit.
    Stop,
}

/// Choose the primary action from turn state and the current draft.
pub fn primary_action(working: bool, draft: &str) -> PrimaryAction {
    if working && draft.trim().is_empty() {
        PrimaryAction::Stop
    } else {
        PrimaryAction::Send
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_during_a_turn_replaces_stop_with_send() {
        assert_eq!(primary_action(true, ""), PrimaryAction::Stop);
        assert_eq!(primary_action(true, "  \n"), PrimaryAction::Stop);
        assert_eq!(
            primary_action(true, "please also test it"),
            PrimaryAction::Send
        );
        assert_eq!(primary_action(false, ""), PrimaryAction::Send);
    }
}
