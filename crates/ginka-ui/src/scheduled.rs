//! Request shape for the scheduled-job settings form.
//!
//! Editing changes the visible fields while retaining a job's agent, enabled
//! state and run history identity.

use ginka_protocol::model::{CronJob, CronVia};
use ginka_protocol::rpc::Request;
use ginka_protocol::{ProjectName, WorkspaceId};

/// Values the scheduled-job form lets the reader change.
pub struct FormValues {
    /// Worktree to run in, or the project checkout when absent.
    pub workspace: Option<WorkspaceId>,
    /// Job label shown in Settings.
    pub name: String,
    /// Cron expression or one-time timestamp.
    pub schedule: String,
    /// Command or agent prompt to run.
    pub body: String,
    /// Optional shell command whose failure skips a scheduled firing.
    pub precheck: String,
    /// Whether to start an agent conversation or a terminal command.
    pub via: CronVia,
}

/// Build a save request from the settings form without dropping hidden fields.
///
/// A chat job keeps its original agent while it is edited; changing a terminal
/// job to chat uses the currently selected agent. The daemon retains run
/// history by updating the same job id.
pub fn save_request(
    project: ProjectName,
    editing: Option<&CronJob>,
    form: FormValues,
    selected_agent: Option<String>,
) -> Request {
    let agent = match form.via {
        CronVia::Chat => editing
            .filter(|job| job.via == CronVia::Chat)
            .and_then(|job| job.agent.clone())
            .or(selected_agent),
        CronVia::Terminal => None,
    };
    Request::SaveCronJob {
        id: editing.map(|job| job.id),
        project,
        workspace: form.workspace,
        // Settings does not set reminders; editing one keeps its target.
        session: editing.and_then(|job| job.session.clone()),
        name: form.name,
        schedule: form.schedule,
        via: form.via,
        agent,
        body: form.body,
        precheck: (!form.precheck.trim().is_empty()).then_some(form.precheck),
        enabled: editing.is_none_or(|job| job.enabled),
    }
}

#[cfg(test)]
mod tests {
    use ginka_protocol::ProjectName;
    use ginka_protocol::WorkspaceId;
    use ginka_protocol::model::{CronJob, CronVia};
    use ginka_protocol::rpc::Request;

    use super::{FormValues, save_request};

    #[test]
    fn editing_keeps_hidden_job_fields_and_identity() {
        let project = ProjectName("shop".into());
        let workspace = WorkspaceId("shop-review".into());
        let job = CronJob {
            id: 42,
            project: project.clone(),
            workspace: Some(workspace.clone()),
            session: None,
            name: "review".into(),
            schedule: "@daily".into(),
            via: CronVia::Chat,
            agent: Some("claude".into()),
            body: "old prompt".into(),
            enabled: false,
            precheck: Some("gh pr list".into()),
            next_run_at: None,
            last_run: None,
        };
        let request = save_request(
            project.clone(),
            Some(&job),
            FormValues {
                workspace: Some(workspace.clone()),
                name: "Review open PRs".into(),
                schedule: "0 9 * * 1-5".into(),
                body: "new prompt".into(),
                precheck: "gh pr list --state open".into(),
                via: CronVia::Chat,
            },
            Some("codex".into()),
        );
        assert!(matches!(request, Request::SaveCronJob {
            id: Some(42), project: p, workspace: Some(w), session: None, name, schedule,
            via: CronVia::Chat, agent: Some(agent), body,
            precheck: Some(precheck), enabled: false,
        } if p == project && w == workspace && name == "Review open PRs"
            && schedule == "0 9 * * 1-5" && agent == "claude"
            && body == "new prompt" && precheck == "gh pr list --state open"));
    }

    #[test]
    fn new_job_uses_the_selected_agent_and_starts_enabled() {
        let request = save_request(
            ProjectName("shop".into()),
            None,
            FormValues {
                workspace: None,
                name: "review".into(),
                schedule: "@daily".into(),
                body: "look at the diff".into(),
                precheck: String::new(),
                via: CronVia::Chat,
            },
            Some("codex".into()),
        );
        assert!(matches!(request, Request::SaveCronJob {
            id: None, workspace: None, agent: Some(agent),
            session: None,
            precheck: None, enabled: true, ..
        } if agent == "codex"));
    }

    #[test]
    fn changing_how_a_job_runs_sets_or_clears_its_agent() {
        let project = ProjectName("shop".into());
        let mut job = CronJob {
            id: 9,
            project: project.clone(),
            workspace: None,
            session: None,
            name: "review".into(),
            schedule: "@daily".into(),
            via: CronVia::Terminal,
            agent: None,
            body: "true".into(),
            enabled: true,
            precheck: None,
            next_run_at: None,
            last_run: None,
        };
        let to_chat = save_request(
            project.clone(),
            Some(&job),
            FormValues {
                workspace: None,
                name: job.name.clone(),
                schedule: job.schedule.clone(),
                body: "review".into(),
                precheck: String::new(),
                via: CronVia::Chat,
            },
            Some("codex".into()),
        );
        assert!(matches!(to_chat, Request::SaveCronJob {
            agent: Some(agent), ..
        } if agent == "codex"));

        job.via = CronVia::Chat;
        job.agent = Some("claude".into());
        let to_terminal = save_request(
            project,
            Some(&job),
            FormValues {
                workspace: None,
                name: job.name.clone(),
                schedule: job.schedule.clone(),
                body: "true".into(),
                precheck: String::new(),
                via: CronVia::Terminal,
            },
            Some("codex".into()),
        );
        assert!(matches!(
            to_terminal,
            Request::SaveCronJob { agent: None, .. }
        ));
    }

    #[test]
    fn clearing_a_precheck_removes_it_from_an_existing_job() {
        let project = ProjectName("shop".into());
        let job = CronJob {
            id: 7,
            project: project.clone(),
            workspace: None,
            session: None,
            name: "review".into(),
            schedule: "@daily".into(),
            via: CronVia::Terminal,
            agent: None,
            body: "true".into(),
            enabled: true,
            precheck: Some("old probe".into()),
            next_run_at: None,
            last_run: None,
        };
        let request = save_request(
            project,
            Some(&job),
            FormValues {
                workspace: None,
                name: job.name.clone(),
                schedule: job.schedule.clone(),
                body: job.body.clone(),
                precheck: "   ".into(),
                via: CronVia::Terminal,
            },
            None,
        );
        assert!(matches!(
            request,
            Request::SaveCronJob {
                id: Some(7),
                precheck: None,
                ..
            }
        ));
    }

    #[test]
    fn new_job_can_target_a_workspace() {
        let workspace = WorkspaceId("shop-review".into());
        let request = save_request(
            ProjectName("shop".into()),
            None,
            FormValues {
                workspace: Some(workspace.clone()),
                name: "review".into(),
                schedule: "@daily".into(),
                body: "true".into(),
                precheck: String::new(),
                via: CronVia::Terminal,
            },
            None,
        );
        assert!(matches!(request, Request::SaveCronJob {
            id: None, workspace: Some(actual), ..
        } if actual == workspace));
    }

    #[test]
    fn editing_can_change_or_clear_the_workspace() {
        let project = ProjectName("shop".into());
        let job = CronJob {
            id: 7,
            project: project.clone(),
            workspace: Some(WorkspaceId("shop-old".into())),
            session: None,
            name: "review".into(),
            schedule: "@daily".into(),
            via: CronVia::Terminal,
            agent: None,
            body: "true".into(),
            enabled: true,
            precheck: None,
            next_run_at: None,
            last_run: None,
        };
        let form = |workspace| FormValues {
            workspace,
            name: job.name.clone(),
            schedule: job.schedule.clone(),
            body: job.body.clone(),
            precheck: String::new(),
            via: job.via,
        };
        let changed = save_request(
            project.clone(),
            Some(&job),
            form(Some(WorkspaceId("shop-new".into()))),
            None,
        );
        assert!(matches!(changed, Request::SaveCronJob {
            id: Some(7), workspace: Some(actual), ..
        } if actual == WorkspaceId("shop-new".into())));
        let cleared = save_request(project, Some(&job), form(None), None);
        assert!(matches!(
            cleared,
            Request::SaveCronJob {
                id: Some(7),
                workspace: None,
                session: None,
                ..
            }
        ));
    }
}
