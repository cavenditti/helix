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

fn set_view_state(editor: &mut Editor, _sv: &SessionView) {
    // Per-file state (cursor, scroll, undo) is now restored via restore_file_state
    // which is called automatically after every file open.
    restore_file_state(editor);
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
    // Use workspace root (finds .git/.svn/.jj/.helix) instead of raw CWD
    // so sessions work regardless of which subdirectory you open hx from.
    let (workspace, _) = helix_loader::find_workspace();
    let mut hasher = DefaultHasher::new();
    workspace.hash(&mut hasher);
    session_dir().join(format!("{:x}.json", hasher.finish()))
}

fn file_state_dir() -> PathBuf {
    helix_loader::data_dir().join("file_state")
}

fn file_state_path_for(file_path: &Path) -> PathBuf {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    file_path.hash(&mut hasher);
    file_state_dir().join(format!("{:x}.json", hasher.finish()))
}

/// Per-file persisted state: cursor, scroll position, and undo history.
#[derive(Serialize, Deserialize)]
pub struct FileState {
    pub path: PathBuf,
    pub anchor: usize,
    pub head: usize,
    pub scroll_anchor: usize,
    pub scroll_v_offset: usize,
    pub scroll_h_offset: usize,
    pub history: Option<helix_core::history::SerializableHistory>,
}

pub fn save_session(session: &Session) -> anyhow::Result<()> {
    let dir = session_dir();
    std::fs::create_dir_all(&dir)?;
    let path = session_file_for_cwd();
    let json = serde_json::to_string_pretty(session)?;
    std::fs::write(path, json)?;
    Ok(())
}

/// Save per-file state (cursor, scroll, undo history) for all open documents.
pub fn save_file_states(editor: &mut Editor) {
    let dir = file_state_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        log::error!("Failed to create file state directory");
        return;
    }
    // Flush pending changes to history for all views before saving
    let view_ids: Vec<_> = editor.tree.views().map(|(v, _)| v.id).collect();
    for view_id in view_ids {
        let view = editor.tree.get_mut(view_id);
        let doc = editor.documents.get_mut(&view.doc).unwrap();
        doc.append_changes_to_history(view);
    }
    // Save state for each document
    for (view, _is_focused) in editor.tree.views() {
        let doc = &editor.documents[&view.doc];
        if let Some(file_path) = doc.path() {
            let sel = doc.selection(view.id).primary();
            let vp = doc.view_offset(view.id);
            let state = FileState {
                path: file_path.clone(),
                anchor: sel.anchor,
                head: sel.head,
                scroll_anchor: vp.anchor,
                scroll_v_offset: vp.vertical_offset,
                scroll_h_offset: vp.horizontal_offset,
                history: Some(doc.serialize_history()),
            };
            let state_path = file_state_path_for(file_path);
            match serde_json::to_string(&state) {
                Ok(json) => {
                    if let Err(e) = std::fs::write(&state_path, json) {
                        log::error!("Failed to save file state for {}: {}", file_path.display(), e);
                    }
                }
                Err(e) => {
                    log::error!("Failed to serialize file state for {}: {}", file_path.display(), e);
                }
            }
        }
    }
}

/// Restore per-file state (cursor, scroll, undo history) for a document.
/// Called whenever a file is opened, regardless of session restore.
pub fn restore_file_state(editor: &mut Editor) {
    let (view, doc) = crate::current!(editor);
    let view_id = view.id;

    let file_path = match doc.path() {
        Some(p) => p.clone(),
        None => return,
    };

    let state_path = file_state_path_for(&file_path);
    if !state_path.exists() {
        return;
    }

    let json = match std::fs::read_to_string(&state_path) {
        Ok(j) => j,
        Err(e) => {
            log::warn!("Failed to read file state for {}: {}", file_path.display(), e);
            return;
        }
    };

    let state: FileState = match serde_json::from_str(&json) {
        Ok(s) => s,
        Err(e) => {
            log::warn!("Failed to parse file state for {}: {}", file_path.display(), e);
            return;
        }
    };

    // Restore cursor (clamped to document length)
    let text_len = doc.text().len_chars();
    let anchor = state.anchor.min(text_len.saturating_sub(1));
    let head = state.head.min(text_len.saturating_sub(1));
    let selection = helix_core::Selection::single(anchor, head);
    doc.set_selection(view_id, selection);

    // Restore scroll position
    let scroll_anchor = state.scroll_anchor.min(text_len.saturating_sub(1));
    doc.set_view_offset(
        view_id,
        crate::view::ViewPosition {
            anchor: scroll_anchor,
            horizontal_offset: state.scroll_h_offset,
            vertical_offset: state.scroll_v_offset,
        },
    );

    // Restore undo history
    if let Some(history_data) = state.history {
        doc.restore_history(history_data);
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
