//! Adapter between workspace language servers and `gpui-component` editors.

use anyhow::Result;
use ginka_core::lsp::{LanguageServer, discover};
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
}

/// Shared state between editor change events and asynchronous LSP startup.
#[derive(Clone, Default)]
pub struct EditorLspBinding {
    server: Arc<Mutex<Option<Arc<LanguageServer>>>>,
    version: Arc<AtomicI32>,
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
        cx.background_spawn(async move {
            let definitions = smol::unblock(move || server.definitions(position)).await?;
            // The editor can jump within its own buffer. Cross-file locations
            // need the file-tab navigation adapter, which is a separate slice.
            Ok(definitions
                .into_iter()
                .filter(|location| location.target_uri == current)
                .collect())
        })
    }
}

/// Discover and attach an installed server without blocking the UI thread.
pub fn attach<T: 'static>(
    editor: Entity<EditorState>,
    binding: EditorLspBinding,
    worktree: PathBuf,
    path: String,
    text: String,
    window: &mut Window,
    cx: &mut gpui::Context<T>,
) {
    let editor = editor.downgrade();
    cx.spawn_in(window, async move |_, cx| {
        let search_path = std::env::var_os("PATH");
        let started = cx
            .background_spawn(async move {
                smol::unblock(move || {
                    let Some(launch) = discover(&worktree, &path, search_path.as_deref())? else {
                        return Ok(None);
                    };
                    LanguageServer::start(&launch, &text).map(Some)
                })
                .await
            })
            .await;
        let server = match started {
            Ok(Some(server)) => server,
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
        });
        let current = match editor.update_in(cx, |editor, _, cx| {
            editor.lsp_mut().hover_provider = Some(provider.clone());
            editor.lsp_mut().definition_provider = Some(provider);
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
