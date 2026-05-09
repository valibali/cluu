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

/// Pull the container image name out of a canonical absolute path, when the
/// path lives inside `/var/images/<name>/...`. Returns `None` for any other
/// shape, including the `/var/images` root itself.
pub fn image_name_from_canonical(canonical: &str) -> Option<String> {
    let rest = canonical.strip_prefix("/var/images/")?;
    let (name, tail) = rest.split_once('/')?;
    if name.is_empty() || tail.is_empty() {
        return None;
    }
    Some(String::from(name))
}

/// Convert a user-typed command word into the bare image name procmgr
/// expects. Bare names pass through; paths-with-slashes are resolved via
/// `vfs.realpath` and then matched against `/var/images/<name>/...`.
/// Returns the original input unchanged when realpath fails or the
/// canonical path does not look like a CLUU image binary; the caller is
/// responsible for downstream error reporting.
pub fn resolve_to_image_name(name: &str, vfs: &VfsClient) -> String {
    if !name.contains('/') {
        return String::from(name);
    }
    match vfs.realpath(name) {
        Ok(canon) => image_name_from_canonical(&canon).unwrap_or_else(|| String::from(name)),
        Err(_) => String::from(name),
    }
}
