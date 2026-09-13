//! Language-server selection and safe workspace document resolution.

use anyhow::{Context as _, Result};
use lsp_types::{
    Diagnostic, DidChangeTextDocumentParams, DidOpenTextDocumentParams, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverParams, Position, PublishDiagnosticsParams,
    TextDocumentContentChangeEvent, TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, VersionedTextDocumentIdentifier,
};
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

/// Largest JSON-RPC body accepted from a language server.
const MAX_LSP_MESSAGE_BYTES: usize = 8 * 1024 * 1024;
const MAX_LSP_HEADER_BYTES: usize = 8 * 1024;
const LSP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
type ResponseSender = mpsc::Sender<std::result::Result<Value, String>>;
type PendingResponses = Arc<Mutex<HashMap<u64, ResponseSender>>>;

/// Static language-server convention for one source-file kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LanguageServerSpec {
    /// LSP language identifier sent with `textDocument/didOpen`.
    pub language_id: &'static str,
    /// Executable searched for on `PATH`.
    pub program: &'static str,
    /// Arguments needed to make the executable speak LSP over stdio.
    pub arguments: &'static [&'static str],
}

/// A resolved language-server process and the document it may inspect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LanguageServerLaunch {
    /// Absolute executable found on `PATH`.
    pub program: PathBuf,
    /// Arguments passed after the executable.
    pub arguments: Vec<String>,
    /// Canonical workspace root advertised to the server.
    pub root: PathBuf,
    /// Canonical document path, guaranteed to remain within [`Self::root`].
    pub document: PathBuf,
    /// LSP language identifier for the document.
    pub language_id: String,
}

/// One stdio language-server connection for an open editor document.
///
/// The connection is reconstructible view support: dropping the last handle
/// ends the child, while the source of truth remains the workspace file.
pub struct LanguageServer {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingResponses,
    diagnostics: Arc<Mutex<Option<Vec<Diagnostic>>>>,
    next_id: AtomicU64,
    sent_version: AtomicI32,
    uri: lsp_types::Uri,
}

impl LanguageServer {
    /// Start, initialize and open the launch's document with its current text.
    pub fn start(launch: &LanguageServerLaunch, text: &str) -> Result<Arc<Self>> {
        let mut command = Command::new(&launch.program);
        command
            .args(&launch.arguments)
            .current_dir(&launch.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {}", launch.program.display()))?;
        let stdin = Arc::new(Mutex::new(
            child.stdin.take().context("language server has no stdin")?,
        ));
        let stdout = child
            .stdout
            .take()
            .context("language server has no stdout")?;
        let pending = Arc::new(Mutex::new(HashMap::new()));
        let diagnostics = Arc::new(Mutex::new(None));
        let uri = file_uri(&launch.document)?;
        spawn_reader(
            stdout,
            stdin.clone(),
            pending.clone(),
            diagnostics.clone(),
            uri.clone(),
        )?;
        let server = Arc::new(Self {
            child: Mutex::new(child),
            stdin,
            pending,
            diagnostics,
            next_id: AtomicU64::new(1),
            sent_version: AtomicI32::new(0),
            uri,
        });

        let root_uri = file_uri(&launch.root)?;
        server.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "general": { "positionEncodings": ["utf-16"] },
                    "textDocument": { "hover": {}, "definition": {}, "publishDiagnostics": {} }
                },
                "clientInfo": { "name": "ginka" }
            }),
        )?;
        server.notify("initialized", json!({}))?;
        server.notify(
            "textDocument/didOpen",
            serde_json::to_value(DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: server.uri.clone(),
                    language_id: launch.language_id.clone(),
                    version: 0,
                    text: text.to_string(),
                },
            })?,
        )?;
        Ok(server)
    }

    /// Replace the open document text with a newer editor version.
    pub fn change(&self, version: i32, text: String) -> Result<()> {
        if version <= self.sent_version.fetch_max(version, Ordering::AcqRel) {
            return Ok(());
        }
        self.notify(
            "textDocument/didChange",
            serde_json::to_value(DidChangeTextDocumentParams {
                text_document: VersionedTextDocumentIdentifier {
                    uri: self.uri.clone(),
                    version,
                },
                content_changes: vec![TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text,
                }],
            })?,
        )
    }

    /// Ask for hover information at one UTF-16 LSP position.
    pub fn hover(&self, position: Position) -> Result<Option<Hover>> {
        let value = self.request(
            "textDocument/hover",
            serde_json::to_value(HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: self.uri.clone(),
                    },
                    position,
                },
                work_done_progress_params: Default::default(),
            })?,
        )?;
        Ok(serde_json::from_value(value)?)
    }

    /// Ask for definition targets at one UTF-16 LSP position.
    pub fn definitions(&self, position: Position) -> Result<Vec<lsp_types::LocationLink>> {
        let value = self.request(
            "textDocument/definition",
            serde_json::to_value(GotoDefinitionParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier {
                        uri: self.uri.clone(),
                    },
                    position,
                },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
            })?,
        )?;
        let response: Option<GotoDefinitionResponse> = serde_json::from_value(value)?;
        Ok(match response {
            None => Vec::new(),
            Some(GotoDefinitionResponse::Link(links)) => links,
            Some(GotoDefinitionResponse::Scalar(location)) => vec![location_link(location)],
            Some(GotoDefinitionResponse::Array(locations)) => {
                locations.into_iter().map(location_link).collect()
            }
        })
    }

    /// Take the newest diagnostics notification, if one arrived.
    pub fn take_diagnostics(&self) -> Option<Vec<Diagnostic>> {
        self.diagnostics.lock().take()
    }

    /// URI of the document opened on this connection.
    pub fn document_uri(&self) -> &lsp_types::Uri {
        &self.uri
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = mpsc::channel();
        self.pending.lock().insert(id, sender);
        if let Err(error) = self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })) {
            self.pending.lock().remove(&id);
            return Err(error);
        }
        let response = match receiver.recv_timeout(LSP_REQUEST_TIMEOUT) {
            Ok(response) => response.map_err(anyhow::Error::msg)?,
            Err(error) => {
                self.pending.lock().remove(&id);
                return Err(error)
                    .with_context(|| format!("language server timed out answering {method}"));
            }
        };
        Ok(response)
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    fn send(&self, message: &Value) -> Result<()> {
        write_message(&mut *self.stdin.lock(), message).context("writing to language server")
    }
}

impl Drop for LanguageServer {
    fn drop(&mut self) {
        let _ = self.notify("exit", Value::Null);
        let _ = self.child.lock().kill();
    }
}

/// Return the conventional language server for a source path.
///
/// This is intentionally a small multi-language baseline, not an installer or
/// package manager. An absent executable leaves the editor syntax-only.
pub fn specification(path: impl AsRef<Path>) -> Option<LanguageServerSpec> {
    let extension = path.as_ref().extension()?.to_str()?;
    let spec = if extension.eq_ignore_ascii_case("rs") {
        LanguageServerSpec {
            language_id: "rust",
            program: "rust-analyzer",
            arguments: &[],
        }
    } else if extension.eq_ignore_ascii_case("ts") {
        LanguageServerSpec {
            language_id: "typescript",
            program: "typescript-language-server",
            arguments: &["--stdio"],
        }
    } else if extension.eq_ignore_ascii_case("tsx") {
        LanguageServerSpec {
            language_id: "typescriptreact",
            program: "typescript-language-server",
            arguments: &["--stdio"],
        }
    } else if ["js", "mjs", "cjs"]
        .iter()
        .any(|candidate| extension.eq_ignore_ascii_case(candidate))
    {
        LanguageServerSpec {
            language_id: "javascript",
            program: "typescript-language-server",
            arguments: &["--stdio"],
        }
    } else if extension.eq_ignore_ascii_case("jsx") {
        LanguageServerSpec {
            language_id: "javascriptreact",
            program: "typescript-language-server",
            arguments: &["--stdio"],
        }
    } else if extension.eq_ignore_ascii_case("py") {
        LanguageServerSpec {
            language_id: "python",
            program: "pyright-langserver",
            arguments: &["--stdio"],
        }
    } else if extension.eq_ignore_ascii_case("go") {
        LanguageServerSpec {
            language_id: "go",
            program: "gopls",
            arguments: &[],
        }
    } else {
        return None;
    };
    Some(spec)
}

/// Resolve an installed server and a safe workspace document.
///
/// `search_path` is explicit so startup uses the environment it was given and
/// tests never mutate the process-global `PATH`. `None` means there is no
/// search path, not that this function should consult the ambient one.
pub fn discover(
    worktree: &Path,
    relative_path: &str,
    search_path: Option<&OsStr>,
) -> Result<Option<LanguageServerLaunch>> {
    let Some(spec) = specification(relative_path) else {
        return Ok(None);
    };
    let Some(program) = search_path.and_then(|path| find_executable(spec.program, path)) else {
        return Ok(None);
    };
    let root = worktree
        .canonicalize()
        .context("the workspace is not where it was")?;
    let document = worktree
        .join(relative_path)
        .canonicalize()
        .with_context(|| format!("no file at {relative_path}"))?;
    anyhow::ensure!(
        document.starts_with(&root),
        "{relative_path} is outside the workspace"
    );
    anyhow::ensure!(document.is_file(), "{relative_path} is not a file");
    Ok(Some(LanguageServerLaunch {
        program,
        arguments: spec
            .arguments
            .iter()
            .map(|value| value.to_string())
            .collect(),
        root,
        document,
        language_id: spec.language_id.to_string(),
    }))
}

fn find_executable(program: &str, search_path: &OsStr) -> Option<PathBuf> {
    for directory in std::env::split_paths(search_path) {
        for candidate in executable_candidates(&directory, program) {
            if executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn executable_candidates(directory: &Path, program: &str) -> Vec<PathBuf> {
    vec![directory.join(program)]
}

#[cfg(windows)]
fn executable_candidates(directory: &Path, program: &str) -> Vec<PathBuf> {
    ["", ".exe", ".cmd", ".bat"]
        .iter()
        .map(|suffix| directory.join(format!("{program}{suffix}")))
        .collect()
}

#[cfg(not(any(unix, windows)))]
fn executable_candidates(directory: &Path, program: &str) -> Vec<PathBuf> {
    vec![directory.join(program)]
}

fn executable(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn file_uri(path: &Path) -> Result<lsp_types::Uri> {
    let url = url::Url::from_file_path(path)
        .map_err(|_| anyhow::anyhow!("{} cannot be represented as a file URI", path.display()))?;
    lsp_types::Uri::from_str(url.as_str()).context("building language-server file URI")
}

fn location_link(location: lsp_types::Location) -> lsp_types::LocationLink {
    lsp_types::LocationLink {
        origin_selection_range: None,
        target_uri: location.uri,
        target_range: location.range,
        target_selection_range: location.range,
    }
}

fn spawn_reader(
    stdout: std::process::ChildStdout,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingResponses,
    diagnostics: Arc<Mutex<Option<Vec<Diagnostic>>>>,
    document_uri: lsp_types::Uri,
) -> Result<()> {
    std::thread::Builder::new()
        .name("ginka-lsp-reader".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let message = match read_message(&mut reader) {
                    Ok(message) => message,
                    Err(error) => {
                        let message = error.to_string();
                        for (_, waiting) in std::mem::take(&mut *pending.lock()) {
                            let _ = waiting.send(Err(message.clone()));
                        }
                        break;
                    }
                };

                if let Some(id) = message.get("id").and_then(Value::as_u64)
                    && message.get("method").is_none()
                {
                    if let Some(waiting) = pending.lock().remove(&id) {
                        let result = if let Some(error) = message.get("error") {
                            Err(error.to_string())
                        } else {
                            Ok(message.get("result").cloned().unwrap_or(Value::Null))
                        };
                        let _ = waiting.send(result);
                    }
                    continue;
                }

                if message.get("method").and_then(Value::as_str)
                    == Some("textDocument/publishDiagnostics")
                    && let Some(params) = message.get("params")
                    && let Ok(published) =
                        serde_json::from_value::<PublishDiagnosticsParams>(params.clone())
                    && published.uri == document_uri
                {
                    *diagnostics.lock() = Some(published.diagnostics);
                    continue;
                }

                // Language servers may ask for dynamic registration or
                // configuration. A null answer means unsupported without
                // leaving the server blocked forever.
                if let Some(id) = message.get("id").cloned()
                    && message.get("method").is_some()
                {
                    let _ = write_message(
                        &mut *stdin.lock(),
                        &json!({"jsonrpc":"2.0","id":id,"result":Value::Null}),
                    );
                }
            }
        })
        .context("starting language-server reader")?;
    Ok(())
}

fn write_message(writer: &mut impl std::io::Write, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    anyhow::ensure!(
        body.len() <= MAX_LSP_MESSAGE_BYTES,
        "language-server message is too large"
    );
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

fn read_message(reader: &mut impl BufRead) -> Result<Value> {
    let mut content_length = None;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line)?;
        anyhow::ensure!(read > 0, "language server closed its output");
        header_bytes += read;
        anyhow::ensure!(
            header_bytes <= MAX_LSP_HEADER_BYTES,
            "language-server header is too large"
        );
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("Content-Length")
        {
            content_length = Some(value.trim().parse::<usize>()?);
        }
    }
    let length = content_length.context("language-server message has no Content-Length")?;
    anyhow::ensure!(
        length <= MAX_LSP_MESSAGE_BYTES,
        "language-server message is too large"
    );
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::io::Cursor;

    fn executable(directory: &std::path::Path, name: &str) {
        let path = directory.join(name);
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mut permissions = std::fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(path, permissions).unwrap();
        }
    }

    fn fixture(path: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "source\n").unwrap();
        (directory, file)
    }

    #[test]
    fn common_source_files_choose_their_standard_language_server() {
        let cases = [
            ("src/main.rs", "rust", "rust-analyzer", Vec::<&str>::new()),
            (
                "web/app.ts",
                "typescript",
                "typescript-language-server",
                vec!["--stdio"],
            ),
            (
                "web/app.tsx",
                "typescriptreact",
                "typescript-language-server",
                vec!["--stdio"],
            ),
            (
                "web/app.jsx",
                "javascriptreact",
                "typescript-language-server",
                vec!["--stdio"],
            ),
            (
                "scripts/check.py",
                "python",
                "pyright-langserver",
                vec!["--stdio"],
            ),
            ("cmd/main.go", "go", "gopls", Vec::<&str>::new()),
        ];

        for (path, language_id, program, arguments) in cases {
            let spec = specification(path).expect(path);
            assert_eq!(spec.language_id, language_id, "{path}");
            assert_eq!(spec.program, program, "{path}");
            assert_eq!(spec.arguments, arguments, "{path}");
        }
    }

    #[test]
    fn unknown_and_document_only_formats_stay_syntax_only() {
        for path in ["README.md", "assets/logo.png", "LICENSE", "data.toml"] {
            assert_eq!(specification(path), None, "{path}");
        }
    }

    #[test]
    fn discovery_uses_the_first_executable_on_path() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        executable(first.path(), "rust-analyzer");
        executable(second.path(), "rust-analyzer");
        let search_path = std::env::join_paths([first.path(), second.path()]).unwrap();
        let (worktree, _) = fixture("src/main.rs");

        let launch = discover(worktree.path(), "src/main.rs", Some(&search_path))
            .unwrap()
            .expect("installed server");
        assert_eq!(launch.program, first.path().join("rust-analyzer"));
        assert_eq!(launch.root, worktree.path().canonicalize().unwrap());
        assert_eq!(
            launch.document,
            worktree.path().join("src/main.rs").canonicalize().unwrap()
        );
        assert_eq!(launch.language_id, "rust");
    }

    #[cfg(unix)]
    #[test]
    fn discovery_skips_directories_and_files_without_execute_permission() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(directory.path().join("rust-analyzer")).unwrap();
        let plain = tempfile::tempdir().unwrap();
        std::fs::write(plain.path().join("rust-analyzer"), "not executable\n").unwrap();
        let installed = tempfile::tempdir().unwrap();
        executable(installed.path(), "rust-analyzer");
        let search_path =
            std::env::join_paths([directory.path(), plain.path(), installed.path()]).unwrap();
        let (worktree, _) = fixture("src/main.rs");

        let launch = discover(worktree.path(), "src/main.rs", Some(&search_path))
            .unwrap()
            .expect("the third entry is executable");
        assert_eq!(launch.program, installed.path().join("rust-analyzer"));
    }

    #[test]
    fn a_missing_server_is_a_syntax_only_editor_not_an_error() {
        let empty = tempfile::tempdir().unwrap();
        let search_path = OsString::from(empty.path());
        let (worktree, _) = fixture("src/main.rs");

        assert_eq!(
            discover(worktree.path(), "src/main.rs", Some(&search_path)).unwrap(),
            None
        );
        assert_eq!(
            discover(worktree.path(), "src/main.rs", None).unwrap(),
            None
        );
    }

    #[test]
    fn a_document_cannot_escape_the_workspace_through_a_symlink() {
        let directory = tempfile::tempdir().unwrap();
        let worktree = directory.path().join("worktree");
        std::fs::create_dir(&worktree).unwrap();
        let outside = directory.path().join("outside.rs");
        std::fs::write(&outside, "secret\n").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, worktree.join("link.rs")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, worktree.join("link.rs")).unwrap();
        let bin = tempfile::tempdir().unwrap();
        executable(bin.path(), "rust-analyzer");
        let search_path = std::env::join_paths([bin.path()]).unwrap();

        let error = discover(&worktree, "link.rs", Some(&search_path)).unwrap_err();
        assert!(
            error.to_string().contains("outside the workspace"),
            "{error}"
        );
    }

    #[test]
    fn json_rpc_messages_are_content_length_framed() {
        let message = serde_json::json!({"jsonrpc":"2.0","id":7,"method":"example"});
        let mut written = Vec::new();
        write_message(&mut written, &message).unwrap();
        let split = written
            .windows(4)
            .position(|part| part == b"\r\n\r\n")
            .unwrap();
        let body = &written[split + 4..];
        assert_eq!(
            &written[..split],
            format!("Content-Length: {}", body.len()).as_bytes()
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(body).unwrap(),
            message
        );
    }

    #[test]
    fn a_frame_reader_accepts_extra_headers_and_keeps_utf8_lengths_in_bytes() {
        let body = serde_json::to_vec(&serde_json::json!({"method":"例"})).unwrap();
        let framed = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\nContent-Length: {}\r\n\r\n",
            body.len()
        )
        .into_bytes()
        .into_iter()
        .chain(body)
        .collect::<Vec<_>>();

        assert_eq!(
            read_message(&mut Cursor::new(framed)).unwrap(),
            serde_json::json!({"method":"例"})
        );
    }

    #[test]
    fn a_language_server_cannot_make_the_client_allocate_an_unbounded_frame() {
        let framed = format!("Content-Length: {}\r\n\r\n", MAX_LSP_MESSAGE_BYTES + 1);
        let error = read_message(&mut Cursor::new(framed)).unwrap_err();
        assert!(error.to_string().contains("too large"), "{error}");
    }
}
