//! Mount-policy parsing for container manifests.
//!
//! procmgr's TOML reader (`libcluu::toml`) does not expose array-of-tables
//! natively, so this module provides a minimal line-oriented fallback parser
//! for the `[[mounts.policy]]` sections in a `manifest.toml`.
//!
//! The resulting `Vec<MountPolicyEntry>` drives per-path inheritance policy
//! when building a container's VFS view (Task 7 consumer).

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Mount inheritance policy for a single path. Drives whether a nested
/// container's view inherits the parent's mount at that path or gets a
/// fresh backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountPolicy {
    /// Use the parent container's mount entry verbatim (same MemFs).
    Inherit,
    /// Replace with a fresh per-container backend (current hardcoded behavior).
    Private,
    /// Inherit, but force writable=false. Deferred — may be unimplemented for now.
    Ro,
}

// Fields consumed by Task 7's view-building block.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct MountPolicyEntry {
    pub path: String,
    pub policy: MountPolicy,
}

pub fn parse_mount_policy(s: &str) -> Option<MountPolicy> {
    match s {
        "inherit" => Some(MountPolicy::Inherit),
        "private" => Some(MountPolicy::Private),
        "ro" => Some(MountPolicy::Ro),
        _ => None,
    }
}

/// Minimal parser for `[[mounts.policy]]` array-of-tables in a manifest.toml.
/// Expects each entry to have `path = "..."` and `policy = "..."`.
///
/// Lines outside `[[mounts.policy]]` sections are ignored. A new array-of-tables
/// header or any other section header flushes the accumulator; incomplete
/// entries (missing path or policy, or an unknown policy value) are silently
/// dropped. This is intentional: the host-side Cluufile parser is the policing
/// layer.
pub fn parse_mount_policies_raw(manifest: &str) -> Vec<MountPolicyEntry> {
    let mut out: Vec<MountPolicyEntry> = Vec::new();
    let mut in_section = false;
    let mut path: Option<String> = None;
    let mut policy: Option<MountPolicy> = None;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[[") {
            if in_section {
                if let (Some(p), Some(pol)) = (path.take(), policy.take()) {
                    out.push(MountPolicyEntry { path: p, policy: pol });
                } else {
                    path = None;
                    policy = None;
                }
            }
            in_section = trimmed == "[[mounts.policy]]";
            continue;
        }
        if trimmed.starts_with('[') {
            if in_section {
                if let (Some(p), Some(pol)) = (path.take(), policy.take()) {
                    out.push(MountPolicyEntry { path: p, policy: pol });
                } else {
                    path = None;
                    policy = None;
                }
            }
            in_section = false;
            continue;
        }
        if !in_section {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("path = ") {
            path = Some(rest.trim_matches('"').to_string());
        } else if let Some(rest) = trimmed.strip_prefix("policy = ") {
            policy = parse_mount_policy(rest.trim_matches('"'));
        }
    }
    if in_section {
        if let (Some(p), Some(pol)) = (path, policy) {
            out.push(MountPolicyEntry { path: p, policy: pol });
        }
    }
    out
}

/// Default mount policy table. Paths not listed here get no entry (meaning
/// the view-inheritance code path applies without per-path fiddling).
///
/// - `/tmp → Inherit`: shell session anchor; child processes see shell's /tmp.
///   Containers that want isolation opt in via `MOUNT /tmp private`.
/// - `/log → Private`: per-container log scope is the whole point of /log.
///
/// Other paths like /data are handled via the PERSISTENT directive upstream
/// and do not appear in this table.
#[allow(dead_code)]
fn default_mount_policies() -> [(&'static str, MountPolicy); 2] {
    [
        ("/tmp", MountPolicy::Inherit),
        ("/log", MountPolicy::Private),
    ]
}

/// Compose defaults + Cluufile overrides into a single effective policy list.
/// Cluufile entries win over defaults on the same path. When `deny_inherit`
/// is set, returns an empty list because there's nothing to inherit — the
/// DENY_INHERIT code path already produces a fresh image-only view.
///
/// Wired in by Task 7 of the mount-policy plan.
#[allow(dead_code)]
pub fn resolve_effective_policies(
    cluufile_entries: &[MountPolicyEntry],
    deny_inherit: bool,
) -> Vec<MountPolicyEntry> {
    if deny_inherit {
        return Vec::new();
    }
    let mut out: Vec<MountPolicyEntry> = Vec::new();
    // Seed with defaults.
    for (path, policy) in default_mount_policies().iter() {
        out.push(MountPolicyEntry { path: path.to_string(), policy: *policy });
    }
    // Apply Cluufile overrides.
    for entry in cluufile_entries {
        if let Some(existing) = out.iter_mut().find(|e| e.path == entry.path) {
            existing.policy = entry.policy;
        } else {
            out.push(entry.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_single_entry() {
        let m = "[[mounts.policy]]\npath = \"/tmp\"\npolicy = \"inherit\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "/tmp");
        assert_eq!(out[0].policy, MountPolicy::Inherit);
    }

    #[test]
    fn parses_multiple_entries() {
        let m = "[[mounts.policy]]\npath = \"/tmp\"\npolicy = \"inherit\"\n\n[[mounts.policy]]\npath = \"/log\"\npolicy = \"private\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, "/tmp");
        assert_eq!(out[0].policy, MountPolicy::Inherit);
        assert_eq!(out[1].path, "/log");
        assert_eq!(out[1].policy, MountPolicy::Private);
    }

    #[test]
    fn ignores_other_sections() {
        let m = "[storage]\npersistent_dirs = [\"/data\"]\n\n[[mounts.policy]]\npath = \"/tmp\"\npolicy = \"private\"\n\n[exec]\nbinary = \"/bin/foo\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "/tmp");
        assert_eq!(out[0].policy, MountPolicy::Private);
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use alloc::vec;

    fn ep(path: &str, policy: MountPolicy) -> MountPolicyEntry {
        MountPolicyEntry { path: path.to_string(), policy }
    }

    fn lookup(policies: &[MountPolicyEntry], path: &str) -> Option<MountPolicy> {
        policies.iter().find(|e| e.path == path).map(|e| e.policy)
    }

    #[test]
    fn defaults_applied_when_no_cluufile_entries() {
        let resolved = resolve_effective_policies(&[], false);
        // /tmp defaults to Inherit, /log to Private.
        assert_eq!(lookup(&resolved, "/tmp"), Some(MountPolicy::Inherit));
        assert_eq!(lookup(&resolved, "/log"), Some(MountPolicy::Private));
    }

    #[test]
    fn cluufile_override_wins() {
        let custom = vec![ep("/tmp", MountPolicy::Private)];
        let resolved = resolve_effective_policies(&custom, false);
        assert_eq!(lookup(&resolved, "/tmp"), Some(MountPolicy::Private));
        // /log default still applies.
        assert_eq!(lookup(&resolved, "/log"), Some(MountPolicy::Private));
    }

    #[test]
    fn deny_inherit_yields_empty_policy_set() {
        let custom = vec![ep("/tmp", MountPolicy::Inherit)];
        let resolved = resolve_effective_policies(&custom, true);
        // DENY_INHERIT means no inheritance at all — MOUNT entries are ignored.
        assert!(resolved.is_empty());
    }

    #[test]
    fn cluufile_appends_new_path_not_in_defaults() {
        let custom = vec![ep("/opt", MountPolicy::Private)];
        let resolved = resolve_effective_policies(&custom, false);
        assert_eq!(lookup(&resolved, "/opt"), Some(MountPolicy::Private));
        // Defaults still present.
        assert_eq!(lookup(&resolved, "/tmp"), Some(MountPolicy::Inherit));
        assert_eq!(lookup(&resolved, "/log"), Some(MountPolicy::Private));
        assert_eq!(resolved.len(), 3);
    }
}
