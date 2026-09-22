//! Composer action decisions that stay independent of GPUI rendering.

/// The one primary action shown at the trailing edge of the composer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrimaryAction {
    /// Submit the draft, including while another turn is running.
    Send,
    /// Stop the running turn when there is no draft to submit.
    Stop,
}

/// Choose the primary action from turn state and pending message contents.
pub fn primary_action(working: bool, draft: &str, has_attachments: bool) -> PrimaryAction {
    if working && draft.trim().is_empty() && !has_attachments {
        PrimaryAction::Stop
    } else {
        PrimaryAction::Send
    }
}

/// Whether the composer can accept another attachment intake request.
///
/// Uploads are serialized so an older completion cannot overwrite a newer
/// draft, and an ask-user response intentionally remains text-only.
pub fn can_accept_attachments(uploading: bool, awaiting_input: bool) -> bool {
    !uploading && !awaiting_input
}

/// Build the stable display name used for an image pasted from the clipboard.
pub fn pasted_image_name(ordinal: usize, extension: &str) -> String {
    format!("pasted-image-{}.{}", ordinal.max(1), extension)
}

/// Build the prompt sent over the shared protocol from text and uploaded files.
///
/// References stay outside the editable draft: the daemon owns their bytes and
/// expands each URI into a host path immediately before starting the agent.
pub fn submission(draft: &str, attachment_references: &[&str]) -> Option<String> {
    let draft = draft.trim();
    let attachments = attachment_references
        .iter()
        .map(|reference| reference.trim())
        .filter(|reference| !reference.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    match (draft.is_empty(), attachments.is_empty()) {
        (true, true) => None,
        (false, true) => Some(draft.to_string()),
        (true, false) => Some(attachments),
        (false, false) => Some(format!("{draft}\n\n{attachments}")),
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
        assert_eq!(primary_action(true, "", false), PrimaryAction::Stop);
        assert_eq!(primary_action(true, "  \n", false), PrimaryAction::Stop);
        assert_eq!(
            primary_action(true, "please also test it", false),
            PrimaryAction::Send
        );
        assert_eq!(primary_action(true, "", true), PrimaryAction::Send);
        assert_eq!(primary_action(false, "", false), PrimaryAction::Send);
    }

    #[test]
    fn attachments_are_only_accepted_while_the_composer_is_available() {
        assert!(can_accept_attachments(false, false));
        assert!(!can_accept_attachments(true, false));
        assert!(!can_accept_attachments(false, true));
        assert!(!can_accept_attachments(true, true));
    }

    #[test]
    fn pasted_images_get_readable_numbered_names() {
        assert_eq!(pasted_image_name(1, "png"), "pasted-image-1.png");
        assert_eq!(pasted_image_name(3, "jpg"), "pasted-image-3.jpg");
        assert_eq!(pasted_image_name(0, "webp"), "pasted-image-1.webp");
    }

    #[test]
    fn a_submission_keeps_attachment_references_outside_the_written_prompt() {
        assert_eq!(
            submission(
                "Review these",
                &["ginka-attachment:first.png", "ginka-attachment:notes.md"]
            ),
            Some("Review these\n\nginka-attachment:first.png\nginka-attachment:notes.md".into())
        );
        assert_eq!(
            submission("", &["ginka-attachment:first.png"]),
            Some("ginka-attachment:first.png".into())
        );
        assert_eq!(submission("  ", &[]), None);
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
