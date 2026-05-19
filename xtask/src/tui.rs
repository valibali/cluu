//! ratatui-based build progress TUI.
//!
//! Replaces the hand-rolled ANSI escape-code renderer
//! (formerly RichTreeUi/render_tree_frame) with a proper
//! crossterm + ratatui terminal UI.
//!
//! The TUI renders on stderr so that stdout remains clean
//! for subprocess output and terminal piping.

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result};
use crossterm::{
    event::{self, Event as CrosstermEvent, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Gauge, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use ratatui::backend::CrosstermBackend;

// ---------------------------------------------------------------------------
// Types shared with main.rs (mirror the old types for clean boundary)
// ---------------------------------------------------------------------------

/// Mirrors main.rs NodeStatus
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeStatus {
    Pending,
    Running,
    Done,
    Failed,
}

/// Mirrors main.rs RichTreeNode
#[derive(Clone, Debug)]
pub struct TreeNode {
    pub id: String,
    pub label: String,
    pub parent: Option<String>,
    pub children: Vec<String>,
    pub is_leaf: bool,
    pub status: NodeStatus,
    pub last_line: String,
    pub fail_log: Option<String>,
    pub progress: f32,
    pub work_units_seen: u32,
    pub work_units_expected: u32,
    pub start_tick: u64,
}

/// Mirrors main.rs RichTreeNodeDef
#[derive(Clone, Debug)]
pub struct TreeNodeDef {
    pub id: String,
    pub label: String,
    pub parent: Option<String>,
    pub is_leaf: bool,
}

// ---------------------------------------------------------------------------
// Internal tree helpers
// ---------------------------------------------------------------------------

fn compute_tree_order(
    defs: &[TreeNodeDef],
    nodes: &[(String, TreeNode)],
) -> Vec<String> {
    let node_map: std::collections::HashMap<&str, &TreeNode> =
        nodes.iter().map(|(id, node)| (id.as_str(), node)).collect();

    let roots: Vec<&str> = defs
        .iter()
        .filter(|def| def.parent.is_none())
        .map(|def| def.id.as_str())
        .collect();

    let mut order = Vec::new();
    fn walk(id: &str, node_map: &std::collections::HashMap<&str, &TreeNode>, order: &mut Vec<String>) {
        order.push(id.to_string());
        if let Some(node) = node_map.get(id) {
            for child in &node.children {
                walk(child, node_map, order);
            }
        }
    }
    for root in roots {
        walk(root, &node_map, &mut order);
    }
    order
}

fn tree_prefix(
    id: &str,
    nodes: &[(String, TreeNode)],
) -> (String, bool) {
    let node_map: std::collections::HashMap<&str, &TreeNode> =
        nodes.iter().map(|(id, node)| (id.as_str(), node)).collect();

    let mut parent_chain: Vec<(&str, &str)> = Vec::new();
    let mut current = id;
    while let Some(node) = node_map.get(current) {
        if let Some(parent) = node.parent.as_deref() {
            parent_chain.push((current, parent));
            current = parent;
        } else {
            break;
        }
    }

    let mut prefix = String::new();
    for (idx, (child, parent)) in parent_chain.iter().rev().enumerate() {
        let parent_node = match node_map.get(*parent) {
            Some(node) => node,
            None => continue,
        };
        let is_last = parent_node
            .children
            .last()
            .map(|last| last.as_str() == *child)
            .unwrap_or(true);
        let is_direct_parent = idx + 1 == parent_chain.len();
        if is_direct_parent {
            prefix.push_str(if is_last { "└─ " } else { "├─ " });
        } else {
            prefix.push_str(if is_last { "   " } else { "│  " });
        }
    }

    let is_root = parent_chain.is_empty();
    (prefix, is_root)
}

fn leaf_progress(node: &TreeNode) -> f32 {
    match node.status {
        NodeStatus::Pending | NodeStatus::Running => node.progress,
        NodeStatus::Done | NodeStatus::Failed => 1.0,
    }
}

fn aggregate_node(
    id: &str,
    node_map: &std::collections::HashMap<&str, &TreeNode>,
) -> (f32, NodeStatus, Option<String>) {
    let Some(node) = node_map.get(id) else {
        return (0.0, NodeStatus::Pending, None);
    };
    if node.children.is_empty() || node.is_leaf {
        return (leaf_progress(node), node.status, node.fail_log.clone());
    }

    let mut progress_sum = 0.0f32;
    let mut count = 0usize;
    let mut any_failed = false;
    let mut all_done = true;
    let mut first_fail_log: Option<String> = None;

    for child in &node.children {
        let (progress, status, fail_log) = aggregate_node(child, node_map);
        progress_sum += progress;
        count += 1;
        if matches!(status, NodeStatus::Failed) {
            any_failed = true;
            if first_fail_log.is_none() {
                first_fail_log = fail_log;
            }
        }
        if !matches!(status, NodeStatus::Done) {
            all_done = false;
        }
    }

    let progress = if count == 0 { 0.0 } else { progress_sum / count as f32 };
    let status = if any_failed {
        NodeStatus::Failed
    } else if count > 0 && all_done {
        NodeStatus::Done
    } else if count > 0 {
        NodeStatus::Running
    } else {
        NodeStatus::Pending
    };

    (progress, status, first_fail_log)
}

fn status_counts(nodes: &[(String, TreeNode)]) -> (usize, usize, usize, usize, usize) {
    let mut leaves = 0;
    let mut pending = 0;
    let mut running = 0;
    let mut done = 0;
    let mut failed = 0;

    for (_, node) in nodes {
        if !node.is_leaf {
            continue;
        }
        leaves += 1;
        match node.status {
            NodeStatus::Pending => pending += 1,
            NodeStatus::Running => running += 1,
            NodeStatus::Done => done += 1,
            NodeStatus::Failed => failed += 1,
        }
    }

    (leaves, pending, running, done, failed)
}

fn status_style(status: NodeStatus) -> Style {
    match status {
        NodeStatus::Pending => Style::new().fg(Color::DarkGray),
        NodeStatus::Running => Style::new().fg(Color::Yellow).bold(),
        NodeStatus::Done => Style::new().fg(Color::Green).bold(),
        NodeStatus::Failed => Style::new().fg(Color::Red).bold(),
    }
}

fn status_icon(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Pending => "○",
        NodeStatus::Running => "◉",
        NodeStatus::Done => "✓",
        NodeStatus::Failed => "✗",
    }
}

fn progress_percent(progress: f32) -> u16 {
    (progress.clamp(0.0, 1.0) * 100.0).round() as u16
}

// ---------------------------------------------------------------------------
// TUI state (shared with main.rs via Arc<Mutex<>>)
// ---------------------------------------------------------------------------

/// Shared state between the build threads and the render loop.
/// Thread-safe: all fields behind the Mutex.
pub struct TuiState {
    pub title: String,
    pub logs_dir: PathBuf,
    pub order: Vec<String>,
    /// (id, node) — order matters for tree rendering
    pub nodes: Vec<(String, TreeNode)>,
    pub tick: u64,
    pub stop: bool,
    pub progress_floor: std::collections::HashMap<String, f32>,
    /// IDs of non-leaf nodes whose children are hidden.
    pub collapsed: std::collections::HashSet<String>,
}

impl TuiState {
    pub fn new(title: String, logs_dir: PathBuf, defs: &[TreeNodeDef]) -> Self {
        let mut nodes: Vec<(String, TreeNode)> = defs
            .iter()
            .map(|def| {
                let expected = if def.is_leaf {
                    // Use a default — the actual historical lookup
                    // is done in main.rs. We'll set expected after init.
                    80u32
                } else {
                    0
                };
                (
                    def.id.clone(),
                    TreeNode {
                        id: def.id.clone(),
                        label: def.label.clone(),
                        parent: def.parent.clone(),
                        children: Vec::new(),
                        is_leaf: def.is_leaf,
                        status: NodeStatus::Pending,
                        last_line: String::new(),
                        fail_log: None,
                        progress: 0.0,
                        work_units_seen: 0,
                        work_units_expected: expected,
                        start_tick: 0,
                    },
                )
            })
            .collect();

        // Populate children
        let child_map: std::collections::HashMap<String, Vec<String>> = defs
            .iter()
            .filter_map(|def| {
                let parent = def.parent.as_deref()?;
                Some((parent.to_string(), def.id.clone()))
            })
            .fold(
                std::collections::HashMap::new(),
                |mut map: std::collections::HashMap<String, Vec<String>>, (parent, child)| {
                    map.entry(parent).or_default().push(child);
                    map
                },
            );

        for (parent, children) in child_map {
            if let Some((_, node)) = nodes.iter_mut().find(|(id, _)| id == &parent) {
                node.children = children;
            }
        }

        let order = compute_tree_order(defs, &nodes);
        Self {
            title,
            logs_dir,
            order,
            nodes,
            tick: 0,
            stop: false,
            progress_floor: std::collections::HashMap::new(),
            collapsed: {
                let mut c = std::collections::HashSet::new();
                for def in defs {
                    let child_count = defs.iter().filter(|d| d.parent.as_deref() == Some(def.id.as_str())).count();
                    if child_count > 4 {
                        c.insert(def.id.clone());
                    }
                }
                c
            },
        }
    }

    pub fn toggle_collapse(&mut self, id: &str) {
        // Only non-leaf nodes can be collapsed.
        let has_children = self.nodes.iter().any(|(_, node)| {
            node.parent.as_deref() == Some(id)
        });
        if !has_children {
            return;
        }
        if self.collapsed.contains(id) {
            self.collapsed.remove(id);
        } else {
            self.collapsed.insert(id.to_string());
        }
    }

    // Public API called from build threads (via Arc<Mutex<>>)

    pub fn start_task(&mut self, id: &str) {
        let tick = self.tick;
        if let Some((_, node)) = self.nodes.iter_mut().find(|(nid, _)| nid == id) {
            node.status = NodeStatus::Running;
            node.last_line.clear();
            node.fail_log = None;
            node.progress = node.progress.max(0.01);
            node.work_units_seen = 0;
            node.start_tick = tick;
        }
    }

    pub fn push_line(&mut self, id: &str, line: String) {
        let tick = self.tick;
        if let Some((_, node)) = self.nodes.iter_mut().find(|(nid, _)| nid == id) {
            node.last_line = line;
            // TODO: integrate work_units_from_line from main.rs
            if node.status == NodeStatus::Running {
                let expected = node.work_units_expected.max(1) as f32;
                let by_units = (node.work_units_seen as f32 / expected).clamp(0.0, 0.97);
                let elapsed = tick.saturating_sub(node.start_tick) as f32;
                let by_time = (0.02 + elapsed * 0.0025).min(0.90);
                node.progress = node.progress.max(by_units.max(by_time));
            }
        }
    }

    pub fn finish_task(&mut self, id: &str, failed: bool, fail_log: Option<String>) {
        if let Some((_, node)) = self.nodes.iter_mut().find(|(nid, _)| nid == id) {
            if failed {
                node.status = NodeStatus::Failed;
                node.fail_log = fail_log;
                node.progress = 1.0;
            } else {
                node.status = NodeStatus::Done;
                node.last_line = "done".to_string();
                node.fail_log = None;
                node.progress = 1.0;
            }
        }
    }

    pub fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    pub fn snapshot(&self) -> Snapshot {
        let node_map: std::collections::HashMap<&str, &TreeNode> = self
            .nodes
            .iter()
            .map(|(id, node)| (id.as_str(), node))
            .collect();

        // Collect which IDs should be hidden (descendants of collapsed nodes).
        let hidden: std::collections::HashSet<&str> = {
            let mut h = std::collections::HashSet::new();
            for cid in &self.collapsed {
                let mut stack: Vec<&str> = node_map
                    .get(cid.as_str())
                    .map(|n| n.children.iter().map(|s| s.as_str()).collect())
                    .unwrap_or_default();
                while let Some(c) = stack.pop() {
                    if h.insert(c) {
                        if let Some(n) = node_map.get(c) {
                            stack.extend(n.children.iter().map(|s| s.as_str()));
                        }
                    }
                }
            }
            h
        };

        let mut tree_rows: Vec<TreeRow> = Vec::new();
        for id in &self.order {
            if hidden.contains(id.as_str()) {
                continue;
            }
            let Some((_, node)) = self.nodes.iter().find(|(nid, _)| nid == id) else {
                continue;
            };
            let (raw_progress, status, fail_log) = aggregate_node(&node.id, &node_map);
            let previous = self.progress_floor.get(id).copied().unwrap_or(0.0);
            let progress = match status {
                NodeStatus::Done | NodeStatus::Failed => 1.0,
                NodeStatus::Pending | NodeStatus::Running => raw_progress.max(previous),
            }
            .clamp(0.0, 1.0);
            let (prefix, is_root) = tree_prefix(&node.id, &self.nodes);
            let has_children = !node.children.is_empty();
            tree_rows.push(TreeRow {
                id: id.clone(),
                label: node.label.clone(),
                prefix,
                is_root,
                has_children,
                progress,
                status,
                last_line: node.last_line.clone(),
                fail_log: fail_log.clone(),
            });
        }

        let (_, pending, running, done, failed) = status_counts(&self.nodes);

        let overall_id = self.order.first().cloned().unwrap_or_default();
        let (overall_progress, _, _) = aggregate_node(&overall_id, &node_map);

        Snapshot {
            title: self.title.clone(),
            logs_dir: self.logs_dir.display().to_string(),
            overall_progress,
            pending,
            running,
            done,
            failed,
            tree_rows,
        }
    }
}

/// Immutable snapshot for rendering.
pub struct TreeRow {
    pub id: String,
    pub label: String,
    pub prefix: String,
    pub is_root: bool,
    pub has_children: bool,
    pub progress: f32,
    pub status: NodeStatus,
    pub last_line: String,
    pub fail_log: Option<String>,
}

pub struct Snapshot {
    pub title: String,
    pub logs_dir: String,
    pub overall_progress: f32,
    pub pending: usize,
    pub running: usize,
    pub done: usize,
    pub failed: usize,
    pub tree_rows: Vec<TreeRow>,
}

// ---------------------------------------------------------------------------
// ratatui widgets
// ---------------------------------------------------------------------------

fn render_header(frame: &mut Frame, area: Rect, snapshot: &Snapshot) {
    let layout = Layout::vertical([
        Constraint::Length(1), // title + overall bar + %
        Constraint::Length(1), // run id
        Constraint::Length(1), // counts
        Constraint::Length(1), // hint
    ])
    .split(area);

    let overall_percent = progress_percent(snapshot.overall_progress);

    // Title bar: title + gauge + percent
    let gauge = Gauge::default()
        .gauge_style(Style::new().fg(Color::Cyan))
        .percent(overall_percent)
        .label(format!("{}%", overall_percent));
    let title_area = layout[0];
    let _title_width = title_area.width as usize;
    let title_text = format!("{}", snapshot.title);
    let gauge_area = Rect {
        x: title_area.x + title_text.len() as u16 + 2,
        y: title_area.y,
        width: 28.min(title_area.width.saturating_sub(title_text.len() as u16 + 6)),
        height: 1,
    };
    let pct_area = Rect {
        x: gauge_area.x + gauge_area.width + 2,
        y: title_area.y,
        width: 6.min(title_area.width.saturating_sub(gauge_area.x + gauge_area.width + 2)),
        height: 1,
    };

    let title_span = Span::styled(
        &snapshot.title,
        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
    );
    frame.render_widget(Paragraph::new(Line::from(title_span)), title_area);
    frame.render_widget(gauge, gauge_area);
    let pct = Span::styled(
        format!("{:>3}%", overall_percent),
        Style::new().fg(Color::Yellow).bold(),
    );
    frame.render_widget(Paragraph::new(Line::from(pct)), pct_area);

    // Run ID + counts line
    let run_id = snapshot
        .logs_dir
        .rsplit('/')
        .next()
        .unwrap_or("unknown");
    let counts_line = Line::from(vec![
        Span::styled("run: ", Style::new().fg(Color::DarkGray)),
        Span::styled(run_id.to_string(), Style::new().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled(format!("done:{} ", snapshot.done), Style::new().fg(Color::Green)),
        Span::styled(format!("run:{} ", snapshot.running), Style::new().fg(Color::Yellow)),
        Span::styled(format!("pend:{} ", snapshot.pending), Style::new().fg(Color::DarkGray)),
        Span::styled(
            format!("fail:{}", snapshot.failed),
            if snapshot.failed > 0 {
                Style::new().fg(Color::Red).bold()
            } else {
                Style::new().fg(Color::Green)
            },
        ),
    ]);
    frame.render_widget(Paragraph::new(counts_line), layout[1]);

    // Hint line
    let hint = Span::styled(
        "hint: cargo xtask logs --task <name> --follow     q to quit",
        Style::new().fg(Color::DarkGray),
    );
    frame.render_widget(Paragraph::new(Line::from(hint)), layout[2]);

    // Separator
    let sep = Span::styled(
        "─".repeat(area.width as usize),
        Style::new().fg(Color::Cyan),
    );
    frame.render_widget(Paragraph::new(Line::from(sep)), layout[3]);
}

fn render_tree_row(frame: &mut Frame, area: Rect, row: &TreeRow, collapsed: &std::collections::HashSet<String>) {
    let progress_percent = progress_percent(row.progress);

    let icon = status_icon(row.status);
    let style = status_style(row.status);

    let (label_style, prefix_style) = if row.is_root {
        (Style::new().fg(Color::White).bold(), Style::new())
    } else {
        (Style::new().fg(Color::Cyan), Style::new().fg(Color::Cyan))
    };

    let mut spans = vec![
        Span::styled(format!("{} ", icon), style),
    ];

    if !row.is_root {
        spans.push(Span::styled(&row.prefix, prefix_style));
    }
    // Collapse indicator
    if row.has_children {
        let marker = if collapsed.contains(&row.id) { "▸ " } else { "▾ " };
        spans.push(Span::styled(marker, Style::new().fg(Color::Yellow)));
    }
    spans.push(Span::styled(&row.label, label_style));
    spans.push(Span::raw(" "));

    // Progress bar (inline characters for compactness)
    let bar_width = 16usize;
    let filled = ((row.progress.clamp(0.0, 1.0)) * bar_width as f32).round() as usize;
    let bar_style = match row.status {
        NodeStatus::Pending => Style::new().fg(Color::DarkGray),
        NodeStatus::Running => Style::new().fg(Color::Cyan),
        NodeStatus::Done => Style::new().fg(Color::Green),
        NodeStatus::Failed => Style::new().fg(Color::Red),
    };
    let bar_str: String = (0..bar_width)
        .map(|i| if i < filled { '#' } else { '-' })
        .collect();
    spans.push(Span::styled(format!("[{}] ", bar_str), bar_style));

    // Percent
    let pct_style = match row.status {
        NodeStatus::Done => Style::new().fg(Color::Green),
        NodeStatus::Failed => Style::new().fg(Color::Red),
        _ => Style::new().fg(Color::Yellow),
    };
    spans.push(Span::styled(format!("{:>3}% ", progress_percent), pct_style));

    // Status label
    let status_label = match row.status {
        NodeStatus::Pending => "PENDING",
        NodeStatus::Running => "RUNNING",
        NodeStatus::Done => "DONE",
        NodeStatus::Failed => "FAILED",
    };
    spans.push(Span::styled(status_label, style));

    // Live log line
    if row.status == NodeStatus::Running && !row.last_line.is_empty() {
        spans.push(Span::raw("  "));
        let live = &row.last_line;
        let truncated: String = live.chars().take(40).collect();
        let dots = if live.chars().count() > 40 { "…" } else { "" };
        spans.push(Span::styled(
            format!("{}{}", truncated, dots),
            Style::new().fg(Color::DarkGray),
        ));
    }

    // Failed log hint
    if row.status == NodeStatus::Failed {
        if let Some(ref log) = row.fail_log {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("log: {}", log),
                Style::new().fg(Color::Red).bold(),
            ));
        }
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn render_tree(
    frame: &mut Frame,
    area: Rect,
    snapshot: &Snapshot,
    state: &mut ListState,
    scroll_offset: usize,
    collapsed: &std::collections::HashSet<String>,
) {
    let visible_count = area.height as usize;
    let total_rows = snapshot.tree_rows.len();
    let max_offset = total_rows.saturating_sub(visible_count);
    let offset = scroll_offset.min(max_offset);

    let visible_rows: Vec<&TreeRow> = snapshot.tree_rows.iter().skip(offset).take(visible_count).collect();

    let items: Vec<ListItem> = visible_rows
        .iter()
        .map(|_| ListItem::new(""))
        .collect();

    let list = List::new(items)
        .block(Block::default())
        .highlight_style(Style::new());
    frame.render_stateful_widget(list, area, state);

    let row_height = 1u16;
    for (i, row) in visible_rows.iter().enumerate() {
        let y = area.y + i as u16 * row_height;
        let row_area = Rect {
            x: area.x,
            y,
            width: area.width,
            height: row_height,
        };
        render_tree_row(frame, row_area, row, collapsed);
    }
}

fn render_ui(
    frame: &mut Frame,
    snapshot: &Snapshot,
    list_state: &mut ListState,
    scroll_offset: usize,
    collapsed: &std::collections::HashSet<String>,
) {
    let area = frame.area();

    // Layout: header (4 lines) + tree (rest)
    let layout = Layout::vertical([
        Constraint::Length(4), // header
        Constraint::Fill(1),   // tree
    ])
    .split(area);

    render_header(frame, layout[0], snapshot);
    render_tree(frame, layout[1], snapshot, list_state, scroll_offset, collapsed);
}

// ---------------------------------------------------------------------------
// TUI runner
// ---------------------------------------------------------------------------

/// Check if terminal supports TUI.
pub fn is_tui_capable() -> bool {
    io::stderr().is_terminal() && std::env::var_os("TERM").map_or(true, |t| t != "dumb")
}

/// Run the ratatui render loop on the shared state until `stop` is set.
///
/// `state` is the shared TuiState (Arc<Mutex<>>) that build threads
/// update. This function blocks until `state.stop` is true or the
/// user presses `q`.
pub fn run_tui(state: Arc<Mutex<TuiState>>) -> Result<()> {
    enable_raw_mode()?;
    let mut stderr = io::stderr();
    execute!(stderr, EnterAlternateScreen)?;

    let backend = CrosstermBackend::new(stderr);
    let mut terminal = Terminal::new(backend)?;
    let mut list_state = ListState::default();
    let mut scroll_offset: usize = 0;

    let result = (|| -> Result<()> {
        loop {
            {
                let s = state.lock().expect("TUI state lock poisoned");
                if s.stop {
                    break;
                }
            }

            let snapshot;
            let collapsed;
            {
                let mut s = state.lock().expect("TUI state lock poisoned");
                s.tick();
                snapshot = s.snapshot();
                collapsed = s.collapsed.clone();
            }

            // Auto-scroll: find first running or failed task
            if let Some(idx) = snapshot.tree_rows.iter().position(|r| {
                matches!(r.status, NodeStatus::Running | NodeStatus::Failed)
            }) {
                let visible = terminal.get_frame().area().height.saturating_sub(4) as usize;
                let view_end = scroll_offset.saturating_add(visible);
                if idx < scroll_offset || idx >= view_end {
                    scroll_offset = idx;
                }
            }

            let local_collapsed = collapsed;
            terminal.draw(|frame| {
                render_ui(frame, &snapshot, &mut list_state, scroll_offset, &local_collapsed);
            })?;

            if event::poll(Duration::from_millis(50))? {
                if let CrosstermEvent::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press {
                        match key.code {
                            KeyCode::Char('q') | KeyCode::Char('Q') | KeyCode::Esc => {
                                let mut s = state.lock().expect("TUI state lock poisoned");
                                s.stop = true;
                                break;
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                // Toggle collapse on currently-highlighted row.
                                // For simplicity: pick first non-leaf in view as target.
                                let snap = snapshot.tree_rows.get(scroll_offset);
                                if let Some(row) = snap {
                                    if row.has_children {
                                        let mut s = state.lock().expect("TUI state lock poisoned");
                                        s.toggle_collapse(&row.id);
                                    }
                                }
                            }
                            KeyCode::Up | KeyCode::Char('k') => {
                                scroll_offset = scroll_offset.saturating_sub(1);
                            }
                            KeyCode::Down | KeyCode::Char('j') => {
                                let max = snapshot.tree_rows.len().saturating_sub(1);
                                scroll_offset = (scroll_offset + 1).min(max);
                            }
                            KeyCode::PageUp => {
                                let visible = terminal.get_frame().area().height.saturating_sub(4) as usize;
                                scroll_offset = scroll_offset.saturating_sub(visible);
                            }
                            KeyCode::PageDown => {
                                let visible = terminal.get_frame().area().height.saturating_sub(4) as usize;
                                let max = snapshot.tree_rows.len().saturating_sub(1);
                                scroll_offset = (scroll_offset + visible).min(max);
                            }
                            KeyCode::Home => scroll_offset = 0,
                            KeyCode::End => {
                                scroll_offset = snapshot.tree_rows.len().saturating_sub(1);
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        Ok(())
    })();

    disable_raw_mode()?;
    execute!(io::stderr(), LeaveAlternateScreen)?;

    result
}