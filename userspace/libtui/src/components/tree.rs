//! Tree — collapsible nested nodes with expand/collapse.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::buffer::{Cell, COLOR_DEFAULT, ATTR_BOLD};
use crate::layout::{Drawable, Rect};
use crate::View;

#[derive(Debug, Clone)]
pub struct TreeNode {
    label: String,
    children: Vec<TreeNode>,
    expanded: bool,
    fg: u8,
}

impl TreeNode {
    pub fn new(label: &str) -> Self {
        TreeNode {
            label: String::from(label),
            children: Vec::new(),
            expanded: false,
            fg: COLOR_DEFAULT,
        }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn fg(mut self, fg: u8) -> Self {
        self.fg = fg;
        self
    }

    pub fn child(mut self, child: TreeNode) -> Self {
        self.children.push(child);
        self
    }

    pub fn children(&self) -> &[TreeNode] {
        &self.children
    }

    pub fn is_leaf(&self) -> bool {
        self.children.is_empty()
    }

    pub fn is_expanded(&self) -> bool {
        self.expanded
    }

    pub fn toggle(&mut self) {
        if !self.is_leaf() {
            self.expanded = !self.expanded;
        }
    }

    pub fn expand(&mut self) {
        self.expanded = true;
    }

    pub fn collapse(&mut self) {
        self.expanded = false;
    }

    pub fn expand_all(&mut self) {
        self.expanded = true;
        for child in &mut self.children {
            child.expand_all();
        }
    }
}

pub struct Tree {
    root: TreeNode,
    cursor_path: Vec<usize>,
    selected_fg: u8,
    selected_bg: u8,
    indent: usize,
}

impl Tree {
    pub fn new(root: TreeNode) -> Self {
        Tree {
            root,
            cursor_path: Vec::new(),
            selected_fg: COLOR_DEFAULT,
            selected_bg: 4,
            indent: 2,
        }
    }

    pub fn selected_fg(mut self, fg: u8) -> Self { self.selected_fg = fg; self }
    pub fn selected_bg(mut self, bg: u8) -> Self { self.selected_bg = bg; self }
    pub fn indent(mut self, n: usize) -> Self { self.indent = n; self }

    pub fn root(&self) -> &TreeNode {
        &self.root
    }

    pub fn root_mut(&mut self) -> &mut TreeNode {
        &mut self.root
    }

    pub fn cursor_down(&mut self) {
        let flat = self.flatten();
        let current_idx = flat.iter().position(|p| *p == self.cursor_path);
        if let Some(idx) = current_idx {
            if idx + 1 < flat.len() {
                self.cursor_path = flat[idx + 1].clone();
            }
        } else if !flat.is_empty() {
            self.cursor_path = flat[0].clone();
        }
    }

    pub fn cursor_up(&mut self) {
        let flat = self.flatten();
        let current_idx = flat.iter().position(|p| *p == self.cursor_path);
        if let Some(idx) = current_idx {
            if idx > 0 {
                self.cursor_path = flat[idx - 1].clone();
            }
        }
    }

    pub fn toggle_cursor(&mut self) {
        let path = self.cursor_path.clone();
        if let Some(node) = self.node_at_mut(&path) {
            node.toggle();
        }
    }

    pub fn selected_label(&self) -> Option<&str> {
        self.node_at(&self.cursor_path).map(|n| n.label.as_str())
    }

    fn node_at(&self, path: &[usize]) -> Option<&TreeNode> {
        let mut node = &self.root;
        for &idx in path {
            node = node.children.get(idx)?;
        }
        Some(node)
    }

    fn node_at_mut(&mut self, path: &[usize]) -> Option<&mut TreeNode> {
        let mut node = &mut self.root;
        for &idx in path {
            node = node.children.get_mut(idx)?;
        }
        Some(node)
    }

    fn flatten(&self) -> Vec<Vec<usize>> {
        let mut result = Vec::new();
        self.flatten_node(&self.root, &[], &mut result);
        result
    }

    fn flatten_node(&self, node: &TreeNode, path: &[usize], out: &mut Vec<Vec<usize>>) {
        out.push(path.to_vec());
        if node.expanded {
            for (i, child) in node.children.iter().enumerate() {
                let mut child_path = path.to_vec();
                child_path.push(i);
                self.flatten_node(child, &child_path, out);
            }
        }
    }
}

impl Drawable for Tree {
    fn draw(&self, area: Rect, buf: &mut View) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let flat = self.flatten();
        for (row, path) in flat.iter().enumerate() {
            if row >= area.height {
                break;
            }
            let depth = path.len();
            let x = area.x + depth * self.indent;
            let is_selected = *path == self.cursor_path;
            let node = self.node_at(path).unwrap();

            let marker = if node.is_leaf() {
                "  "
            } else if node.expanded {
                "▼ "
            } else {
                "▶ "
            };

            let mut col = x;
            for ch in marker.chars() {
                if col >= area.x + area.width { break; }
                let mut cell = Cell::new(ch).fg(node.fg);
                if is_selected { cell = cell.bg(self.selected_bg); }
                buf.set(area.y + row, col, cell);
                col += 1;
            }
            for ch in node.label.chars() {
                if col >= area.x + area.width { break; }
                let mut cell = Cell::new(ch).fg(node.fg);
                if is_selected {
                    cell = cell.fg(self.selected_fg).bg(self.selected_bg);
                }
                buf.set(area.y + row, col, cell);
                col += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_tree() -> Tree {
        Tree::new(
            TreeNode::new("root")
                .child(TreeNode::new("a"))
                .child(
                    TreeNode::new("b")
                        .child(TreeNode::new("b1"))
                        .child(TreeNode::new("b2"))
                )
                .child(TreeNode::new("c"))
        )
    }

    #[test]
    fn tree_new_collapsed() {
        let t = make_tree();
        assert!(!t.root().is_expanded());
        assert_eq!(t.root().children().len(), 3);
    }

    #[test]
    fn tree_expand_reveals_children() {
        let mut t = make_tree();
        t.root_mut().expand();
        assert!(t.root().is_expanded());
    }

    #[test]
    fn tree_flatten_collapsed() {
        let t = make_tree();
        let flat = t.flatten();
        assert_eq!(flat.len(), 1);
    }

    #[test]
    fn tree_flatten_expanded() {
        let mut t = make_tree();
        t.root_mut().expand();
        let flat = t.flatten();
        assert_eq!(flat.len(), 4);
    }

    #[test]
    fn tree_flatten_deep_expand() {
        let mut t = make_tree();
        t.root_mut().expand();
        t.root_mut().children[1].expand();
        let flat = t.flatten();
        assert_eq!(flat.len(), 6);
    }

    #[test]
    fn tree_cursor_down_up() {
        let mut t = make_tree();
        t.root_mut().expand();
        t.cursor_down();
        assert_eq!(t.selected_label(), Some("a"));
        t.cursor_down();
        assert_eq!(t.selected_label(), Some("b"));
        t.cursor_up();
        assert_eq!(t.selected_label(), Some("a"));
    }

    #[test]
    fn tree_toggle_cursor() {
        let mut t = make_tree();
        t.root_mut().expand();
        t.cursor_down();
        t.cursor_down();
        let label = t.selected_label();
        assert_eq!(label, Some("b"));
        t.toggle_cursor();
        assert!(t.root().children[1].is_expanded());
    }

    #[test]
    fn tree_draw_root() {
        let t = make_tree();
        let mut buf = View::new(20, 5);
        t.draw(Rect::new(0, 0, 20, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('▶'));
        assert_eq!(buf.get(0, 2).map(|c| c.ch), Some('r'));
    }

    #[test]
    fn tree_draw_expanded() {
        let mut t = make_tree();
        t.root_mut().expand();
        let mut buf = View::new(20, 5);
        t.draw(Rect::new(0, 0, 20, 5), &mut buf);
        assert_eq!(buf.get(0, 0).map(|c| c.ch), Some('▼'));
        assert_eq!(buf.get(1, 4).map(|c| c.ch), Some('a'));
    }

    #[test]
    fn tree_draw_selected() {
        let mut t = make_tree();
        t.root_mut().expand();
        t.cursor_down();
        let mut buf = View::new(20, 5);
        t.draw(Rect::new(0, 0, 20, 5), &mut buf);
        assert_eq!(buf.get(1, 2).map(|c| c.bg), Some(4));
    }

    #[test]
    fn tree_expand_all() {
        let mut t = make_tree();
        t.root_mut().expand_all();
        let flat = t.flatten();
        assert!(flat.len() > 4);
    }

    #[test]
    fn tree_is_leaf() {
        let t = make_tree();
        assert!(!t.root().is_leaf());
        assert!(t.root().children()[0].is_leaf());
    }
}
