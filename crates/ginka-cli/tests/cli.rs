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

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

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
