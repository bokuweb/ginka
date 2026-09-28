//! Request shape for the scheduled-job settings form.
//!
//! Editing changes the visible fields while retaining a job's scope, agent,
//! precheck, enabled state and run history identity.

use ginka_protocol::ProjectName;
use ginka_protocol::model::{CronJob, CronVia};
use ginka_protocol::rpc::Request;

/// Build a save request without dropping fields absent from the settings form.
///
/// A chat job keeps its original agent while it is edited; changing a terminal
/// job to chat uses the currently selected agent. The daemon retains run
/// history by updating the same job id.
pub fn save_request(
    project: ProjectName,
    editing: Option<&CronJob>,
    name: String,
    schedule: String,
    body: String,
    via: CronVia,
    selected_agent: Option<String>,
) -> Request {
    let agent = match via {
        CronVia::Chat => editing
            .filter(|job| job.via == CronVia::Chat)
            .and_then(|job| job.agent.clone())
            .or(selected_agent),
        CronVia::Terminal => None,
    };
    Request::SaveCronJob {
        id: editing.map(|job| job.id),
        project,
        workspace: editing.and_then(|job| job.workspace.clone()),
        name,
        schedule,
        via,
        agent,
        body,
        precheck: editing.and_then(|job| job.precheck.clone()),
        enabled: editing.is_none_or(|job| job.enabled),
    }
}

#[cfg(test)]
mod tests {
    use ginka_protocol::ProjectName;
    use ginka_protocol::WorkspaceId;
    use ginka_protocol::model::{CronJob, CronVia};
    use ginka_protocol::rpc::Request;

    use super::save_request;

    #[test]
    fn editing_keeps_hidden_job_fields_and_identity() {
        let project = ProjectName("shop".into());
        let workspace = WorkspaceId("shop-review".into());
        let job = CronJob {
            id: 42,
            project: project.clone(),
            workspace: Some(workspace.clone()),
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
            "Review open PRs".into(),
            "0 9 * * 1-5".into(),
            "new prompt".into(),
            CronVia::Chat,
            Some("codex".into()),
        );
        assert!(matches!(request, Request::SaveCronJob {
            id: Some(42), project: p, workspace: Some(w), name, schedule,
            via: CronVia::Chat, agent: Some(agent), body,
            precheck: Some(precheck), enabled: false,
        } if p == project && w == workspace && name == "Review open PRs"
            && schedule == "0 9 * * 1-5" && agent == "claude"
            && body == "new prompt" && precheck == "gh pr list"));
    }

    #[test]
    fn new_job_uses_the_selected_agent_and_starts_enabled() {
        let request = save_request(
            ProjectName("shop".into()),
            None,
            "review".into(),
            "@daily".into(),
            "look at the diff".into(),
            CronVia::Chat,
            Some("codex".into()),
        );
        assert!(matches!(request, Request::SaveCronJob {
            id: None, workspace: None, agent: Some(agent),
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
            job.name.clone(),
            job.schedule.clone(),
            "review".into(),
            CronVia::Chat,
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
            job.name.clone(),
            job.schedule.clone(),
            "true".into(),
            CronVia::Terminal,
            Some("codex".into()),
        );
        assert!(matches!(
            to_terminal,
            Request::SaveCronJob { agent: None, .. }
        ));
    }
}
