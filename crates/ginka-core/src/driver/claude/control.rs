//! Claude Code asking before it acts, and the answers it is sent back.
//!
//! Started with `--permission-prompt-tool stdio`, the CLI does not refuse a
//! tool its permission mode would ask about: it writes a `control_request`
//! (`subtype: "can_use_tool"`) on stdout and waits for a `control_response`
//! on stdin — the same exchange the vendor's own SDK uses. Three kinds of ask
//! arrive this way, and each becomes the card the transcript already draws:
//!
//! - an ordinary tool (a shell command under `acceptEdits`, an edit under
//!   `plan`) is a question with *Allow* and *Deny*;
//! - `AskUserQuestion` is the agent's own question, with its own choices;
//! - `ExitPlanMode` is a plan to approve.
//!
//! The answer has to name the request and, to allow a tool, hand its input
//! back. So, as the ACP driver does, the id the card carries is itself a
//! small JSON document holding what the answer needs; the supervisor treats
//! it as opaque and gives it back with the reader's choice.
//!
//! Any other control request (hooks, SDK-side MCP servers) was never offered
//! by this client, and is answered with an error at once rather than left
//! for the CLI to wait on.

use ginka_protocol::event::AgentEvent;
use serde_json::{Value, json};

/// The choice that lets a tool run.
pub const ALLOW: &str = "Allow";
/// The choice that refuses it.
pub const DENY: &str = "Deny";

/// What the plan card sends when the plan is approved (`src/shell.rs`).
const PLAN_APPROVED: &str = "Approved";

/// How long a command or path is shown in a card before it is cut.
const SHOWN_CHARS: usize = 400;

/// Read one `control_request`: the card to show, and any line to write back
/// at once.
pub fn request(message: &Value) -> (Vec<AgentEvent>, Option<String>) {
    let Some(request_id) = message.get("request_id").and_then(Value::as_str) else {
        return (Vec::new(), None);
    };
    let request = message.get("request").unwrap_or(&Value::Null);
    if request.get("subtype").and_then(Value::as_str) != Some("can_use_tool") {
        return (
            Vec::new(),
            Some(error(request_id, "not supported by this client")),
        );
    }
    let tool = request
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input = request.get("input").cloned().unwrap_or_else(|| json!({}));
    let card =
        |kind: &str| json!({"control": request_id, "kind": kind, "input": input}).to_string();
    let event = match tool {
        "AskUserQuestion" => {
            let questions = input
                .get("questions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let text = |question: &Value| {
                question
                    .get("question")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            };
            match questions.as_slice() {
                // One question is a card with its choices as buttons.
                [only] => AgentEvent::AskUser {
                    id: card("question"),
                    question: text(only),
                    options: only
                        .get("options")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|option| option.get("label").and_then(Value::as_str))
                        .map(str::to_string)
                        .collect(),
                },
                // Several are asked together and answered in words.
                several => AgentEvent::AskUser {
                    id: card("question"),
                    question: several.iter().map(text).collect::<Vec<_>>().join("\n"),
                    options: Vec::new(),
                },
            }
        }
        "ExitPlanMode" => AgentEvent::PlanProposal {
            id: card("plan"),
            plan: input
                .get("plan")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        },
        _ => AgentEvent::AskUser {
            id: card("tool"),
            question: describe(tool, &input),
            options: vec![ALLOW.to_string(), DENY.to_string()],
        },
    };
    (vec![event], None)
}

/// The line that answers a card, from the id it carried and what was chosen.
pub fn response(card: &str, answer: &str) -> Option<String> {
    let card: Value = serde_json::from_str(card).ok()?;
    let request_id = card.get("control")?.as_str()?;
    let input = card.get("input").cloned().unwrap_or_else(|| json!({}));
    let answer = answer.trim();
    let decision = match card.get("kind")?.as_str()? {
        "tool" if answer == ALLOW => json!({"behavior": "allow", "updatedInput": input}),
        "tool" => json!({"behavior": "deny", "message": "The user did not allow this."}),
        "plan" if answer.starts_with(PLAN_APPROVED) => {
            json!({"behavior": "allow", "updatedInput": input})
        }
        // The reader's words are the feedback the agent plans again from.
        "plan" => json!({"behavior": "deny", "message": answer}),
        "question" => {
            // The answers go back beside the questions, keyed by each
            // question's text; one answer covers every question asked.
            let mut input = input;
            let answers: serde_json::Map<String, Value> = input
                .get("questions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|question| question.get("question").and_then(Value::as_str))
                .map(|question| (question.to_string(), Value::from(answer)))
                .collect();
            if let Some(object) = input.as_object_mut() {
                object.insert("answers".into(), Value::Object(answers));
            }
            json!({"behavior": "allow", "updatedInput": input})
        }
        _ => return None,
    };
    Some(
        json!({
            "type": "control_response",
            "response": {"subtype": "success", "request_id": request_id, "response": decision},
        })
        .to_string(),
    )
}

/// A control request this client does not take, refused.
fn error(request_id: &str, message: &str) -> String {
    json!({
        "type": "control_response",
        "response": {"subtype": "error", "request_id": request_id, "error": message},
    })
    .to_string()
}

/// What a card asks, in the words the reader needs to decide: the command,
/// the file or the address, rather than the tool's raw input.
fn describe(tool: &str, input: &Value) -> String {
    let field = |key: &str| input.get(key).and_then(Value::as_str);
    let subject = field("command")
        .or_else(|| field("file_path"))
        .or_else(|| field("notebook_path"))
        .or_else(|| field("url"))
        .or_else(|| field("pattern"))
        .map(str::to_string)
        .unwrap_or_else(|| input.to_string());
    let subject = match subject.char_indices().nth(SHOWN_CHARS) {
        Some((end, _)) => format!("{}…", &subject[..end]),
        None => subject,
    };
    format!("Claude wants to use {tool}:\n{subject}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asked(tool: &str, input: Value) -> Value {
        json!({
            "type": "control_request",
            "request_id": "req-1",
            "request": {"subtype": "can_use_tool", "tool_name": tool, "input": input},
        })
    }

    fn sent(line: &str) -> Value {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn a_command_is_asked_about_with_allow_and_deny() {
        let (events, reply) = request(&asked("Bash", json!({"command": "cargo test"})));
        assert!(reply.is_none(), "the CLI waits for the reader, not for us");
        let [
            AgentEvent::AskUser {
                question, options, ..
            },
        ] = events.as_slice()
        else {
            panic!("expected one question, got {events:?}");
        };
        assert!(question.contains("Bash") && question.contains("cargo test"));
        assert_eq!(options, &[ALLOW, DENY]);
    }

    #[test]
    fn allowing_hands_the_input_back_and_denying_says_so() {
        let (events, _) = request(&asked("Bash", json!({"command": "ls"})));
        let AgentEvent::AskUser { id, .. } = &events[0] else {
            panic!()
        };
        let allowed = sent(&response(id, ALLOW).unwrap());
        assert_eq!(allowed["type"], "control_response");
        assert_eq!(allowed["response"]["request_id"], "req-1");
        assert_eq!(allowed["response"]["response"]["behavior"], "allow");
        assert_eq!(
            allowed["response"]["response"]["updatedInput"]["command"],
            "ls"
        );
        let denied = sent(&response(id, DENY).unwrap());
        assert_eq!(denied["response"]["response"]["behavior"], "deny");
        // Anything but the allow choice refuses: a stale or odd answer must
        // never let a command run.
        let odd = sent(&response(id, "sure, whatever").unwrap());
        assert_eq!(odd["response"]["response"]["behavior"], "deny");
    }

    #[test]
    fn the_agents_own_question_keeps_its_choices_and_gets_its_answer_back() {
        let input = json!({"questions": [{
            "question": "Which database?",
            "header": "DB",
            "options": [{"label": "SQLite", "description": "local"}, {"label": "Postgres"}],
            "multiSelect": false,
        }]});
        let (events, _) = request(&asked("AskUserQuestion", input));
        let [
            AgentEvent::AskUser {
                id,
                question,
                options,
            },
        ] = events.as_slice()
        else {
            panic!("expected one question, got {events:?}");
        };
        assert_eq!(question, "Which database?");
        assert_eq!(options, &["SQLite", "Postgres"]);
        let answered = sent(&response(id, "SQLite").unwrap());
        let decision = &answered["response"]["response"];
        assert_eq!(decision["behavior"], "allow");
        assert_eq!(
            decision["updatedInput"]["answers"]["Which database?"],
            "SQLite"
        );
        assert_eq!(decision["updatedInput"]["questions"][0]["header"], "DB");
    }

    #[test]
    fn a_plan_to_leave_plan_mode_is_a_plan_card() {
        let (events, _) = request(&asked("ExitPlanMode", json!({"plan": "1. Read\n2. Fix"})));
        let [AgentEvent::PlanProposal { id, plan }] = events.as_slice() else {
            panic!("expected a plan, got {events:?}");
        };
        assert_eq!(plan, "1. Read\n2. Fix");
        let approved = sent(&response(id, "Approved. Go ahead with this plan.").unwrap());
        assert_eq!(approved["response"]["response"]["behavior"], "allow");
        let rejected = sent(&response(id, "Not approved. Use SQLite instead.").unwrap());
        assert_eq!(rejected["response"]["response"]["behavior"], "deny");
        assert_eq!(
            rejected["response"]["response"]["message"],
            "Not approved. Use SQLite instead."
        );
    }

    #[test]
    fn a_request_this_client_never_offered_is_refused_at_once() {
        let message = json!({
            "type": "control_request",
            "request_id": "req-9",
            "request": {"subtype": "hook_callback", "callback_id": "x"},
        });
        let (events, reply) = request(&message);
        assert!(events.is_empty());
        let reply = sent(&reply.expect("an answer, so the CLI does not wait"));
        assert_eq!(reply["response"]["subtype"], "error");
        assert_eq!(reply["response"]["request_id"], "req-9");
    }

    #[test]
    fn a_long_command_is_cut_for_the_card() {
        let (events, _) = request(&asked("Bash", json!({"command": "x".repeat(2_000)})));
        let AgentEvent::AskUser { question, .. } = &events[0] else {
            panic!()
        };
        assert!(question.chars().count() < 500);
    }
}
