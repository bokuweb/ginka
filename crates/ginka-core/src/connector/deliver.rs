//! Turning what a thread should see into calls on a transport.
//!
//! The one place [`Outbound`] meets [`ChatTransport`]. It knows which
//! message a reaction goes on and where the progress message is; the fold
//! that produced the outbound does not, which is what keeps the fold pure.

use super::fold::{Outbound, QuestionKind};
use super::text::{CHUNK_CHARS, MAX_CHUNKS, chunk, to_mrkdwn};
use super::transport::ChatTransport;
use anyhow::Result;

/// Where a followed turn's messages go.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ThreadContext {
    pub channel: String,
    pub thread: String,
    /// The message that triggered the turn, which reactions go on.
    pub trigger: String,
    /// The progress message, once there is one.
    pub progress: Option<String>,
}

/// The text the runner filled a footer in with.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FooterText {
    pub changed_files: usize,
    pub checkpoint_turn: u32,
    /// The workspace id, for the `ginka review <workspace>` hint.
    pub workspace: String,
}

impl FooterText {
    /// `3 files changed · checkpoint 7 · ginka review comet/harbor`
    pub fn line(&self) -> String {
        let files = match self.changed_files {
            1 => "1 file changed".to_string(),
            n => format!("{n} files changed"),
        };
        format!(
            "{files} · checkpoint {} · `ginka review {}`",
            self.checkpoint_turn, self.workspace
        )
    }
}

/// Send one outbound. `footer` is consulted for [`Outbound::Footer`]; a
/// footer with nothing changed is not posted, because "0 files changed" is
/// noise under an answer that was only words.
pub fn apply(
    transport: &mut dyn ChatTransport,
    context: &mut ThreadContext,
    outbound: &Outbound,
    footer: Option<&FooterText>,
) -> Result<()> {
    let channel = context.channel.clone();
    let thread = context.thread.clone();
    match outbound {
        Outbound::React { glyph } => transport.react(&channel, &context.trigger, *glyph),
        Outbound::Unreact { glyph } => transport.unreact(&channel, &context.trigger, *glyph),
        Outbound::Progress { text } => match &context.progress {
            Some(message) => transport.edit(&channel, message, text),
            None => {
                let message = transport.post(&channel, &thread, text)?;
                context.progress = Some(message);
                Ok(())
            }
        },
        Outbound::ClearProgress => {
            if let Some(message) = context.progress.take() {
                transport.delete(&channel, &message)?;
            }
            Ok(())
        }
        Outbound::Reply { markdown } => {
            let text = to_mrkdwn(markdown);
            let chunks = chunk(&text, CHUNK_CHARS);
            let (posted, rest) = if chunks.len() > MAX_CHUNKS {
                chunks.split_at(MAX_CHUNKS)
            } else {
                (&chunks[..], &[][..])
            };
            for piece in posted {
                transport.post(&channel, &thread, piece)?;
            }
            if !rest.is_empty() {
                transport.upload(&channel, &thread, "reply.md", rest.join("\n").as_bytes())?;
            }
            Ok(())
        }
        Outbound::Footer { .. } => match footer {
            Some(footer) if footer.changed_files > 0 => {
                transport.post(&channel, &thread, &footer.line())?;
                Ok(())
            }
            _ => Ok(()),
        },
        Outbound::Question {
            request_id,
            kind,
            text,
            options,
        } => {
            transport.post(
                &channel,
                &thread,
                &question_text(request_id, *kind, text, options),
            )?;
            Ok(())
        }
        Outbound::Note { text } => {
            transport.post(&channel, &thread, text)?;
            Ok(())
        }
    }
}

/// A question as the thread reads it, with the id and how to answer.
pub fn question_text(
    request_id: &str,
    kind: QuestionKind,
    text: &str,
    options: &[String],
) -> String {
    let mut out = match kind {
        QuestionKind::Ask => "The agent asks:".to_string(),
        QuestionKind::Plan => "The agent proposes a plan and wants it approved:".to_string(),
        QuestionKind::Permission => {
            "The agent wants to do something its access mode does not allow:".to_string()
        }
    };
    out.push('\n');
    out.push_str(&to_mrkdwn(text));
    if !options.is_empty() {
        out.push('\n');
        for (index, option) in options.iter().enumerate() {
            out.push_str(&format!("\n{}. {option}", index + 1));
        }
    }
    out.push_str(&format!(
        "\n\nAn approver answers with `yes {request_id}`, `no {request_id}`, or `{request_id}: your answer`."
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::transport::Glyph;
    use crate::connector::transport::testing::{Call, ScriptedTransport};

    fn context() -> ThreadContext {
        ThreadContext {
            channel: "C1".into(),
            thread: "1.0".into(),
            trigger: "1.0".into(),
            progress: None,
        }
    }

    #[test]
    fn progress_is_posted_once_then_edited_then_deleted() {
        let mut transport = ScriptedTransport::new();
        let mut context = context();
        apply(
            &mut transport,
            &mut context,
            &Outbound::Progress {
                text: "Working: a".into(),
            },
            None,
        )
        .unwrap();
        apply(
            &mut transport,
            &mut context,
            &Outbound::Progress {
                text: "Working: b".into(),
            },
            None,
        )
        .unwrap();
        apply(&mut transport, &mut context, &Outbound::ClearProgress, None).unwrap();
        assert_eq!(
            transport.calls(),
            vec![
                Call::Post {
                    channel: "C1".into(),
                    thread: "1.0".into(),
                    text: "Working: a".into()
                },
                Call::Edit {
                    channel: "C1".into(),
                    message: "m1".into(),
                    text: "Working: b".into()
                },
                Call::Delete {
                    channel: "C1".into(),
                    message: "m1".into()
                },
            ]
        );
        assert!(context.progress.is_none());
    }

    #[test]
    fn reactions_go_on_the_message_that_asked() {
        let mut transport = ScriptedTransport::new();
        let mut context = context();
        apply(
            &mut transport,
            &mut context,
            &Outbound::React {
                glyph: Glyph::Working,
            },
            None,
        )
        .unwrap();
        assert_eq!(
            transport.calls()[0],
            Call::React {
                channel: "C1".into(),
                message: "1.0".into(),
                glyph: Glyph::Working
            }
        );
    }

    #[test]
    fn a_long_reply_is_chunked_and_the_rest_becomes_a_file() {
        let mut transport = ScriptedTransport::new();
        let mut context = context();
        let paragraph = "word ".repeat(200);
        let markdown = (0..40)
            .map(|_| paragraph.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        apply(
            &mut transport,
            &mut context,
            &Outbound::Reply { markdown },
            None,
        )
        .unwrap();
        let posts = transport.posts();
        assert_eq!(posts.len(), MAX_CHUNKS);
        assert!(posts.iter().all(|p| p.chars().count() <= CHUNK_CHARS));
        assert!(matches!(
            transport.calls().last(),
            Some(Call::Upload { name, .. }) if name == "reply.md"
        ));
    }

    #[test]
    fn a_footer_is_posted_only_when_something_changed() {
        let mut transport = ScriptedTransport::new();
        let mut context = context();
        apply(
            &mut transport,
            &mut context,
            &Outbound::Footer { turn: 1 },
            Some(&FooterText {
                changed_files: 0,
                checkpoint_turn: 1,
                workspace: "comet/harbor".into(),
            }),
        )
        .unwrap();
        assert!(transport.posts().is_empty());
        apply(
            &mut transport,
            &mut context,
            &Outbound::Footer { turn: 2 },
            Some(&FooterText {
                changed_files: 3,
                checkpoint_turn: 2,
                workspace: "comet/harbor".into(),
            }),
        )
        .unwrap();
        assert_eq!(
            transport.posts(),
            vec!["3 files changed · checkpoint 2 · `ginka review comet/harbor`".to_string()]
        );
    }

    #[test]
    fn a_question_names_its_id_and_how_to_answer() {
        let text = question_text(
            "abcde",
            QuestionKind::Ask,
            "Which one?",
            &["left".into(), "right".into()],
        );
        assert!(text.contains("1. left"));
        assert!(text.contains("`yes abcde`"));
        assert!(text.contains("`abcde: your answer`"));
    }
}
