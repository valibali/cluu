//! Mount-policy parsing for container manifests.
//!
//! procmgr's TOML reader (`libcluu::toml`) does not expose array-of-tables
//! natively, so this module provides a minimal line-oriented fallback parser
//! for the `[[mounts.policy]]` sections in a `manifest.toml`.
//!
//! The resulting `Vec<MountPolicyEntry>` drives per-path inheritance policy
//! when building a container's VFS view (Task 7 consumer).

#![cfg_attr(not(test), no_std)]

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
