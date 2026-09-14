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

/// Append one message to a draft as a Markdown blockquote.
///
/// Line endings are normalized before quoting, blank lines remain part of the
/// quote, and the trailing blank line leaves the caret ready for a reply. An
/// empty message leaves the draft unchanged.
pub fn append_quote(draft: &str, message: &str) -> String {
    let message = message.replace("\r\n", "\n").replace('\r', "\n");
    let message = message.trim();
    if message.is_empty() {
        return draft.to_string();
    }

    let quote = message
        .split('\n')
        .map(|line| {
            if line.is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let separator = if draft.is_empty() || draft.ends_with("\n\n") {
        ""
    } else if draft.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    format!("{draft}{separator}{quote}\n\n")
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

    #[test]
    fn quote_normalizes_multiline_text_and_leaves_room_for_a_reply() {
        assert_eq!(
            append_quote("", " first line\r\n\r\nsecond line "),
            "> first line\n>\n> second line\n\n"
        );
    }

    #[test]
    fn quote_separates_itself_from_every_existing_draft_shape() {
        for draft in ["draft", "draft\n", "draft\n\n"] {
            assert_eq!(append_quote(draft, "answer"), "draft\n\n> answer\n\n");
        }
    }

    #[test]
    fn quote_ignores_a_whitespace_only_message() {
        assert_eq!(append_quote("draft", " \r\n  "), "draft");
    }
}
