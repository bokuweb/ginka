//! Where the user's command-line tools are, as their own shell would find
//! them.
//!
//! A daemon started from the Dock inherits launchd's four-directory `PATH`,
//! and one started from a shell that loads a version manager lazily inherits
//! a `PATH` without it — either way an agent the user can run in a terminal
//! is "not installed" here, and one that is a Node script cannot find `node`.
//! The search path is therefore the daemon's own, then the login shell's,
//! then the places version managers and package managers put binaries; and
//! every CLI the daemon starts is given the same.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The directories nvm puts Node's binaries in, the default version first.
///
/// The default is `~/.nvm/alias/default`, which may name a full version
/// (`v20.19.0`), a prefix (`20`) or an alias nvm resolves itself (`lts/*`);
/// a prefix picks the newest installed match. The rest follow newest first,
/// so a CLI installed under any version is still found.
pub fn nvm_bins(home: &Path) -> Vec<PathBuf> {
    let versions_dir = home.join(".nvm/versions/node");
    let Ok(entries) = std::fs::read_dir(&versions_dir) else {
        return Vec::new();
    };
    let mut versions: Vec<(Vec<u64>, String)> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| Some((version_key(&name)?, name)))
        .collect();
    // Newest first.
    versions.sort_by(|a, b| b.0.cmp(&a.0));

    let default = std::fs::read_to_string(home.join(".nvm/alias/default"))
        .ok()
        .map(|text| text.trim().trim_start_matches('v').to_string())
        .filter(|text| !text.is_empty());
    if let Some(default) = default {
        let wanted: Vec<&str> = default.split('.').collect();
        let matches = |name: &str| {
            let have: Vec<&str> = name.trim_start_matches('v').split('.').collect();
            wanted.len() <= have.len() && wanted.iter().zip(&have).all(|(w, h)| w == h)
        };
        if let Some(index) = versions.iter().position(|(_, name)| matches(name)) {
            let chosen = versions.remove(index);
            versions.insert(0, chosen);
        }
    }
    versions
        .into_iter()
        .map(|(_, name)| versions_dir.join(name).join("bin"))
        .collect()
}

/// `v20.19.0` as numbers, for ordering; `None` for anything else.
fn version_key(name: &str) -> Option<Vec<u64>> {
    name.strip_prefix('v')?
        .split('.')
        .map(|part| part.parse().ok())
        .collect()
}

/// The `PATH` a login shell printed between two markers, ignoring whatever
/// else its startup files wrote.
pub fn path_from_shell_output(output: &str, marker: &str) -> Option<Vec<PathBuf>> {
    let start = output.find(marker)? + marker.len();
    let end = start + output[start..].find(marker)?;
    Some(
        std::env::split_paths(&output[start..end])
            .filter(|dir| !dir.as_os_str().is_empty())
            .collect(),
    )
}

/// Directories in order, each once, empty entries dropped.
pub fn merge(parts: &[Vec<PathBuf>]) -> Vec<PathBuf> {
    let mut merged: Vec<PathBuf> = Vec::new();
    for dir in parts.iter().flatten() {
        if !dir.as_os_str().is_empty() && !merged.contains(dir) {
            merged.push(dir.clone());
        }
    }
    merged
}

/// Where tools are commonly installed outside any shell's configuration.
pub fn well_known_dirs(home: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![
        home.join(".local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        home.join(".volta/bin"),
        home.join(".bun/bin"),
        home.join(".npm-global/bin"),
        home.join(".local/share/fnm/aliases/default/bin"),
        home.join(".local/share/mise/shims"),
        home.join(".asdf/shims"),
        home.join(".cargo/bin"),
    ];
    dirs.extend(nvm_bins(home));
    dirs
}

/// The marker the login shell's `PATH` is printed between.
const MARKER: &str = "__GINKA_TOOL_PATH__";

/// How long the login shell may take to start before it is given up on.
const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// Ask the user's login shell for its `PATH`, the way a terminal would see it.
fn login_shell_path() -> Option<Vec<PathBuf>> {
    use std::io::Read as _;
    use std::process::{Command, Stdio};

    let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let mut child = Command::new(shell)
        .args(["-ilc", &format!("printf '{MARKER}%s{MARKER}' \"$PATH\"")])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + SHELL_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let mut output = String::new();
    child.stdout.take()?.read_to_string(&mut output).ok()?;
    path_from_shell_output(&output, MARKER)
}

/// The search path: the daemon's own `PATH`, the login shell's, then the
/// usual install places — directories that exist, each once. Worked out once
/// per process: the login shell is slow to start and does not change under a
/// running daemon.
pub fn search_path() -> &'static [PathBuf] {
    static PATH: std::sync::OnceLock<Vec<PathBuf>> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let own: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|value| std::env::split_paths(&value).collect())
            .unwrap_or_default();
        let shell = login_shell_path().unwrap_or_default();
        let known = std::env::var_os("HOME")
            .map(|home| well_known_dirs(Path::new(&home)))
            .unwrap_or_default();
        let known: Vec<PathBuf> = known.into_iter().filter(|dir| dir.is_dir()).collect();
        merge(&[own, shell, known])
    })
}

/// The search path as a `PATH` value.
pub fn joined() -> OsString {
    std::env::join_paths(search_path()).unwrap_or_default()
}

/// Give a command the full search path, so a CLI that is itself a script can
/// find its interpreter.
pub fn apply(process: &mut std::process::Command) {
    process.env("PATH", joined());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nvm_home(versions: &[&str], default: Option<&str>) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        for version in versions {
            std::fs::create_dir_all(
                home.path()
                    .join(".nvm/versions/node")
                    .join(version)
                    .join("bin"),
            )
            .unwrap();
        }
        if let Some(default) = default {
            std::fs::create_dir_all(home.path().join(".nvm/alias")).unwrap();
            std::fs::write(
                home.path().join(".nvm/alias/default"),
                format!("{default}\n"),
            )
            .unwrap();
        }
        home
    }

    fn bin(home: &Path, version: &str) -> PathBuf {
        home.join(".nvm/versions/node").join(version).join("bin")
    }

    #[test]
    fn nvm_puts_its_default_first_and_the_rest_newest_first() {
        let home = nvm_home(&["v18.20.1", "v20.19.0", "v22.21.0", "v20.9.0"], Some("20"));
        assert_eq!(
            nvm_bins(home.path()),
            vec![
                bin(home.path(), "v20.19.0"),
                bin(home.path(), "v22.21.0"),
                bin(home.path(), "v20.9.0"),
                bin(home.path(), "v18.20.1"),
            ]
        );

        let exact = nvm_home(&["v20.19.0", "v22.21.0"], Some("v20.19.0"));
        assert_eq!(nvm_bins(exact.path())[0], bin(exact.path(), "v20.19.0"));

        // An alias nvm resolves itself, or none at all: newest first.
        let alias = nvm_home(&["v20.19.0", "v22.21.0"], Some("lts/*"));
        assert_eq!(nvm_bins(alias.path())[0], bin(alias.path(), "v22.21.0"));
        let none = nvm_home(&["v20.19.0", "v22.21.0"], None);
        assert_eq!(nvm_bins(none.path())[0], bin(none.path(), "v22.21.0"));

        assert!(nvm_bins(tempfile::tempdir().unwrap().path()).is_empty());
    }

    #[test]
    fn a_login_shell_path_is_read_between_its_markers() {
        let output = "Welcome!\n__M__/a/bin:/b/bin::/c__M__\ntrailing noise";
        assert_eq!(
            path_from_shell_output(output, "__M__"),
            Some(vec!["/a/bin".into(), "/b/bin".into(), "/c".into()])
        );
        assert_eq!(path_from_shell_output("no markers here", "__M__"), None);
    }

    #[test]
    fn merged_paths_keep_the_first_place_each_directory_appears() {
        let merged = merge(&[
            vec!["/usr/bin".into(), "/a".into()],
            vec!["/a".into(), PathBuf::new(), "/b".into()],
            vec!["/usr/bin".into(), "/c".into()],
        ]);
        assert_eq!(
            merged,
            vec![
                PathBuf::from("/usr/bin"),
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
    }

    #[test]
    fn the_usual_install_places_are_searched() {
        let home = Path::new("/Users/someone");
        let dirs = well_known_dirs(home);
        for expected in [
            "/Users/someone/.local/bin",
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/Users/someone/.volta/bin",
            "/Users/someone/.bun/bin",
            "/Users/someone/.cargo/bin",
        ] {
            assert!(
                dirs.contains(&PathBuf::from(expected)),
                "{expected} missing"
            );
        }
    }
}
