//! Per-client VFS views for capability-based path filtering.
//!
//! Each client (identified by sender_tid) can have a `VfsView` that restricts
//! which paths it can access.  Clients with no registered view get unrestricted
//! access (backward compatibility during transition).

use alloc::collections::BTreeMap;
use alloc::string::String;
use libcluu::{Error, Result};

/// A single mount rule within a view: maps a virtual path to a real backing path.
pub struct ViewMount {
    /// Real backing path in the VFS mount table (e.g. "/data/home").
    pub src: String,
    /// Virtual path seen by the client (e.g. "/home").
    pub dst: String,
    /// Whether the client can write through this mount.
    pub writable: bool,
}

/// A client's view of the filesystem: an ordered list of mount rules.
/// First matching rule wins during path resolution.
pub struct VfsView {
    pub mounts: alloc::vec::Vec<ViewMount>,
}

impl VfsView {
    /// Resolve a client-visible path to the real backing path.
    ///
    /// Returns `Some((real_path, writable))` if the path matches a mount rule,
    /// `None` if the path is outside the view entirely.
    pub fn resolve_path(&self, path: &str) -> Option<(String, bool)> {
        for m in &self.mounts {
            if let Some(real) = rewrite_path(path, &m.dst, &m.src) {
                return Some((real, m.writable));
            }
        }
        None
    }
}

/// Table of per-client views, keyed by client_id (sender_tid).
pub struct VfsViewTable {
    views: BTreeMap<usize, VfsView>,
}

impl VfsViewTable {
    pub fn new() -> Self {
        Self {
            views: BTreeMap::new(),
        }
    }

    /// Register a view for a client.
    pub fn set_view(&mut self, client_id: usize, view: VfsView) {
        self.views.insert(client_id, view);
    }

    /// Remove a client's view (cleanup on process exit).
    pub fn remove_view(&mut self, client_id: usize) {
        self.views.remove(&client_id);
    }

    /// Check and rewrite a path for a client.
    ///
    /// - If no view is registered for `client_id`, passes through unchanged
    ///   (backward compatibility).
    /// - If a view exists but the path doesn't match any mount rule, returns
    ///   `Err(Error::NotFound)` — indistinguishable from "path doesn't exist."
    /// - Otherwise returns the rewritten real path.
    pub fn check_path(&self, client_id: usize, path: &str) -> Result<String> {
        let view = match self.views.get(&client_id) {
            Some(v) => v,
            None => return Ok(String::from(path)),
        };
        match view.resolve_path(path) {
            Some((real_path, _)) => Ok(real_path),
            None => Err(Error::NotFound),
        }
    }

    /// Check a path for a client and also return the writable flag.
    ///
    /// Same semantics as `check_path`, but also indicates whether the
    /// resolved mount allows writes.
    pub fn check_path_writable(&self, client_id: usize, path: &str) -> Result<(String, bool)> {
        let view = match self.views.get(&client_id) {
            Some(v) => v,
            None => return Ok((String::from(path), true)),
        };
        match view.resolve_path(path) {
            Some(result) => Ok(result),
            None => Err(Error::NotFound),
        }
    }
}

/// Rewrite `path` if it falls under `dst_prefix`, replacing that prefix with `src_prefix`.
///
/// Matches:
/// - Exact: path == dst_prefix
/// - Subtree: path starts with dst_prefix + "/"
fn rewrite_path(path: &str, dst_prefix: &str, src_prefix: &str) -> Option<String> {
    if dst_prefix == "/" {
        // Root mount matches all absolute paths.
        if src_prefix == "/" {
            return Some(String::from(path));
        }
        let suffix = path.strip_prefix('/').unwrap_or(path);
        let mut real = String::with_capacity(src_prefix.len() + 1 + suffix.len());
        real.push_str(src_prefix);
        if !suffix.is_empty() {
            real.push('/');
            real.push_str(suffix);
        }
        return Some(real);
    }
    if path == dst_prefix {
        return Some(String::from(src_prefix));
    }
    if let Some(rest) = path.strip_prefix(dst_prefix) {
        // Require path-component boundary: "/bin" matches "/bin/ls" but not "/binary".
        if rest.starts_with('/') {
            let mut real = String::with_capacity(src_prefix.len() + rest.len());
            real.push_str(src_prefix);
            real.push_str(rest);
            return Some(real);
        }
    }
    None
}
