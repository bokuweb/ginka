//! What the settings file says about chat connectors.
//!
//! Bindings are configuration, not state, so they live in `settings.json`
//! beside the agent overrides (`docs/connectors.md` §4.2). Everything here is
//! validated at load rather than trusted: a misspelt key or a binding that
//! cannot work is said out loud, because the alternative is a security
//! setting silently dropped.

use ginka_protocol::model::ConnectorBinding;
use ginka_protocol::provider::AccessMode;
use ginka_protocol::{AccountId, ProjectName, WorkspaceId};
use serde::{Deserialize, Serialize};

/// The `connectors` object of the daemon's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectorsSettings {
    /// The Slack connector, when the user has written one down at all.
    pub slack: Option<SlackSettings>,
}

/// How many turns one sender may start in an hour unless told otherwise.
///
/// Enough for a working day of asks; small enough that a compromised member
/// account is a nuisance rather than a bill (`docs/connectors.md` §8).
pub const DEFAULT_TURNS_PER_HOUR: u32 = 30;

/// The Slack connector's configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SlackSettings {
    /// Off means the daemon never dials Slack, tokens or not.
    pub enabled: bool,
    /// Member ids that may speak to the bot. **Empty means deny everyone**:
    /// inviting the bot to a channel must not, by itself, let the channel
    /// drive it, and there is no `allow_all`.
    pub allowed_users: Vec<String>,
    /// The subset that may answer questions, approve plans and grant
    /// permissions. `None` means everyone in `allowed_users`. Split from it
    /// because "anyone who can reply can approve" is a stronger grant than
    /// "anyone who can ask".
    pub approvers: Option<Vec<String>>,
    /// Per-sender budget of turns per hour.
    pub turns_per_hour: u32,
    /// The channels the bot listens in.
    pub bindings: Vec<Binding>,
}

impl Default for SlackSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            allowed_users: Vec::new(),
            approvers: None,
            turns_per_hour: DEFAULT_TURNS_PER_HOUR,
            bindings: Vec::new(),
        }
    }
}

/// When a root message in a bound channel starts a conversation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// Only when the bot is mentioned. Replies inside a thread the bot
    /// already owns never need one.
    #[default]
    Mention,
    /// Every root message, for a channel that exists only for this.
    All,
}

/// Where a binding's conversations run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeMode {
    /// The project's own checkout, the same rule a chat from the window
    /// follows. Two agents editing one checkout is a merge, not concurrency,
    /// so this mode runs one turn at a time.
    #[default]
    Shared,
    /// A worktree per thread, named after it. Opt-in, because it leaves
    /// worktrees behind that someone has to remove.
    PerThread,
}

/// How the thread is told what the agent is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Progress {
    /// One message, edited in place as the agent works.
    #[default]
    Edit,
    /// Nothing until the reply.
    Off,
}

/// One channel the bot listens in, and what a message there becomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Binding {
    /// The conversation id, never the name: names are renamed.
    pub channel: String,
    /// The project whose own checkout conversations run in. Exactly one of
    /// this and `workspace`.
    pub project: Option<ProjectName>,
    /// A specific workspace to run in.
    pub workspace: Option<WorkspaceId>,
    /// The driver id: `claude`, `codex`.
    pub agent: String,
    pub model: Option<String>,
    /// Which login to run on; the provider's active account when absent.
    pub account: Option<AccountId>,
    /// The ceiling for this channel. `auto` has to be written down here in
    /// plain text; nothing said in Slack can raise it.
    pub access_mode: AccessMode,
    pub trigger: Trigger,
    pub worktree: WorktreeMode,
    /// Live turns at once. A `shared` binding must say `1`.
    pub max_concurrent: u32,
    pub progress: Progress,
    /// Delete the progress message when the reply lands, rather than
    /// leaving it collapsed above it.
    pub cleanup_progress: bool,
}

impl Default for Binding {
    fn default() -> Self {
        Self {
            channel: String::new(),
            project: None,
            workspace: None,
            agent: "claude".to_string(),
            model: None,
            account: None,
            access_mode: AccessMode::Ask,
            trigger: Trigger::Mention,
            worktree: WorktreeMode::Shared,
            max_concurrent: 1,
            progress: Progress::Edit,
            cleanup_progress: true,
        }
    }
}

impl Binding {
    /// Where conversations run, as one string a client can print.
    pub fn target(&self) -> String {
        match (&self.workspace, &self.project) {
            (Some(workspace), _) => workspace.0.clone(),
            (None, Some(project)) => project.0.clone(),
            (None, None) => String::new(),
        }
    }

    /// The binding as a client sees it.
    pub fn to_wire(&self) -> ConnectorBinding {
        ConnectorBinding {
            channel: self.channel.clone(),
            target: self.target(),
            agent: self.agent.clone(),
            trigger: match self.trigger {
                Trigger::Mention => "mention",
                Trigger::All => "all",
            }
            .to_string(),
            worktree: match self.worktree {
                WorktreeMode::Shared => "shared",
                WorktreeMode::PerThread => "per_thread",
            }
            .to_string(),
        }
    }
}

impl SlackSettings {
    /// Who may answer questions and approve plans.
    pub fn approvers(&self) -> &[String] {
        self.approvers.as_deref().unwrap_or(&self.allowed_users)
    }

    /// Whether a member may speak to the bot at all.
    pub fn is_allowed(&self, sender: &str) -> bool {
        self.allowed_users.iter().any(|allowed| allowed == sender)
    }

    /// Whether a member may answer a question the agent asked.
    ///
    /// An approver who is not also allowed to speak cannot approve either:
    /// the allowlist is the outer gate.
    pub fn may_approve(&self, sender: &str) -> bool {
        self.is_allowed(sender) && self.approvers().iter().any(|approver| approver == sender)
    }

    /// The binding for a channel, if the bot listens there.
    pub fn binding_for(&self, channel: &str) -> Option<(usize, &Binding)> {
        self.bindings
            .iter()
            .enumerate()
            .find(|(_, binding)| binding.channel == channel)
    }

    /// Everything wrong with this configuration, one line each.
    ///
    /// Empty means it can run. A connector with problems does not start,
    /// and the lines are what `ginka slack status` shows.
    pub fn validate(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if self.turns_per_hour == 0 {
            problems.push("turns_per_hour is 0, which would refuse every message".to_string());
        }
        let mut channels = std::collections::HashSet::new();
        for (index, binding) in self.bindings.iter().enumerate() {
            let at = format!("bindings[{index}]");
            if binding.channel.trim().is_empty() {
                problems.push(format!("{at}: channel is empty"));
            } else if !channels.insert(binding.channel.as_str()) {
                problems.push(format!(
                    "{at}: channel {} is bound more than once",
                    binding.channel
                ));
            }
            match (&binding.project, &binding.workspace) {
                (None, None) => {
                    problems.push(format!("{at}: names neither a project nor a workspace"))
                }
                (Some(_), Some(_)) => problems.push(format!(
                    "{at}: names both a project and a workspace; pick one"
                )),
                _ => {}
            }
            if binding.agent.trim().is_empty() {
                problems.push(format!("{at}: agent is empty"));
            }
            if binding.max_concurrent == 0 {
                problems.push(format!(
                    "{at}: max_concurrent is 0, which would run nothing"
                ));
            }
            if binding.worktree == WorktreeMode::Shared && binding.max_concurrent > 1 {
                problems.push(format!(
                    "{at}: a shared worktree runs one turn at a time; max_concurrent {} would \
                     have two agents editing one checkout",
                    binding.max_concurrent
                ));
            }
        }
        problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn binding(channel: &str) -> Binding {
        Binding {
            channel: channel.to_string(),
            project: Some(ProjectName("comet".into())),
            ..Binding::default()
        }
    }

    #[test]
    fn an_empty_allowlist_denies_everyone() {
        // Inviting the bot to a channel is not a grant.
        let settings = SlackSettings::default();
        assert!(!settings.is_allowed("U1"));
        assert!(!settings.may_approve("U1"));
    }

    #[test]
    fn approvers_default_to_the_allowlist_and_never_exceed_it() {
        let mut settings = SlackSettings {
            allowed_users: vec!["U1".into(), "U2".into()],
            ..SlackSettings::default()
        };
        assert!(settings.may_approve("U2"));
        settings.approvers = Some(vec!["U1".into(), "U9".into()]);
        assert!(settings.may_approve("U1"));
        assert!(!settings.may_approve("U2"), "asking is not approving");
        assert!(
            !settings.may_approve("U9"),
            "the allowlist is the outer gate"
        );
    }

    #[test]
    fn a_shared_worktree_with_more_than_one_turn_is_rejected_with_the_reason() {
        let settings = SlackSettings {
            bindings: vec![Binding {
                max_concurrent: 3,
                ..binding("C1")
            }],
            ..SlackSettings::default()
        };
        let problems = settings.validate();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("one checkout"));

        let per_thread = SlackSettings {
            bindings: vec![Binding {
                max_concurrent: 3,
                worktree: WorktreeMode::PerThread,
                ..binding("C1")
            }],
            ..SlackSettings::default()
        };
        assert!(per_thread.validate().is_empty());
    }

    #[test]
    fn a_binding_names_exactly_one_place_to_run() {
        let neither = SlackSettings {
            bindings: vec![Binding {
                project: None,
                ..binding("C1")
            }],
            ..SlackSettings::default()
        };
        assert!(neither.validate()[0].contains("neither"));
        let both = SlackSettings {
            bindings: vec![Binding {
                workspace: Some(WorkspaceId("comet/harbor".into())),
                ..binding("C1")
            }],
            ..SlackSettings::default()
        };
        assert!(both.validate()[0].contains("both"));
    }

    #[test]
    fn a_channel_bound_twice_is_a_problem() {
        let settings = SlackSettings {
            bindings: vec![binding("C1"), binding("C1")],
            ..SlackSettings::default()
        };
        assert!(settings.validate()[0].contains("more than once"));
    }

    #[test]
    fn a_misspelt_key_is_refused_rather_than_dropped() {
        // `allowed_user` instead of `allowed_users` would otherwise deny
        // everyone with no explanation -- or, worse, an `approver` typo would
        // widen who may approve.
        let parsed = serde_json::from_str::<SlackSettings>(r#"{"allowed_user":["U1"]}"#);
        assert!(parsed.is_err());
        let parsed = serde_json::from_str::<Binding>(r#"{"channel":"C1","projet":"comet"}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn the_documented_example_parses_with_its_defaults() {
        let settings: SlackSettings = serde_json::from_str(
            r#"{"enabled":true,"allowed_users":["U01ABC2DEF3"],
                "bindings":[{"channel":"C0123456789","project":"ginka","agent":"claude",
                             "access_mode":"ask","trigger":"mention","worktree":"shared"}]}"#,
        )
        .expect("the example in docs/connectors.md parses");
        assert_eq!(settings.turns_per_hour, DEFAULT_TURNS_PER_HOUR);
        let (index, binding) = settings.binding_for("C0123456789").expect("bound");
        assert_eq!(index, 0);
        assert_eq!(binding.max_concurrent, 1);
        assert!(binding.cleanup_progress);
        assert_eq!(binding.to_wire().target, "ginka");
        assert!(settings.validate().is_empty());
    }
}
