//! The `ginka` command, driven as a user drives it.
//!
//! Every subcommand goes through the daemon, so these tests are also the proof
//! of `AGENTS.md` rule 3: if the UI can do it, the command line can, because
//! there is only one way in. The daemon is started by the CLI itself, with
//! `GINKA_HOME` pointed at a temporary directory.
//!
//! Run them as `cargo test --workspace`. The CLI starts whichever
//! `ginka-daemon` is sitting next to it, and `cargo test -p ginka-cli` does not
//! rebuild another package's binary — against a stale one these fail with an
//! `unsupported` method rather than with anything about the code under test.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const CLI: &str = env!("CARGO_BIN_EXE_ginka-cli");

struct Home {
    dir: tempfile::TempDir,
    work: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            work: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().join("state")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(CLI)
            .args(args)
            .env("GINKA_HOME", self.root())
            .output()
            .expect("the CLI runs")
    }

    /// Run and return stdout, asserting the command succeeded.
    fn ok(&self, args: &[&str]) -> String {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "ginka {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).to_string()
    }

    /// Drive the MCP bridge with a line-delimited conversation, and read the
    /// replies back.
    fn mcp(&self, messages: &[&str]) -> Vec<serde_json::Value> {
        let mut child = Command::new(CLI)
            .arg("mcp")
            .env("GINKA_HOME", self.root())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("the bridge starts");
        {
            let stdin = child.stdin.as_mut().expect("it takes messages");
            for message in messages {
                writeln!(stdin, "{message}").unwrap();
            }
        }
        let output = child.wait_with_output().expect("it finishes");
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("each reply is JSON"))
            .collect()
    }

    /// A repository with one commit, ready to register.
    fn repository(&self, name: &str) -> PathBuf {
        let root = self.work.path().join(name);
        std::fs::create_dir_all(&root).unwrap();
        for args in [
            vec!["init", "--initial-branch=main"],
            vec!["config", "user.email", "test@example.com"],
            vec!["config", "user.name", "Test"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            git(&root, &args);
        }
        std::fs::write(root.join("README.md"), "hello").unwrap();
        git(&root, &["add", "."]);
        git(&root, &["commit", "-m", "first"]);
        root
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Whatever a test started must not outlive it.
        self.run(&["daemon", "stop"]);
    }
}

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .expect("git is on PATH");
    assert!(output.status.success(), "git {args:?}");
}

#[test]
fn doctor_reports_where_state_lives_without_starting_a_daemon() {
    // It is the command a user runs when something is wrong; starting a
    // background process as a side effect of asking would be surprising.
    let home = Home::new();
    let report = home.ok(&["doctor"]);
    assert!(
        report.contains(&home.root().display().to_string()),
        "{report}"
    );
    assert!(report.contains("not running"), "{report}");
    assert!(
        !home.root().join("daemon.json").exists(),
        "doctor must not have started anything"
    );
}

#[test]
fn stored_images_can_be_read_through_the_cli() {
    let home = Home::new();
    let path = home.work.path().join("diagram.bin");
    std::fs::write(&path, b"\x89PNG\r\n\x1a\npreview").unwrap();

    let reference = home.ok(&["attach", path.to_str().unwrap()]);
    let image = home.ok(&["attachment-image", reference.trim()]);
    assert!(image.starts_with("data:image/png;base64,"), "{image}");
    assert_eq!(
        home.ok(&["attachment-image", "ginka-attachment:missing"]),
        ""
    );
}

#[test]
fn the_first_command_starts_a_daemon_and_the_next_one_reuses_it() {
    let home = Home::new();
    home.ok(&["project", "list"]);
    let handshake = home.root().join("daemon.json");
    assert!(handshake.is_file(), "the CLI started a daemon");
    let first = std::fs::read_to_string(&handshake).unwrap();

    home.ok(&["project", "list"]);
    assert_eq!(
        std::fs::read_to_string(&handshake).unwrap(),
        first,
        "the second command must not start a rival daemon"
    );
}

#[test]
fn a_project_registered_through_the_daemon_is_visible_to_the_next_command() {
    let home = Home::new();
    let repository = home.repository("comet");
    let added = home.ok(&["project", "add", repository.to_str().unwrap()]);
    assert!(added.contains("comet"), "{added}");

    let listed = home.ok(&["project", "list"]);
    assert!(listed.contains("comet"), "{listed}");
    assert!(listed.contains("git"), "the kind is shown: {listed}");
}

#[test]
fn a_project_skill_created_by_the_cli_is_visible_and_cannot_be_overwritten() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);

    let args = [
        "skills",
        "create",
        "review-guide",
        "--description",
        "Review a change",
        "--body",
        "Check the tests.",
        "--project",
        "comet",
    ];
    home.ok(&args);
    let path = repository.join(".agents/skills/review-guide/SKILL.md");
    assert!(path.is_file());
    assert!(
        home.ok(&["skills", "list", "--project", "comet"])
            .contains("review-guide")
    );

    let duplicate = home.run(&args);
    assert!(!duplicate.status.success());
    assert_eq!(
        std::fs::read_to_string(path)
            .unwrap()
            .matches("Check the tests.")
            .count(),
        1
    );
}

#[test]
fn an_account_selection_is_persistent_and_visible() {
    let home = Home::new();
    home.ok(&[
        "account",
        "add",
        "codex-work",
        "--provider",
        "codex",
        "--label",
        "Work",
    ]);

    home.ok(&["account", "select", "codex-work"]);
    let selected = home.ok(&["account", "list"]);
    assert!(
        selected
            .lines()
            .any(|line| line.starts_with("* codex-work")),
        "{selected}"
    );

    home.ok(&["daemon", "stop"]);
    let after_restart = home.ok(&["account", "list"]);
    assert!(
        after_restart
            .lines()
            .any(|line| line.starts_with("* codex-work")),
        "{after_restart}"
    );

    home.ok(&["account", "select", "codex"]);
    let restored = home.ok(&["account", "list"]);
    assert!(
        restored.lines().any(|line| line.starts_with("* codex ")),
        "{restored}"
    );
    assert!(
        !restored
            .lines()
            .any(|line| line.starts_with("* codex-work")),
        "{restored}"
    );
}

#[test]
fn a_project_search_finds_paths_and_content_across_its_worktrees() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "second"]);

    std::fs::write(repository.join("needle-main.txt"), "path only\n").unwrap();
    std::fs::write(repository.join("README.md"), "needle on main\n").unwrap();
    let second = home.root().join("worktrees/comet/second");
    std::fs::write(second.join("needle-second.txt"), "path only\n").unwrap();
    std::fs::write(second.join("README.md"), "needle on second\n").unwrap();

    let found = home.ok(&["project", "search", "comet", "needle", "--limit", "2"]);
    assert!(found.contains("comet/main:needle-main.txt"), "{found}");
    assert!(found.contains("comet/second:needle-second.txt"), "{found}");
    assert!(found.contains("comet/main:README.md:1"), "{found}");
    assert!(found.contains("comet/second:README.md:1"), "{found}");
}

#[test]
fn a_workspace_created_on_the_command_line_is_a_real_worktree() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "bright-harbor"]);

    let listed = home.ok(&["workspace", "list"]);
    assert!(listed.contains("comet/bright-harbor"), "{listed}");

    let path = home
        .root()
        .join("worktrees")
        .join("comet")
        .join("bright-harbor");
    assert!(
        path.join("README.md").is_file(),
        "the worktree is checked out"
    );

    home.ok(&["workspace", "remove", "comet", "bright-harbor"]);
    let after = home.ok(&["workspace", "list"]);
    assert!(!after.contains("bright-harbor"), "{after}");
}

#[test]
fn a_workspace_can_be_archived_and_restored_without_removing_it() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "later"]);

    home.ok(&["workspace", "archive", "comet/later"]);
    let archived = home.ok(&["--json", "workspace", "list"]);
    let archived: serde_json::Value = serde_json::from_str(&archived).unwrap();
    let workspace = archived["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workspace| workspace["worktree"]["name"] == "later")
        .expect("workspace remains registered");
    assert_eq!(workspace["worktree"]["archived"], true);

    home.ok(&["workspace", "archive", "comet/later", "--restore"]);
    let restored = home.ok(&["--json", "workspace", "list"]);
    let restored: serde_json::Value = serde_json::from_str(&restored).unwrap();
    let workspace = restored["workspaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|workspace| workspace["worktree"]["name"] == "later")
        .expect("workspace remains registered");
    assert_eq!(workspace["worktree"]["archived"], false);
}

#[test]
fn json_output_is_the_protocols_own_shape_so_an_agent_can_read_it() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);

    let listed = home.ok(&["--json", "project", "list"]);
    let parsed: serde_json::Value = serde_json::from_str(&listed).expect("valid json");
    assert_eq!(parsed["result"], serde_json::json!("projects"));
    assert_eq!(parsed["projects"][0]["name"], serde_json::json!("comet"));
}

#[test]
fn a_command_naming_something_that_does_not_exist_fails_rather_than_printing_nothing() {
    let home = Home::new();
    let output = home.run(&["workspace", "list", "absent"]);
    assert!(!output.status.success(), "an unknown project is an error");
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(complaint.contains("absent"), "{complaint}");
}

#[test]
fn the_daemon_can_be_asked_about_and_stopped() {
    let home = Home::new();
    home.ok(&["project", "list"]);

    let status = home.ok(&["daemon", "status"]);
    assert!(status.contains("running"), "{status}");

    home.ok(&["daemon", "stop"]);
    for _ in 0..100 {
        if !home.root().join("daemon.json").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(!home.root().join("daemon.json").exists());
    assert!(home.ok(&["daemon", "status"]).contains("not running"));
}

#[test]
fn a_scratch_workspace_needs_no_repository() {
    // The "just start an agent" flow: somewhere to work, made on the spot.
    let home = Home::new();
    let made = home.ok(&["workspace", "scratch", "try the parser"]);
    assert!(made.contains("try-the-parser"), "{made}");

    let listed = home.ok(&["workspace", "list"]);
    assert!(listed.contains("try-the-parser"), "{listed}");
    assert!(
        home.root().join("projects").is_dir(),
        "scratch work lives under Ginka's own state"
    );
}

#[test]
fn changes_are_readable_from_the_command_line() {
    // The review half of the loop, without a window: what an agent did to a
    // worktree is the thing a script most often wants next.
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "review-me"]);
    let worktree = home
        .root()
        .join("worktrees")
        .join("comet")
        .join("review-me");

    // Nothing yet.
    assert!(
        home.ok(&["changes", "comet/review-me"])
            .contains("nothing has changed"),
        "a clean worktree says so"
    );

    std::fs::write(worktree.join("README.md"), "rewritten\n").unwrap();
    std::fs::write(worktree.join("added.rs"), "fn new() {}\n").unwrap();

    let summary = home.ok(&["changes", "comet/review-me"]);
    assert!(summary.contains("README.md"), "{summary}");
    assert!(
        summary.contains("added.rs"),
        "an agent's new file is part of what it changed: {summary}"
    );
    assert!(summary.contains("2 file(s)"), "{summary}");

    let patch = home.ok(&["changes", "comet/review-me", "--patch"]);
    assert!(patch.contains("+rewritten"), "the diff itself: {patch}");

    // And the same shapes for a program.
    let json = home.ok(&["--json", "changes", "comet/review-me"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["result"], serde_json::json!("changes"));
    assert_eq!(
        parsed["changes"]["source"]["against"],
        serde_json::json!("uncommitted")
    );
}

#[test]
fn one_hunk_can_be_staged_unstaged_and_discarded_from_the_command_line() {
    let home = Home::new();
    let repository = home.repository("comet");
    let baseline = (1..=30)
        .map(|line| format!("line {line}\n"))
        .collect::<String>();
    std::fs::write(repository.join("README.md"), &baseline).unwrap();
    git(&repository, &["add", "README.md"]);
    git(&repository, &["commit", "-m", "long fixture"]);
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "partial-review"]);
    let worktree = home
        .root()
        .join("worktrees")
        .join("comet")
        .join("partial-review");
    let edited = baseline
        .replace("line 2\n", "line two\n")
        .replace("line 29\n", "line twenty-nine\n");
    std::fs::write(worktree.join("README.md"), edited).unwrap();

    let before = home.ok(&["changes", "comet/partial-review", "--unstaged", "--patch"]);
    let header = before
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("@@"))
        .expect("the CLI prints the exact hunk header")
        .to_string();
    home.ok(&["stage-hunk", "comet/partial-review", "README.md", &header]);

    let staged = home.ok(&["changes", "comet/partial-review", "--staged", "--patch"]);
    let unstaged = home.ok(&["changes", "comet/partial-review", "--unstaged", "--patch"]);
    assert!(staged.contains("line two"), "{staged}");
    assert!(!staged.contains("line twenty-nine"), "{staged}");
    assert!(unstaged.contains("line twenty-nine"), "{unstaged}");
    assert!(!unstaged.contains("line two"), "{unstaged}");

    home.ok(&[
        "stage-hunk",
        "comet/partial-review",
        "README.md",
        &header,
        "--undo",
    ]);
    assert!(
        home.ok(&["changes", "comet/partial-review", "--staged"])
            .contains("nothing has changed")
    );
    let restored = home.ok(&["changes", "comet/partial-review", "--unstaged", "--patch"]);
    assert!(restored.contains("line two"), "{restored}");
    assert!(restored.contains("line twenty-nine"), "{restored}");

    home.ok(&["revert-hunk", "comet/partial-review", "README.md", &header]);
    let left = home.ok(&["changes", "comet/partial-review", "--unstaged", "--patch"]);
    assert!(!left.contains("line two"), "{left}");
    assert!(left.contains("line twenty-nine"), "{left}");
}

#[test]
fn recent_history_is_readable_from_the_command_line() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);

    let history = home.ok(&["history", "comet/main", "--limit", "1"]);
    assert!(history.contains("Test"), "{history}");
    assert!(history.contains("first"), "{history}");

    let json = home.ok(&["--json", "history", "comet/main", "--limit", "1"]);
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["result"], serde_json::json!("history"));
    assert_eq!(parsed["commits"].as_array().unwrap().len(), 1);
    assert!(
        parsed["commits"][0]["parents"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn work_can_be_committed_once_it_has_been_read() {
    // The other half of the review loop: read the diff, then commit it,
    // without leaving the tool.
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "commit-me"]);
    let worktree = home
        .root()
        .join("worktrees")
        .join("comet")
        .join("commit-me");
    std::fs::write(worktree.join("added.rs"), "fn new() {}\n").unwrap();

    let committed = home.ok(&["commit", "comet/commit-me", "the agent's work"]);
    assert!(committed.contains("committed"), "{committed}");

    // Everything the review listed went in, including the file git had never
    // seen, and nothing is left behind.
    assert!(
        home.ok(&["changes", "comet/commit-me"])
            .contains("nothing has changed"),
        "the worktree is clean afterwards"
    );
    let listed = home.ok(&["workspace", "list", "comet"]);
    assert!(listed.contains("clean"), "{listed}");
}

#[test]
fn committing_nothing_says_what_git_said() {
    // The message is on git's stdout, and an error with no message is not
    // something a user can act on.
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "empty"]);

    let output = home.run(&["commit", "comet/empty", "nothing to say"]);
    assert!(!output.status.success());
    let complaint = String::from_utf8_lossy(&output.stderr);
    assert!(complaint.contains("nothing to commit"), "{complaint}");
}

#[test]
fn a_workspaces_files_are_searchable_by_any_part_of_their_path() {
    // What `@` in the composer reaches for.
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "files"]);
    let worktree = home.root().join("worktrees").join("comet").join("files");
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::write(worktree.join("src/parser.rs"), "fn parse() {}\n").unwrap();
    std::fs::write(worktree.join(".gitignore"), "secret.txt\n").unwrap();
    std::fs::write(worktree.join("secret.txt"), "shh\n").unwrap();

    let all = home.ok(&["files", "comet/files"]);
    assert!(
        all.contains("src/parser.rs"),
        "a file written now is offered: {all}"
    );
    assert!(
        !all.contains("secret.txt"),
        "what the project ignores is not offered: {all}"
    );

    // Any subsequence of the path will do.
    let found = home.ok(&["files", "comet/files", "prsr"]);
    assert!(found.contains("src/parser.rs"), "{found}");
}

#[test]
fn a_file_can_be_saved_from_the_revision_the_cli_read() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "edit"]);

    let opened = home.ok(&["--json", "show", "comet/edit", "README.md"]);
    let opened: serde_json::Value = serde_json::from_str(&opened).unwrap();
    let revision = opened["file"]["revision"].as_str().unwrap();
    home.ok(&[
        "save",
        "comet/edit",
        "README.md",
        "--expected-revision",
        revision,
        "--text",
        "edited\n",
    ]);
    let worktree = home.root().join("worktrees/comet/edit/README.md");
    assert_eq!(std::fs::read_to_string(worktree).unwrap(), "edited\n");

    let stale = home.run(&[
        "save",
        "comet/edit",
        "README.md",
        "--expected-revision",
        revision,
        "--text",
        "lost\n",
    ]);
    assert!(!stale.status.success());
    assert!(String::from_utf8_lossy(&stale.stderr).contains("changed since it was opened"));
}

#[test]
fn searching_conversations_that_do_not_exist_yet_is_an_empty_answer() {
    // Not an error: a user searching a fresh install has asked a reasonable
    // question and the answer is "nothing".
    let home = Home::new();
    let found = home.ok(&["session", "search", "anything"]);
    assert!(found.contains("nothing said that"), "{found}");
}

#[test]
fn sessions_and_checkpoints_are_listable_before_any_agent_has_run() {
    // An empty list is an answer; a user asking what is going on in a fresh
    // workspace should not meet an error.
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "harbor"]);

    let sessions = home.ok(&["session", "list"]);
    assert!(sessions.contains("no sessions"), "{sessions}");
    let checkpoints = home.ok(&["checkpoint", "list", "comet/harbor"]);
    assert!(checkpoints.contains("no checkpoints"), "{checkpoints}");
}

#[test]
fn an_agent_can_drive_ginka_over_mcp() {
    // M4's exit criterion, and rule 3's third client: what a person can do
    // from the command line, an agent can do through a tool call, because
    // both are the same request.
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);

    let replies = home.mcp(&[
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"ginka_workspace_create","arguments":{"project":"comet","branch":"harbor"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"ginka_workspaces","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ginka_read_file","arguments":{"workspace":"comet/harbor","path":"README.md"}}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"ginka_nonsense","arguments":{}}}"#,
    ]);

    // The notification is not answered: a client waiting for a reply to one
    // would wait forever.
    let ids: Vec<u64> = replies
        .iter()
        .filter_map(|reply| reply["id"].as_u64())
        .collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5, 6], "{replies:#?}");

    assert_eq!(replies[0]["result"]["serverInfo"]["name"], "ginka");
    let tools = replies[1]["result"]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool["name"] == "ginka_session_start"),
        "starting a sibling agent is the point of the surface"
    );

    let created = replies[2]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(created.contains("comet/harbor"), "{created}");
    let listed = replies[3]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(listed.contains("harbor"), "{listed}");
    let read = replies[4]["result"]["content"][0]["text"].as_str().unwrap();
    assert!(read.contains("README"), "{read}");

    // A call the agent got wrong is a result that says so, not a dead
    // connection.
    assert_eq!(replies[5]["result"]["isError"], true);
    assert!(
        replies[5]["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("ginka_nonsense")
    );
}

#[test]
fn a_workspace_is_merged_into_the_branch_the_project_is_on() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "try-1"]);
    let attempt = home.root().join("worktrees/comet/try-1");
    std::fs::write(attempt.join("answer.txt"), "42\n").unwrap();

    let refused = home.run(&["workspace", "merge", "comet/try-1"]);
    assert!(
        !refused.status.success(),
        "uncommitted work needs a message"
    );

    let merged = home.ok(&[
        "workspace",
        "merge",
        "comet/try-1",
        "--message",
        "Keep try-1",
    ]);
    assert!(merged.contains("main"), "{merged}");
    assert!(repository.join("answer.txt").exists());
}

#[test]
fn a_scheduled_job_is_added_listed_run_and_removed() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);

    let refused = home.run(&[
        "cron",
        "add",
        "comet",
        "nightly",
        "--schedule",
        "61 * * * *",
        "--shell",
        "true",
    ]);
    assert!(!refused.status.success());

    let added = home.ok(&[
        "cron",
        "add",
        "comet",
        "nightly",
        "--schedule",
        "@daily",
        "--shell",
        "touch .cron-ran",
    ]);
    let id = added
        .split_whitespace()
        .next()
        .expect("the id comes first")
        .to_string();
    let listed = home.ok(&["cron", "list"]);
    assert!(
        listed.contains("nightly") && listed.contains("@daily"),
        "{listed}"
    );

    home.ok(&["cron", "run", &id]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !repository.join(".cron-ran").exists() {
        assert!(std::time::Instant::now() < deadline, "the job never ran");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let runs = home.ok(&["cron", "runs", &id]);
    assert!(
        runs.contains("started") || runs.contains("finished"),
        "{runs}"
    );

    home.ok(&["cron", "remove", &id]);
    assert!(!home.ok(&["cron", "list"]).contains("nightly"));
}

#[test]
fn a_one_time_job_can_be_added_with_at_but_not_two_schedules() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    let at = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
    let added = home.ok(&[
        "cron", "add", "comet", "reminder", "--at", &at, "--shell", "true",
    ]);
    assert!(!added.is_empty());
    let listed = home.ok(&["cron", "list"]);
    assert!(listed.contains(&format!("@once {at}")), "{listed}");

    let both = home.run(&[
        "cron",
        "add",
        "comet",
        "invalid",
        "--at",
        &at,
        "--schedule",
        "@daily",
        "--shell",
        "true",
    ]);
    assert!(!both.status.success());
    let neither = home.run(&["cron", "add", "comet", "invalid", "--shell", "true"]);
    assert!(!neither.status.success());
}

#[test]
fn a_project_can_be_labelled_and_moved() {
    let home = Home::new();
    for name in ["comet", "aurora"] {
        let repository = home.repository(name);
        home.ok(&["project", "add", repository.to_str().unwrap()]);
    }
    home.ok(&["project", "move", "aurora", "0"]);
    home.ok(&["project", "label", "comet", "Side projects"]);
    let listed = home.ok(&["project", "list"]);
    let aurora = listed.find("aurora").expect("listed");
    let comet = listed.find("comet").expect("listed");
    assert!(aurora < comet, "moved first: {listed}");
    assert!(listed.contains("Side projects"), "{listed}");
    assert!(
        !home
            .run(&["project", "move", "nowhere", "0"])
            .status
            .success()
    );
}

#[test]
fn a_terminal_is_opened_written_to_read_and_closed_from_the_cli() {
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    let id = home
        .ok(&["terminal", "open", "comet/main"])
        .trim()
        .to_string();
    assert!(!id.is_empty());
    assert!(home.ok(&["terminal", "list", "comet/main"]).contains(&id));

    home.ok(&["terminal", "send", &id, "echo hi-from-$((40+2))"]);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let read = home.ok(&["terminal", "read", &id]);
        if read.contains("hi-from-42") {
            assert!(
                !read.contains('\u{1b}'),
                "escape sequences are stripped: {read:?}"
            );
            break;
        }
        assert!(std::time::Instant::now() < deadline, "never ran: {read}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    home.ok(&["terminal", "close", &id]);
    assert!(!home.ok(&["terminal", "list", "comet/main"]).contains(&id));
}

#[test]
fn a_setting_is_read_and_changed_from_the_cli() {
    let home = Home::new();
    assert!(
        home.ok(&["settings", "show"])
            .contains("\"keep_awake\": true")
    );
    home.ok(&["settings", "set", "keep_awake", "false"]);
    home.ok(&["settings", "set", "retention_days", "7"]);
    let shown = home.ok(&["settings", "show"]);
    assert!(shown.contains("\"keep_awake\": false"), "{shown}");
    assert!(shown.contains("\"retention_days\": 7"), "{shown}");
    assert!(
        !home
            .run(&["settings", "set", "no_such_setting", "1"])
            .status
            .success()
    );
    // Written where the daemon keeps it, and taken without a restart.
    let file = std::fs::read_to_string(home.root().join("settings.json")).unwrap();
    assert!(file.contains("\"retention_days\": 7"), "{file}");
}

/// The commands `ginka --help` (or `ginka <command> --help`) lists.
fn listed_commands(args: &[&str]) -> Vec<String> {
    let output = Command::new(CLI)
        .args(args)
        .arg("--help")
        .output()
        .expect("the CLI runs");
    let help = String::from_utf8_lossy(&output.stdout).to_string();
    help.lines()
        .skip_while(|line| !line.starts_with("Commands:"))
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| *name != "help")
        .map(str::to_string)
        .collect()
}

#[test]
fn every_command_is_in_the_cli_reference() {
    // docs/cli.md is what a reader is sent to; a command added without it is
    // one they cannot find.
    let reference = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/cli.md"),
    )
    .unwrap();
    let mut missing = Vec::new();
    for command in listed_commands(&[]) {
        let subcommands = listed_commands(&[command.as_str()]);
        if subcommands.is_empty() && !reference.contains(&format!("`ginka {command}")) {
            missing.push(command.clone());
        }
        for subcommand in subcommands {
            if !reference.contains(&format!("`ginka {command} {subcommand}`")) {
                missing.push(format!("{command} {subcommand}"));
            }
        }
    }
    assert!(missing.is_empty(), "missing from docs/cli.md: {missing:?}");
}

#[test]
fn a_refused_commit_is_handed_to_the_agent_with_fix_with_agent() {
    // What the app's "Ask the agent to fix it" does, from a terminal: the
    // refusal is printed, and a conversation in the workspace is asked to
    // sort it out.
    let home = Home::new();
    let repository = home.repository("comet");
    home.ok(&["project", "add", repository.to_str().unwrap()]);
    home.ok(&["workspace", "new", "comet", "refused"]);
    // Never a real agent in a test: the conversation starts, and the
    // "agent" exits at once.
    home.ok(&[
        "settings",
        "provider",
        "claude",
        "--program",
        "/usr/bin/false",
    ]);
    let hooks = home.root().join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    let hook = hooks.join("pre-commit");
    std::fs::write(
        &hook,
        "#!/bin/sh\necho 'lint: trailing space' >&2\nexit 1\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    git(
        &repository,
        &["config", "core.hooksPath", hooks.to_str().unwrap()],
    );
    let worktree = home.root().join("worktrees").join("comet").join("refused");
    std::fs::write(worktree.join("added.rs"), "fn new() {} \n").unwrap();

    let without = home.run(&["commit", "comet/refused", "add it"]);
    assert!(!without.status.success(), "the hook refused it");
    assert!(
        !home.ok(&["session", "list"]).contains("comet/refused"),
        "a plain commit hands nothing over"
    );

    let handed = home.run(&["commit", "comet/refused", "add it", "--fix-with-agent"]);
    let said = String::from_utf8_lossy(&handed.stdout).to_string()
        + &String::from_utf8_lossy(&handed.stderr);
    assert!(said.contains("lint: trailing space"), "{said}");
    assert!(said.contains("asked to fix"), "{said}");
    assert!(
        home.ok(&["session", "list"]).contains("comet/refused"),
        "a conversation was started in the workspace"
    );
}
