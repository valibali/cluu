//! TreeBuilder — build a display-ordered tree from flat parent/child IDs.
//!
//! Utility for apps like `top` that have flat records with parent IDs and
//! need to render a tree hierarchy with `├──` / `└──` connectors.

extern crate alloc;

use alloc::string::String;
use alloc::string::ToString;
use alloc::vec::Vec;
use alloc::collections::BTreeMap;

pub struct FlatNode {
    pub id: u64,
    pub parent_id: u64,
    pub label: String,
}

pub struct TreeEntry {
    pub connector: String,
    pub label: String,
    pub depth: usize,
    pub id: u64,
}

pub fn build_tree(nodes: &[FlatNode]) -> Vec<TreeEntry> {
    let mut by_id: BTreeMap<u64, usize> = BTreeMap::new();
    let mut children: BTreeMap<u64, Vec<usize>> = BTreeMap::new();
    let mut roots: Vec<usize> = Vec::new();

    for (idx, node) in nodes.iter().enumerate() {
        by_id.insert(node.id, idx);
    }

    for (idx, node) in nodes.iter().enumerate() {
        if node.parent_id != 0 && by_id.contains_key(&node.parent_id) {
            children.entry(node.parent_id).or_insert_with(Vec::new).push(idx);
        } else {
            roots.push(idx);
        }
    }

    roots.sort_unstable_by_key(|&i| nodes[i].id);

    if roots.is_empty() && !nodes.is_empty() {
        let min_idx = nodes.iter().enumerate().min_by_key(|(_, n)| n.id).map(|(i, _)| i);
        if let Some(idx) = min_idx {
            roots.push(idx);
        }
    }

    let mut result: Vec<TreeEntry> = Vec::new();
    let mut visited: BTreeMap<usize, ()> = BTreeMap::new();

    for (i, &root_idx) in roots.iter().enumerate() {
        let is_last = i == roots.len() - 1;
        build_dfs(root_idx, "", is_last, true, nodes, &children, &mut result, &mut visited);
    }

    result
}

fn build_dfs(
    idx: usize,
    prefix: &str,
    is_last: bool,
    is_root: bool,
    nodes: &[FlatNode],
    children: &BTreeMap<u64, Vec<usize>>,
    out: &mut Vec<TreeEntry>,
    visited: &mut BTreeMap<usize, ()>,
) {
    if visited.contains_key(&idx) {
        return;
    }
    visited.insert(idx, ());

    let connector = if is_root {
        String::new()
    } else if is_last {
        alloc::format!("{}\u{2514}\u{2500}\u{2500} ", prefix)
    } else {
        alloc::format!("{}\u{251C}\u{2500}\u{2500} ", prefix)
    };

    let depth = if is_root { 0 } else { 1 };

    out.push(TreeEntry {
        connector: connector.clone(),
        label: nodes[idx].label.clone(),
        depth,
        id: nodes[idx].id,
    });

    let cid = nodes[idx].id;
    if let Some(kids) = children.get(&cid) {
        let mut sorted = kids.clone();
        sorted.sort_unstable_by_key(|&i| nodes[i].id);
        for (i, &kid) in sorted.iter().enumerate() {
            let child_prefix = if is_root {
                String::new()
            } else if is_last {
                alloc::format!("{}    ", prefix)
            } else {
                alloc::format!("{}\u{2502}   ", prefix)
            };
            let kid_is_last = i == sorted.len() - 1;
            build_dfs(kid, &child_prefix, kid_is_last, false, nodes, children, out, visited);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn node(id: u64, parent_id: u64, label: &str) -> FlatNode {
        FlatNode { id, parent_id, label: String::from(label) }
    }

    #[test]
    fn flat_single_root() {
        let nodes = vec![node(1, 0, "root")];
        let tree = build_tree(&nodes);
        assert_eq!(tree.len(), 1);
        assert_eq!(tree[0].label, "root");
        assert!(tree[0].connector.is_empty());
    }

    #[test]
    fn flat_parent_child() {
        let nodes = vec![
            node(1, 0, "root"),
            node(2, 1, "child"),
        ];
        let tree = build_tree(&nodes);
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].label, "root");
        assert_eq!(tree[1].label, "child");
        assert!(tree[1].connector.contains('\u{2514}'));
    }

    #[test]
    fn flat_two_children_last_has_corner() {
        let nodes = vec![
            node(1, 0, "root"),
            node(2, 1, "first"),
            node(3, 1, "second"),
        ];
        let tree = build_tree(&nodes);
        assert_eq!(tree.len(), 3);
        assert!(tree[1].connector.contains('\u{251C}'));
        assert!(tree[2].connector.contains('\u{2514}'));
    }

    #[test]
    fn flat_nested_tree() {
        let nodes = vec![
            node(1, 0, "a"),
            node(2, 1, "b"),
            node(3, 2, "c"),
        ];
        let tree = build_tree(&nodes);
        assert_eq!(tree.len(), 3);
        assert_eq!(tree[2].label, "c");
        assert!(tree[2].connector.contains('\u{2514}'));
    }

    #[test]
    fn flat_orphan_becomes_root() {
        let nodes = vec![
            node(1, 0, "root"),
            node(2, 99, "orphan"),
        ];
        let tree = build_tree(&nodes);
        assert_eq!(tree.len(), 2);
        assert!(tree[0].connector.is_empty());
        assert!(tree[1].connector.is_empty());
    }

    #[test]
    fn flat_sorted_by_id() {
        let nodes = vec![
            node(3, 0, "c"),
            node(1, 0, "a"),
            node(2, 0, "b"),
        ];
        let tree = build_tree(&nodes);
        assert_eq!(tree[0].label, "a");
        assert_eq!(tree[1].label, "b");
        assert_eq!(tree[2].label, "c");
    }

    #[test]
    fn flat_cycle_safe() {
        let nodes = vec![
            node(1, 2, "a"),
            node(2, 1, "b"),
        ];
        let tree = build_tree(&nodes);
        // Both become roots since neither's parent is found as non-root
        assert!(tree.len() >= 1);
    }
}
