//! Daemon-host external editor launch for a validated workspace file.
//!
//! Editor settings are tokenized as arguments and never run through a shell.
//! This action opens on the daemon host, which can differ from a client's host.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

/// Launch the configured editor at a worktree file and optional one-based line.
pub fn open(worktree: &Path, path: &str, line: Option<u32>) -> Result<()> {
    if line == Some(0) {
        bail!("editor line must be one-based");
    }
    let file = crate::files::resolve_file(worktree, path)?;
    if let Some(setting) = ["GINKA_EDITOR", "VISUAL", "EDITOR"]
        .into_iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
    {
        let words = shlex::split(&setting).ok_or_else(|| anyhow!("invalid editor setting"))?;
        let (program, arguments) = words
            .split_first()
            .ok_or_else(|| anyhow!("editor setting is empty"))?;
        let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
        return launch(program, &arguments, &file, line);
    }

    let candidates: &[(&str, &[&str])] = &[
        ("zed", &[]),
        ("code", &[]),
        ("cursor", &[]),
        #[cfg(target_os = "macos")]
        ("open", &["-a", "TextEdit"]),
        #[cfg(target_os = "linux")]
        ("xdg-open", &[]),
    ];
    for (program, arguments) in candidates {
        match launch(program, arguments, &file, line) {
            Ok(()) => return Ok(()),
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
    }
    bail!("no external editor found; set GINKA_EDITOR, VISUAL, or EDITOR")
}

/// Form an argv that preserves a file path as one argument.
fn editor_args(program: &str, preset: &[&str], file: &Path, line: Option<u32>) -> Vec<OsString> {
    let mut args: Vec<OsString> = preset.iter().map(OsString::from).collect();
    let name = Path::new(program)
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or(program);
    match (name, line) {
        ("zed", Some(line)) => args.push(format!("{}:{line}", file.display()).into()),
        ("code" | "code-insiders" | "cursor", Some(line)) => {
            args.push("--goto".into());
            args.push(format!("{}:{line}", file.display()).into());
        }
        ("vim" | "nvim" | "vi" | "nano", Some(line)) => {
            args.push(format!("+{line}").into());
            args.push(file.as_os_str().into());
        }
        _ => args.push(file.as_os_str().into()),
    }
    args
}

/// Spawn and reap an editor without waiting for its window to close.
fn launch(program: &str, preset: &[&str], file: &Path, line: Option<u32>) -> Result<()> {
    let args = editor_args(program, preset, file, line);
    let mut child = Command::new(program)
        .args(args)
        .current_dir(file.parent().context("file has no parent")?)
        .spawn()
        .with_context(|| format!("starting external editor {program}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_arguments_keep_a_spaced_path_intact() {
        let file = Path::new("/work/a file.rs");
        assert_eq!(
            editor_args("/usr/bin/code", &["--reuse-window"], file, Some(42)),
            ["--reuse-window", "--goto", "/work/a file.rs:42"]
        );
        assert_eq!(
            editor_args("zed", &[], file, Some(9)),
            ["/work/a file.rs:9"]
        );
        assert_eq!(
            editor_args("nvim", &[], file, Some(9)),
            ["+9", "/work/a file.rs"]
        );
        assert_eq!(
            editor_args("custom", &["--wait"], file, Some(9)),
            ["--wait", "/work/a file.rs"]
        );
    }

    #[test]
    fn shell_metacharacters_are_passed_as_literal_arguments() {
        let file = Path::new("/work/a; echo danger.rs");
        assert_eq!(
            editor_args("code", &[], file, Some(3)),
            ["--goto", "/work/a; echo danger.rs:3"]
        );
    }

    #[test]
    fn missing_editor_reports_a_launch_error() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let error = launch("ginka-editor-that-does-not-exist", &[], &file, Some(1)).unwrap_err();
        assert!(error.to_string().contains("starting external editor"));
        assert_eq!(
            error
                .downcast_ref::<std::io::Error>()
                .map(std::io::Error::kind),
            Some(std::io::ErrorKind::NotFound)
        );
    }
}
