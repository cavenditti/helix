use helix_core::history::RevisionInfo;
use helix_view::graphics::Rect;
use std::time::Instant;
use tui::buffer::Buffer as Surface;
use tui::widgets::{Block, Borders, Widget};

use crate::compositor::{self, Component, Context, Event, EventResult};

/// A line in the rendered undo tree, mapping to a revision index.
struct TreeLine {
    text: String,
    revision_index: usize,
}

pub struct UndoTreeView {
    revisions: Vec<RevisionInfo>,
    lines: Vec<TreeLine>,
    selected: usize,
    scroll: usize,
}

impl UndoTreeView {
    pub fn new(revisions: Vec<RevisionInfo>, current_rev: usize) -> Self {
        let lines = build_tree_lines(&revisions);
        // Find the line corresponding to the current revision
        let selected = lines
            .iter()
            .position(|l| l.revision_index == current_rev)
            .unwrap_or(0);
        Self {
            revisions,
            lines,
            selected,
            scroll: 0,
        }
    }
}

/// Build the tree lines by doing a DFS traversal from the root.
/// The "main" path follows the last child (most recent) at each node,
/// and older branches fork to the right.
fn build_tree_lines(revisions: &[RevisionInfo]) -> Vec<TreeLine> {
    if revisions.is_empty() {
        return Vec::new();
    }

    let mut lines = Vec::new();
    let now = Instant::now();

    // DFS traversal - collect nodes in reverse chronological order
    // (newest at top, root at bottom)
    let mut stack: Vec<(usize, usize)> = Vec::new(); // (revision_index, depth)
    stack.push((0, 0));

    // We'll collect all nodes then reverse for display
    let mut entries: Vec<(usize, usize)> = Vec::new(); // (revision_index, depth)
    dfs_collect(revisions, 0, 0, &mut entries);

    // Reverse so newest is at top
    entries.reverse();

    for (rev_idx, depth) in entries {
        let rev = &revisions[rev_idx];
        let elapsed = now.duration_since(rev.timestamp);
        let time_str = format_duration(elapsed);

        let indent = "  ".repeat(depth);
        let marker = if rev.is_current { "*" } else { "o" };
        let branch_info = if rev.children.len() > 1 {
            format!(" [{} branches]", rev.children.len())
        } else {
            String::new()
        };

        let text = format!(
            "{}{} [{}] {}{}",
            indent, marker, rev_idx, time_str, branch_info
        );

        lines.push(TreeLine {
            text,
            revision_index: rev_idx,
        });
    }

    lines
}

/// Recursive DFS: follow the main path (last child) first at each level,
/// then older children at increasing depth.
fn dfs_collect(
    revisions: &[RevisionInfo],
    node: usize,
    depth: usize,
    out: &mut Vec<(usize, usize)>,
) {
    out.push((node, depth));

    let rev = &revisions[node];
    if rev.children.is_empty() {
        return;
    }

    // Main path: last child (most recent) stays at same depth
    let main_child = *rev.children.last().unwrap();
    // Older branches get increasing depth
    for &child in rev.children.iter().rev().skip(1) {
        dfs_collect(revisions, child, depth + 1, out);
    }
    dfs_collect(revisions, main_child, depth, out);
}

fn format_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

impl Component for UndoTreeView {
    fn render(&mut self, area: Rect, surface: &mut Surface, cx: &mut Context) {
        let theme = &cx.editor.theme;
        let popup_style = theme.get("ui.popup");
        let text_style = theme.get("ui.text");
        let selected_style = theme.get("ui.selection");
        let cursor_style = theme.get("ui.cursor.primary");

        // Draw background
        surface.set_style(area, popup_style);

        // Draw border
        let block = Block::default()
            .title("Undo Tree")
            .borders(Borders::ALL)
            .border_style(popup_style);
        let inner = block.inner(area);
        block.render(area, surface);

        if self.lines.is_empty() || inner.height == 0 {
            return;
        }

        let height = inner.height as usize;

        // Adjust scroll to keep selection visible
        if self.selected < self.scroll {
            self.scroll = self.selected;
        }
        if self.selected >= self.scroll + height {
            self.scroll = self.selected - height + 1;
        }

        // Render visible lines
        let visible_end = (self.scroll + height).min(self.lines.len());
        for (i, line) in self.lines[self.scroll..visible_end].iter().enumerate() {
            let y = inner.y + i as u16;
            let is_current = self.revisions.get(line.revision_index)
                .map_or(false, |r| r.is_current);
            let is_selected = self.scroll + i == self.selected;

            let style = if is_selected && is_current {
                cursor_style
            } else if is_selected {
                selected_style
            } else if is_current {
                cursor_style
            } else {
                text_style
            };

            let display_width = inner.width as usize;
            let truncated: String = line.text.chars().take(display_width).collect();
            surface.set_stringn(
                inner.x,
                y,
                &truncated,
                display_width,
                style,
            );
        }
    }

    fn handle_event(&mut self, event: &Event, _ctx: &mut Context) -> EventResult {
        use helix_view::input::{KeyCode, KeyEvent};

        let key_event = match event {
            Event::Key(ev) => ev,
            _ => return EventResult::Ignored(None),
        };

        match key_event {
            KeyEvent {
                code: KeyCode::Char('j') | KeyCode::Down,
                ..
            } => {
                if self.selected + 1 < self.lines.len() {
                    self.selected += 1;
                }
                EventResult::Consumed(None)
            }
            KeyEvent {
                code: KeyCode::Char('k') | KeyCode::Up,
                ..
            } => {
                self.selected = self.selected.saturating_sub(1);
                EventResult::Consumed(None)
            }
            KeyEvent {
                code: KeyCode::Char('g'),
                ..
            } => {
                self.selected = 0;
                EventResult::Consumed(None)
            }
            KeyEvent {
                code: KeyCode::Char('G'),
                ..
            } => {
                if !self.lines.is_empty() {
                    self.selected = self.lines.len() - 1;
                }
                EventResult::Consumed(None)
            }
            KeyEvent {
                code: KeyCode::Enter,
                ..
            } => {
                let revision = self.lines[self.selected].revision_index;
                let callback: compositor::Callback =
                    Box::new(move |compositor: &mut compositor::Compositor, cx| {
                        // Jump to the selected revision
                        let (view, doc) = helix_view::current!(cx.editor);
                        doc.jump_to_revision(view, revision);
                        // Remove the undotree overlay
                        compositor.remove("undotree");
                    });
                EventResult::Consumed(Some(callback))
            }
            KeyEvent {
                code: KeyCode::Esc | KeyCode::Char('q'),
                ..
            } => {
                let callback: compositor::Callback =
                    Box::new(|compositor: &mut compositor::Compositor, _cx| {
                        compositor.remove("undotree");
                    });
                EventResult::Consumed(Some(callback))
            }
            _ => EventResult::Ignored(None),
        }
    }

    fn required_size(&mut self, viewport: (u16, u16)) -> Option<(u16, u16)> {
        Some(viewport)
    }

    fn id(&self) -> Option<&'static str> {
        Some("undotree")
    }
}
