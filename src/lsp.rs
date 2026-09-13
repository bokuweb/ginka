//! Adapter between workspace language servers and `gpui-component` editors.

use anyhow::Result;
use ginka_core::lsp::{LanguageServer, discover};
use ginka_ui::editor::{DefinitionTarget, definition_target};
use gpui::{App, AppContext as _, Entity, Task, Window};
use gpui_component::input::{DefinitionProvider, EditorState, HoverProvider, Rope, RopeExt as _};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone)]
struct EditorLanguageServer {
    server: Arc<LanguageServer>,
    worktree: PathBuf,
}

/// Shared state between editor change events and asynchronous LSP startup.
#[derive(Clone, Default)]
pub struct EditorLspBinding {
    server: Arc<Mutex<Option<Arc<LanguageServer>>>>,
    version: Arc<AtomicI32>,
}

/// Workspace document and host callback attached to one editor.
pub struct EditorLspDocument {
    worktree: PathBuf,
    path: String,
    text: String,
    show_definition: ShowDefinitionHandler,
}

/// Host callback used when a definition belongs in another file tab.
pub type ShowDefinitionHandler = Rc<dyn Fn(DefinitionTarget, &mut App)>;

impl EditorLspDocument {
    /// Describe one editor document before its optional server is discovered.
    pub fn new(
        worktree: PathBuf,
        path: String,
        text: String,
        show_definition: ShowDefinitionHandler,
    ) -> Self {
        Self {
            worktree,
            path,
            text,
            show_definition,
        }
    }
}

impl EditorLspBinding {
    /// Advance the document version even while the server is still starting.
    pub fn change_target(&self) -> (i32, Option<Arc<LanguageServer>>) {
        let version = self.version.fetch_add(1, Ordering::AcqRel) + 1;
        let server = self
            .server
            .lock()
            .expect("language-server slot poisoned")
            .clone();
        (version, server)
    }
}

impl HoverProvider for EditorLanguageServer {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<lsp_types::Hover>>> {
        let position = text.offset_to_position(offset);
        let server = self.server.clone();
        cx.background_spawn(async move { smol::unblock(move || server.hover(position)).await })
    }
}

impl DefinitionProvider for EditorLanguageServer {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<lsp_types::LocationLink>>> {
        let position = text.offset_to_position(offset);
        let server = self.server.clone();
        let current = server.document_uri().clone();
        let worktree = self.worktree.clone();
        cx.background_spawn(async move {
            let definitions = smol::unblock(move || server.definitions(position)).await?;
            Ok(definitions
                .into_iter()
                .filter(|location| {
                    location.target_uri == current
                        || definition_target(
                            &worktree,
                            &location.target_uri.to_string(),
                            location.target_selection_range,
                        )
                        .is_some()
                })
                .collect())
        })
    }
}

/// Discover and attach an installed server without blocking the UI thread.
pub fn attach<T: 'static>(
    editor: Entity<EditorState>,
    binding: EditorLspBinding,
    document: EditorLspDocument,
    window: &mut Window,
    cx: &mut gpui::Context<T>,
) {
    let editor = editor.downgrade();
    cx.spawn_in(window, async move |_, cx| {
        let EditorLspDocument {
            mut worktree,
            path,
            text,
            show_definition,
        } = document;
        let search_path = std::env::var_os("PATH");
        let server_worktree = worktree.clone();
        let started = cx
            .background_spawn(async move {
                smol::unblock(move || {
                    let Some(launch) = discover(&server_worktree, &path, search_path.as_deref())?
                    else {
                        return Ok(None);
                    };
                    LanguageServer::start(&launch, &text).map(|server| Some((server, launch.root)))
                })
                .await
            })
            .await;
        let server = match started {
            Ok(Some((server, canonical_worktree))) => {
                worktree = canonical_worktree;
                server
            }
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "language server unavailable; keeping syntax-only editor");
                return;
            }
        };
        *binding
            .server
            .lock()
            .expect("language-server slot poisoned") = Some(server.clone());
        let provider = Rc::new(EditorLanguageServer {
            server: server.clone(),
            worktree: worktree.clone(),
        });
        let current_uri = server.document_uri().to_string();
        let current = match editor.update_in(cx, |editor, _, cx| {
            editor.lsp_mut().hover_provider = Some(provider.clone());
            editor.lsp_mut().definition_provider = Some(provider);
            editor.lsp_mut().show_document = Some(Rc::new(move |params, _, cx| {
                if params.uri.to_string() == current_uri {
                    return false;
                }
                if let Some(range) = params.selection
                    && let Some(target) =
                        definition_target(&worktree, &params.uri.to_string(), range)
                {
                    show_definition(target, cx);
                }
                // A target in another document must not fall through to the
                // toolkit's same-buffer cursor movement.
                true
            }));
            editor.refresh(cx);
            editor.value().to_string()
        }) {
            Ok(current) => current,
            Err(_) => return,
        };
        let current_version = binding.version.load(Ordering::Acquire);
        if current_version > 0 {
            let server = server.clone();
            if let Err(error) = cx
                .background_spawn(async move {
                    smol::unblock(move || server.change(current_version, current)).await
                })
                .await
            {
                tracing::debug!(%error, "initial language-server changes were not delivered");
            }
        }

        let server = Arc::downgrade(&server);
        loop {
            cx.background_executor()
                .timer(Duration::from_millis(250))
                .await;
            let Some(server) = server.upgrade() else {
                break;
            };
            let Some(diagnostics) = server.take_diagnostics() else {
                continue;
            };
            if editor
                .update_in(cx, |editor, _, cx| {
                    if let Some(set) = editor.diagnostics_mut() {
                        set.clear();
                        set.extend(diagnostics);
                    }
                    cx.notify();
                })
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
}
