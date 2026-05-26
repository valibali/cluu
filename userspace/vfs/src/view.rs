//! Per-client VFS views for capability-based path filtering.
//!
//! Each client (identified by sender_tid) can have a `VfsView` that restricts
//! which paths it can access. If no explicit view is registered, VFS can fall
//! back to the client's stored capability profile defaults.

use alloc::collections::BTreeMap;
use alloc::string::String;
use libcluu::cap::CapProfile;
use libcluu::vfs_view;
use libcluu::{Error, Result};

use crate::pts::{PtsBackend, PtsRegistry};

/// Where a view mount routes I/O requests.
#[derive(Clone, Copy, PartialEq)]
pub enum MountTarget {
    /// Route through the global MountTable (initrd, ext2, devfs, procfs).
    MountTable,
    /// Route to a per-container in-memory filesystem.
    MemFs { container_id: u64 },
}

/// A single mount rule within a view: maps a virtual path to a real backing path.
pub struct ViewMount {
    /// Real backing path in the VFS mount table (e.g. "/data/home").
    /// For MemFs targets, this is the path prefix within the MemFs (e.g. "/tmp").
    pub src: String,
    /// Virtual path seen by the client (e.g. "/home").
    pub dst: String,
    /// Whether the client can write through this mount.
    pub writable: bool,
    /// Routing target for this mount.
    pub target: MountTarget,
}

/// A client's view of the filesystem: an ordered list of mount rules.
/// First matching rule wins during path resolution.
pub struct VfsView {
    pub mounts: alloc::vec::Vec<ViewMount>,
}

impl VfsView {
    /// Resolve a client-visible path to the real backing path.
    ///
    /// Returns `Some((real_path, writable, target))` if the path matches a mount rule,
    /// `None` if the path is outside the view entirely.
    pub fn resolve_path(&self, path: &str) -> Option<(String, bool, MountTarget)> {
        for m in &self.mounts {
            if let Some(real) = rewrite_path(path, &m.dst, &m.src) {
                return Some((real, m.writable, m.target));
            }
        }
        None
    }
}

/// Table of per-client views, keyed by client_id (sender_tid).
pub struct VfsViewTable {
    views: BTreeMap<usize, VfsView>,
    profiles: BTreeMap<usize, CapProfile>,
}

impl VfsViewTable {
    pub fn new() -> Self {
        Self {
            views: BTreeMap::new(),
            profiles: BTreeMap::new(),
        }
    }

    /// Register a view for a client.
    ///
    /// # Monotone rule
    ///
    /// Every child view must be a subset of its parent's view (paths present
    /// and write-permission no broader).  VFS does **not** enforce this here
    /// because `set_view` has no knowledge of the parent–child relationship —
    /// that lives entirely in procmgr (`pid_to_container_id`, `pid_to_view`).
    ///
    /// The assertion is therefore placed at the **procmgr call site**
    /// (`install_view_and_run`) where both the parent view and the proposed
    /// child view are available before the IPC is sent.
    ///
    /// TODO(plan2-task7): if/when a future refactor threads `parent_client_id`
    /// through the `VFS_SET_VIEW_LABEL` wire protocol, add a
    /// `#[cfg(debug_assertions)]` check here using `get_view(parent_client_id)`.
    pub fn set_view(&mut self, client_id: usize, view: VfsView) {
        self.views.insert(client_id, view);
    }

    pub fn set_profile(&mut self, client_id: usize, profile: CapProfile) {
        self.profiles.insert(client_id, profile);
    }

    /// Remove a client's view (cleanup on process exit).
    pub fn remove_view(&mut self, client_id: usize) {
        self.views.remove(&client_id);
        self.profiles.remove(&client_id);
    }

    /// Get the view for a client (if one is explicitly registered).
    pub fn get_view(&self, client_id: usize) -> Option<&VfsView> {
        self.views.get(&client_id)
    }

    /// Remove only the explicit view while keeping profile metadata.
    pub fn clear_explicit_view(&mut self, client_id: usize) {
        self.views.remove(&client_id);
    }

    fn resolve_effective_path(
        &self,
        client_id: usize,
        path: &str,
    ) -> Result<(String, bool, MountTarget)> {
        if let Some(view) = self.views.get(&client_id) {
            return view.resolve_path(path).ok_or(Error::NotFound);
        }
        let profile = match self.profiles.get(&client_id).copied() {
            Some(bits) => bits,
            None => return Err(Error::PermissionDenied),
        };
        default_view_for_profile(profile)
            .resolve_path(path)
            .ok_or(Error::NotFound)
    }

    /// Check and rewrite a path for a client.
    ///
    /// - If no explicit view is registered, falls back to profile defaults.
    ///   If no profile is known, returns `Err(Error::PermissionDenied)`.
    /// - If a view exists but the path doesn't match any mount rule, returns
    ///   `Err(Error::NotFound)` — indistinguishable from "path doesn't exist."
    /// - Otherwise returns the rewritten real path.
    #[allow(dead_code)]
    pub fn check_path(&self, client_id: usize, path: &str) -> Result<String> {
        validate_clean_absolute_path(path)?;
        self.resolve_effective_path(client_id, path)
            .map(|(real_path, _, _)| real_path)
    }

    /// Check a path for a client and also return the writable flag.
    ///
    /// Same semantics as `check_path`, but also indicates whether the
    /// resolved mount allows writes.
    #[allow(dead_code)]
    pub fn check_path_writable(&self, client_id: usize, path: &str) -> Result<(String, bool)> {
        validate_clean_absolute_path(path)?;
        self.resolve_effective_path(client_id, path)
            .map(|(real_path, writable, _)| (real_path, writable))
    }

    /// Check a path and return the resolved path + mount target.
    pub fn check_path_with_target(
        &self,
        client_id: usize,
        path: &str,
    ) -> Result<(String, MountTarget)> {
        validate_clean_absolute_path(path)?;
        self.resolve_effective_path(client_id, path)
            .map(|(real_path, _, target)| (real_path, target))
    }

    /// Check a path for write access and return the resolved path, writable flag, and target.
    pub fn check_path_writable_with_target(
        &self,
        client_id: usize,
        path: &str,
    ) -> Result<(String, bool, MountTarget)> {
        validate_clean_absolute_path(path)?;
        self.resolve_effective_path(client_id, path)
    }
}

fn default_view_for_profile(profile: CapProfile) -> VfsView {
    fn mount(src: &str, dst: &str, writable: bool) -> ViewMount {
        ViewMount {
            src: String::from(src),
            dst: String::from(dst),
            writable,
            target: MountTarget::MountTable,
        }
    }

    let mounts = vfs_view::default_mounts_for_profile(profile)
        .iter()
        .map(|(src, dst, writable)| mount(src, dst, *writable))
        .collect();

    VfsView { mounts }
}

/// Validate a request path before prefix matching/rewrite.
///
/// Policy:
/// - must be absolute
/// - reject any "." or ".." segment to prevent traversal escapes
pub fn validate_clean_absolute_path(path: &str) -> Result<()> {
    if !path.starts_with('/') {
        return Err(Error::InvalidArgument);
    }
    for segment in path.split('/').skip(1) {
        if segment == "." || segment == ".." {
            return Err(Error::PermissionDenied);
        }
    }
    Ok(())
}

/// Rewrite `path` if it falls under `dst_prefix`, replacing that prefix with `src_prefix`.
///
/// Matches:
/// - Exact: path == dst_prefix
/// - Subtree: path starts with dst_prefix + "/"
fn rewrite_path(path: &str, dst_prefix: &str, src_prefix: &str) -> Option<String> {
    if !path.starts_with('/') {
        return None;
    }
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

// ─── Per-session PTS overlay ──────────────────────────────────────────────────

/// Narrow a `/dev/pts` mount entry for a child view.
///
/// If `session_id` is `Some`, the mount is replaced with a session-private
/// `PtsBackend` that only shows the global pts entries plus those registered
/// under `session_id`.  This is called during view derivation when a child
/// process inherits the `/dev/pts` mount.
///
/// If `session_id` is `None` (sessionless caller), returns `None` — the
/// caller should not see `/dev/pts` at all, because there is no session
/// context to filter by.
///
/// The `registry_ptr` must point to the live `PtsRegistry` owned by the
/// VFS server.  SAFETY: same lifetime contract as other `PtsBackend` uses.
pub fn narrow_pts_mount(
    registry_ptr: *const PtsRegistry,
    session_id: Option<u32>,
) -> Option<alloc::boxed::Box<PtsBackend>> {
    match session_id {
        Some(sid) => Some(alloc::boxed::Box::new(PtsBackend::for_session(
            registry_ptr,
            sid,
        ))),
        None => None,
    }
}

// ─── Virtual-root synthesis ───────────────────────────────────────────────────

impl VfsViewTable {
    /// Return the synthetic child names visible under `path` if it is a proper
    /// prefix of at least one mount destination in the client's view.
    ///
    /// Only paths that ARE a prefix but are NOT themselves a mount destination
    /// produce entries — real mounts are handled by the normal resolve path.
    /// Entries are first-level basenames, sorted, deduped.
    pub fn virtual_resolve(
        &self,
        client_id: usize,
        path: &str,
    ) -> Option<alloc::vec::Vec<alloc::string::String>> {
        if validate_clean_absolute_path(path).is_err() {
            return None;
        }
        // Canonical prefix: strip trailing slash except for root.
        let prefix = if path == "/" {
            "/"
        } else {
            path.trim_end_matches('/')
        };

        // Pick the right mount list — explicit view wins, then profile default.
        let mounts: &alloc::vec::Vec<ViewMount> = if let Some(v) = self.views.get(&client_id) {
            &v.mounts
        } else {
            // profile fallback: synthesise on the stack is awkward without
            // alloc; we compute it directly.
            let profile = self.profiles.get(&client_id).copied()?;
            // Build a temporary VfsView and recurse once.
            let tmp = crate::view::default_view_for_profile_pub(profile);
            return Self::virtual_resolve_from_mounts(&tmp.mounts, prefix);
        };

        Self::virtual_resolve_from_mounts(mounts, prefix)
    }

    fn virtual_resolve_from_mounts(
        mounts: &[ViewMount],
        prefix: &str,
    ) -> Option<alloc::vec::Vec<alloc::string::String>> {
        let mut children: alloc::vec::Vec<alloc::string::String> = alloc::vec::Vec::new();

        for m in mounts {
            let dst = m.dst.trim_end_matches('/');
            // Skip if this dst IS the prefix — that is a real mount, not virtual.
            if dst == prefix {
                continue;
            }
            // Check that dst starts with prefix and the next char is '/'.
            let rest = if prefix == "/" {
                // Every absolute dst contributes its first segment.
                dst.strip_prefix('/')
            } else {
                dst.strip_prefix(prefix)
                    .and_then(|r| r.strip_prefix('/'))
            };
            let Some(rest) = rest else { continue };
            // First segment only (up to the next '/' or end-of-string).
            let segment = match rest.find('/') {
                Some(pos) => &rest[..pos],
                None => rest,
            };
            if segment.is_empty() {
                continue;
            }
            if !children.iter().any(|s| s == segment) {
                children.push(alloc::string::String::from(segment));
            }
        }

        if children.is_empty() {
            None
        } else {
            children.sort();
            Some(children)
        }
    }
}

/// Public wrapper around the profile→VfsView conversion (needed by virtual_resolve).
pub(crate) fn default_view_for_profile_pub(profile: libcluu::cap::CapProfile) -> VfsView {
    default_view_for_profile(profile)
}

// ─── Path helpers ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn user_view() -> VfsView {
        fn m(dst: &str) -> ViewMount {
            ViewMount {
                src: alloc::string::String::from(dst),
                dst: alloc::string::String::from(dst),
                writable: false,
                target: MountTarget::MountTable,
            }
        }
        VfsView {
            mounts: alloc::vec![
                m("/bin"),
                m("/lib"),
                m("/tmp"),
                m("/home/root"),
                m("/dev/initrd"),
                m("/dev/pts"),
                m("/proc"),
            ],
        }
    }

    fn table_with_view(id: usize, view: VfsView) -> VfsViewTable {
        let mut t = VfsViewTable::new();
        t.set_view(id, view);
        t
    }

    #[test]
    fn virtual_root_returns_top_level_names() {
        let t = table_with_view(1, user_view());
        let entries = t.virtual_resolve(1, "/").expect("should be virtual");
        // bin, dev, home, lib, proc, tmp — sorted, deduped
        assert_eq!(entries, alloc::vec!["bin", "dev", "home", "lib", "proc", "tmp"]);
    }

    #[test]
    fn virtual_dev_returns_children() {
        let t = table_with_view(1, user_view());
        let entries = t.virtual_resolve(1, "/dev").expect("should be virtual");
        assert_eq!(entries, alloc::vec!["initrd", "pts"]);
    }

    #[test]
    fn virtual_home_returns_children() {
        let t = table_with_view(1, user_view());
        let entries = t.virtual_resolve(1, "/home").expect("should be virtual");
        assert_eq!(entries, alloc::vec!["root"]);
    }

    #[test]
    fn real_mount_not_virtual() {
        let t = table_with_view(1, user_view());
        // /bin is a real mount dst — should NOT be virtual (None)
        assert!(t.virtual_resolve(1, "/bin").is_none());
    }

    #[test]
    fn non_prefix_path_returns_none() {
        let t = table_with_view(1, user_view());
        assert!(t.virtual_resolve(1, "/etc").is_none());
        assert!(t.virtual_resolve(1, "/home/root/docs").is_none());
    }

    #[test]
    fn no_view_no_profile_returns_none() {
        let t = VfsViewTable::new();
        assert!(t.virtual_resolve(99, "/").is_none());
    }
}
