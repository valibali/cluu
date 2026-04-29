//! PATH-based binary resolution for the shell.
//!
//! When the user types a bare command (no `spawn` prefix, no slash in
//! the name), the shell walks $PATH left-to-right looking for a CLUU
//! container image whose name matches. Existence is checked by stat'ing
//! `/var/images/<name>/manifest.toml`; the PATH dir itself is just an
//! envelope-driven gate for which command names are reachable. First
//! hit wins; lookup falls through to "unsupported command" otherwise.

use alloc::format;
use alloc::string::{String, ToString};
use libcluu::fs::client::VfsClient;

/// Resolve a bare command name against $PATH. Returns `Some(name)` if
/// `<name>` is a known container image and the user's $PATH has at
/// least one non-empty directory entry (i.e. PATH is configured for
/// the session). Returns `None` if the name contains a slash (caller
/// should treat it as a literal path), if PATH is empty, or if no
/// matching container manifest exists.
pub fn resolve(bare_name: &str, path_env: &str, vfs: &VfsClient) -> Option<String> {
    if bare_name.is_empty() || bare_name.contains('/') {
        return None;
    }

    // The container model: every binary lives at /var/images/<name>/manifest.toml,
    // not at the directory listed in PATH. PATH controls reachability — if a user's
    // envelope only has /bin in PATH, we still gate the lookup so that a binary
    // not "in /bin" semantically isn't reachable. For now, accept any non-empty
    // PATH; tighten later if we need per-PATH-dir filtering (TODO: scope by
    // PATH-dir membership once binaries declare their canonical PATH bucket).
    let path_has_at_least_one_dir = path_env.split(':').any(|d| !d.is_empty());
    if !path_has_at_least_one_dir {
        return None;
    }

    let manifest_path = format!("/var/images/{}/manifest.toml", bare_name);
    match vfs.stat(&manifest_path) {
        Ok(_) => Some(bare_name.to_string()),
        Err(_) => None,
    }
}
