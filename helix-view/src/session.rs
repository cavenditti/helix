use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::editor::Action;
use crate::tree::{Content, Layout};
use crate::Editor;

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub version: u32,
    pub working_directory: PathBuf,
    pub layout: SessionNode,
    pub focus_index: usize,
}

#[derive(Serialize, Deserialize, Clone)]
pub enum SessionNode {
    View(SessionView),
    Container {
        layout: SessionLayout,
        children: Vec<SessionNode>,
    },
}

#[derive(Serialize, Deserialize, Clone, Copy)]
pub enum SessionLayout {
    Horizontal,
    Vertical,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct SessionView {
    pub path: Option<PathBuf>,
    pub anchor: usize,
    pub head: usize,
    pub scroll_anchor: usize,
    pub scroll_v_offset: usize,
    pub scroll_h_offset: usize,
}

impl Session {
    pub fn from_editor(editor: &Editor) -> Self {
        let working_directory = helix_stdx::env::current_working_dir();

        let mut focus_index = 0;
        let mut view_counter = 0;

        let layout = serialize_node(editor, editor.tree.root(), &mut focus_index, &mut view_counter);

        Session {
            version: 1,
            working_directory,
            layout,
            focus_index,
        }
    }

    pub fn restore(self, editor: &mut Editor) -> anyhow::Result<()> {
        // Flatten the layout tree into a list of views with their nesting info
        let mut views = Vec::new();
        flatten_views(&self.layout, &mut views);

        if views.is_empty() {
            anyhow::bail!("Session has no views to restore");
        }

        // Open the first file
        let first = &views[0];
        if let Some(path) = &first.path {
            if path.exists() {
                editor.open(path, Action::VerticalSplit)?;
            } else {
                log::warn!("Session file not found, skipping: {}", path.display());
                editor.new_file(Action::VerticalSplit);
            }
        } else {
            editor.new_file(Action::VerticalSplit);
        }
        set_view_state(editor, &first);

        // Open subsequent files by splitting
        for (i, sv) in views.iter().enumerate().skip(1) {
            let action = if i % 2 == 0 {
                Action::HorizontalSplit
            } else {
                Action::VerticalSplit
            };
            if let Some(path) = &sv.path {
                if path.exists() {
                    editor.open(path, action)?;
                } else {
                    log::warn!("Session file not found, skipping: {}", path.display());
                    editor.new_file(action);
                }
            } else {
                editor.new_file(action);
            }
            set_view_state(editor, sv);
        }

        // Try to focus the correct view
        let view_ids: Vec<_> = editor.tree.views().map(|(v, _)| v.id).collect();
        if let Some(&id) = view_ids.get(self.focus_index) {
            editor.focus(id);
        }

        Ok(())
    }
}

fn set_view_state(editor: &mut Editor, sv: &SessionView) {
    let (view, doc) = current!(editor);
    let view_id = view.id;
    let text = doc.text().clone();

    // Restore cursor position (clamped to document length)
    let text_len = text.len_chars();
    let anchor = sv.anchor.min(text_len.saturating_sub(1));
    let head = sv.head.min(text_len.saturating_sub(1));
    let selection = helix_core::Selection::single(anchor, head);
    doc.set_selection(view_id, selection);

    // Restore scroll position
    let scroll_anchor = sv.scroll_anchor.min(text_len.saturating_sub(1));
    doc.set_view_offset(
        view_id,
        crate::view::ViewPosition {
            anchor: scroll_anchor,
            horizontal_offset: sv.scroll_h_offset,
            vertical_offset: sv.scroll_v_offset,
        },
    );

    // Restore undo history
    restore_undo_history(doc);
}

fn serialize_node(
    editor: &Editor,
    node_id: crate::ViewId,
    focus_index: &mut usize,
    view_counter: &mut usize,
) -> SessionNode {
    let node = editor.tree.node(node_id);
    match &node.content {
        Content::View(view) => {
            if editor.tree.focus == node_id {
                *focus_index = *view_counter;
            }
            *view_counter += 1;

            let doc = editor.documents.get(&view.doc);
            let path = doc.and_then(|d| d.path().map(|p| p.to_path_buf()));

            let (anchor, head) = doc
                .map(|d| {
                    let sel = d.selection(view.id);
                    let primary = sel.primary();
                    (primary.anchor, primary.head)
                })
                .unwrap_or((0, 0));

            let (scroll_anchor, scroll_v_offset, scroll_h_offset) = doc
                .map(|d| {
                    let vp = d.view_offset(view.id);
                    (vp.anchor, vp.vertical_offset, vp.horizontal_offset)
                })
                .unwrap_or((0, 0, 0));

            SessionNode::View(SessionView {
                path,
                anchor,
                head,
                scroll_anchor,
                scroll_v_offset,
                scroll_h_offset,
            })
        }
        Content::Container(container) => {
            let layout = match container.layout {
                Layout::Horizontal => SessionLayout::Horizontal,
                Layout::Vertical => SessionLayout::Vertical,
            };
            let children = container
                .children
                .iter()
                .map(|&child_id| serialize_node(editor, child_id, focus_index, view_counter))
                .collect();
            SessionNode::Container { layout, children }
        }
    }
}

fn flatten_views(node: &SessionNode, out: &mut Vec<SessionView>) {
    match node {
        SessionNode::View(sv) => out.push(sv.clone()),
        SessionNode::Container { children, .. } => {
            for child in children {
                flatten_views(child, out);
            }
        }
    }
}

// --- File I/O ---

pub fn session_dir() -> PathBuf {
    helix_loader::data_dir().join("sessions")
}

pub fn session_file_for_cwd() -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let cwd = helix_stdx::env::current_working_dir();
    let mut hasher = DefaultHasher::new();
    cwd.hash(&mut hasher);
    session_dir().join(format!("{:x}.json", hasher.finish()))
}

fn undo_dir() -> PathBuf {
    session_dir().join("undo")
}

fn undo_file_for_path(file_path: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    file_path.hash(&mut hasher);
    undo_dir().join(format!("{:x}.json", hasher.finish()))
}

pub fn save_session(session: &Session) -> anyhow::Result<()> {
    let dir = session_dir();
    std::fs::create_dir_all(&dir)?;
    let path = session_file_for_cwd();
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Save undo history for all open documents that have file paths.
pub fn save_undo_histories(editor: &Editor) {
    let dir = undo_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        log::error!("Failed to create undo directory");
        return;
    }
    for doc in editor.documents() {
        if let Some(file_path) = doc.path() {
            let history_data = doc.serialize_history();
            let undo_path = undo_file_for_path(file_path);
            match serde_json::to_string(&history_data) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&undo_path, json) {
                        log::error!("Failed to save undo history for {}: {}", file_path.display(), e);
                    }
                }
                Err(e) => {
                    log::error!("Failed to serialize undo history for {}: {}", file_path.display(), e);
                }
            }
        }
    }
}

/// Try to restore undo history for a document from disk.
pub fn restore_undo_history(doc: &crate::Document) {
    if let Some(file_path) = doc.path() {
        let undo_path = undo_file_for_path(file_path);
        if undo_path.exists() {
            match std::fs::read_to_string(&undo_path) {
                Ok(json) => {
                    match serde_json::from_str::<helix_core::history::SerializableHistory>(&json) {
                        Ok(data) => doc.restore_history(data),
                        Err(e) => log::warn!("Failed to parse undo history for {}: {}", file_path.display(), e),
                    }
                }
                Err(e) => log::warn!("Failed to read undo history for {}: {}", file_path.display(), e),
            }
        }
    }
}

pub fn load_session() -> anyhow::Result<Option<Session>> {
    let path = session_file_for_cwd();
    if !path.exists() {
        return Ok(None);
    }
    let json = std::fs::read_to_string(&path)?;
    let session: Session = serde_json::from_str(&json)?;
    Ok(Some(session))
}

pub fn delete_session() -> anyhow::Result<()> {
    let path = session_file_for_cwd();
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
