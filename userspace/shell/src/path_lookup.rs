//! PATH-based binary resolution for the shell.
//!
//! When the user types a bare command (no `spawn` prefix, no slash in
//! the name), the shell walks $PATH left-to-right looking for a CLUU
//! container image whose name matches. Existence is checked by stat'ing
//! `/var/images/<name>/manifest.toml`; the PATH dir itself is just an
//! envelope-driven gate for which command names are reachable. First
//! hit wins; lookup falls through to "unsupported command" otherwise.

use alloc::format;
use alloc::string::String;
use libcluu::fs::client::VfsClient;

/// Resolve a bare command name against $PATH. Walks PATH dirs in order,
/// stat'ing `<dir>/<name>`. First hit is realpath'd; if the canonical
/// path lives under `/var/images/<n>/...` the image name is returned.
/// User-visible PATH dirs (e.g. `/bin`) carry symlinks into the
/// container image tree, so this path-of-least-surprise resolution
/// works inside the restricted envelope view (no `/var/images` mount
/// required).
///
/// Returns `None` when the name contains a slash (caller dispatches as
/// a literal path), when PATH has no non-empty entry, or when nothing
/// in PATH resolves into a known image.
pub fn resolve(bare_name: &str, path_env: &str, vfs: &VfsClient) -> Option<String> {
    if bare_name.is_empty() || bare_name.contains('/') {
        return None;
    }
    for dir in path_env.split(':') {
        if dir.is_empty() {
            continue;
        }
        let candidate = if dir.ends_with('/') {
            format!("{}{}", dir, bare_name)
        } else {
            format!("{}/{}", dir, bare_name)
        };
        if vfs.stat(&candidate).is_err() {
            continue;
        }
        let canonical = match vfs.realpath(&candidate) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if let Some(image) = image_name_from_canonical(&canonical) {
            return Some(image);
        }
    }
    None
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
