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

use crate::envelopes::MountMode;

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
    #[allow(dead_code)]
    Ro,
}

// Fields consumed by Task 7's view-building block.
#[derive(Debug, Clone)]
pub struct MountPolicyEntry {
    pub path: String,
    pub policy: MountPolicy,
    pub mode: MountMode,
}

/// Parse a `MOUNT <path> <keyword>` keyword into a (policy, mode) pair.
///
/// UE12: keywords now communicate both the policy (Inherit vs Private) and
/// the writability mode (Ro vs Rw):
///
/// | keyword              | policy             | mode |
/// |----------------------|--------------------|------|
/// | `inherit`            | Inherit            | Rw   |
/// | `private`            | Private            | Rw   |
/// | `ro` / `readonly`    | Inherit            | Ro   |
/// | `rw` / `readwrite`   | Inherit            | Rw   |
///
/// Note: the legacy `MountPolicy::Ro` enum variant is kept for backwards
/// compatibility but is no longer emitted by this parser — `ro/readonly`
/// now resolves to `(Inherit, Ro)`.
pub fn parse_mount_policy(s: &str) -> Option<(MountPolicy, MountMode)> {
    match s {
        "inherit"          => Some((MountPolicy::Inherit, MountMode::Rw)),
        "private"          => Some((MountPolicy::Private, MountMode::Rw)),
        "ro" | "readonly"  => Some((MountPolicy::Inherit, MountMode::Ro)),
        "rw" | "readwrite" => Some((MountPolicy::Inherit, MountMode::Rw)),
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
    let mut policy_and_mode: Option<(MountPolicy, MountMode)> = None;
    for line in manifest.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("[[") {
            if in_section {
                if let (Some(p), Some(pol_mode)) = (path.take(), policy_and_mode.take()) {
                    out.push(MountPolicyEntry { path: p, policy: pol_mode.0, mode: pol_mode.1 });
                } else {
                    path = None;
                    policy_and_mode = None;
                }
            }
            in_section = trimmed == "[[mounts.policy]]";
            continue;
        }
        if trimmed.starts_with('[') {
            if in_section {
                if let (Some(p), Some(pol_mode)) = (path.take(), policy_and_mode.take()) {
                    out.push(MountPolicyEntry { path: p, policy: pol_mode.0, mode: pol_mode.1 });
                } else {
                    path = None;
                    policy_and_mode = None;
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
            policy_and_mode = parse_mount_policy(rest.trim_matches('"'));
        }
    }
    if in_section {
        if let (Some(p), Some(pol_mode)) = (path, policy_and_mode) {
            out.push(MountPolicyEntry { path: p, policy: pol_mode.0, mode: pol_mode.1 });
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
        out.push(MountPolicyEntry { path: path.to_string(), policy: *policy, mode: MountMode::Rw });
    }
    // Apply Cluufile overrides.
    for entry in cluufile_entries {
        if let Some(existing) = out.iter_mut().find(|e| e.path == entry.path) {
            existing.policy = entry.policy;
            existing.mode = entry.mode;
        } else {
            out.push(entry.clone());
        }
    }
    out
}

/// Validate that every Cluufile MOUNT directive is satisfiable by the parent's
/// view. Returns Err with a human-readable reason if any directive demands
/// more than the parent provides. Today the only "more" we check for is
/// rw-vs-ro (Cluufile asks Rw, parent provides only Ro).
///
/// `parent_view` shape mirrors `ViewMountList` from main.rs:
///   `(src, dst, writable, memfs_cid)`.
/// We accept it as a slice to keep `mount_policy.rs` independent of the
/// `ViewMountList` type alias.
///
/// Path matching uses longest-prefix-match on `dst`. The catch-all `dst == "/"`
/// matches any path (covers the supervisor-envelope `rw:/`). Boundary safety
/// (`/etc` vs `/etcetera`) is handled by appending `/` for the prefix test.
pub fn validate_cluufile_against_parent(
    cluufile_entries: &[MountPolicyEntry],
    parent_view: &[(String, String, bool, u64)],
) -> core::result::Result<(), alloc::string::String> {
    use alloc::format;
    for cl in cluufile_entries {
        let parent_mount = parent_view
            .iter()
            .filter(|(_, dst, _, _)| {
                cl.path == *dst
                    || dst == "/"
                    || (dst.ends_with('/') && cl.path.starts_with(dst.as_str()))
                    || (!dst.ends_with('/') && cl.path.starts_with(&format!("{}/", dst)))
            })
            .max_by_key(|(_, dst, _, _)| dst.len());

        let Some((_, _, parent_writable, _)) = parent_mount else {
            return Err(format!(
                "cluufile mismatch: {} not provided by parent view",
                cl.path
            ));
        };

        if matches!(cl.mode, MountMode::Rw) && !parent_writable {
            return Err(format!(
                "cluufile mismatch: {} requires rw, parent has ro",
                cl.path
            ));
        }
    }
    Ok(())
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
        // bare `inherit` defaults to Rw.
        assert_eq!(out[0].mode, MountMode::Rw);
    }

    #[test]
    fn parses_multiple_entries() {
        let m = "[[mounts.policy]]\npath = \"/tmp\"\npolicy = \"inherit\"\n\n[[mounts.policy]]\npath = \"/log\"\npolicy = \"private\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, "/tmp");
        assert_eq!(out[0].policy, MountPolicy::Inherit);
        assert_eq!(out[0].mode, MountMode::Rw);
        assert_eq!(out[1].path, "/log");
        assert_eq!(out[1].policy, MountPolicy::Private);
        assert_eq!(out[1].mode, MountMode::Rw);
    }

    #[test]
    fn ignores_other_sections() {
        let m = "[storage]\npersistent_dirs = [\"/data\"]\n\n[[mounts.policy]]\npath = \"/tmp\"\npolicy = \"private\"\n\n[exec]\nbinary = \"/bin/foo\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "/tmp");
        assert_eq!(out[0].policy, MountPolicy::Private);
        assert_eq!(out[0].mode, MountMode::Rw);
    }

    #[test]
    fn ro_keyword_emits_inherit_plus_ro_mode() {
        // UE12: `ro` (and its `readonly` alias) now mean "inherit, but readonly".
        let m = "[[mounts.policy]]\npath = \"/etc\"\npolicy = \"ro\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "/etc");
        assert_eq!(out[0].policy, MountPolicy::Inherit);
        assert_eq!(out[0].mode, MountMode::Ro);

        let m2 = "[[mounts.policy]]\npath = \"/etc\"\npolicy = \"readonly\"\n";
        let out2 = parse_mount_policies_raw(m2);
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].policy, MountPolicy::Inherit);
        assert_eq!(out2[0].mode, MountMode::Ro);
    }

    #[test]
    fn rw_keyword_emits_inherit_plus_rw_mode() {
        // UE12: `rw` (and its `readwrite` alias) explicitly request inherit + writable.
        let m = "[[mounts.policy]]\npath = \"/data\"\npolicy = \"rw\"\n";
        let out = parse_mount_policies_raw(m);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "/data");
        assert_eq!(out[0].policy, MountPolicy::Inherit);
        assert_eq!(out[0].mode, MountMode::Rw);

        let m2 = "[[mounts.policy]]\npath = \"/data\"\npolicy = \"readwrite\"\n";
        let out2 = parse_mount_policies_raw(m2);
        assert_eq!(out2.len(), 1);
        assert_eq!(out2[0].policy, MountPolicy::Inherit);
        assert_eq!(out2[0].mode, MountMode::Rw);
    }
}

#[cfg(test)]
mod resolve_tests {
    use super::*;
    use alloc::vec;

    fn ep(path: &str, policy: MountPolicy) -> MountPolicyEntry {
        MountPolicyEntry { path: path.to_string(), policy, mode: MountMode::Rw }
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

#[cfg(test)]
mod validate_tests {
    use super::*;
    use alloc::vec;

    fn ep(path: &str, mode: MountMode) -> MountPolicyEntry {
        MountPolicyEntry {
            path: path.to_string(),
            policy: MountPolicy::Inherit,
            mode,
        }
    }

    fn pv(dst: &str, writable: bool) -> (String, String, bool, u64) {
        (dst.to_string(), dst.to_string(), writable, 0)
    }

    #[test]
    fn rw_demand_against_ro_parent_rejects() {
        let cluufile = vec![ep("/etc", MountMode::Rw)];
        let parent = vec![pv("/etc", false)];
        let r = validate_cluufile_against_parent(&cluufile, &parent);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("requires rw, parent has ro"));
    }

    #[test]
    fn ro_demand_against_ro_parent_ok() {
        let cluufile = vec![ep("/etc", MountMode::Ro)];
        let parent = vec![pv("/etc", false)];
        assert!(validate_cluufile_against_parent(&cluufile, &parent).is_ok());
    }

    #[test]
    fn rw_demand_against_rw_parent_ok() {
        let cluufile = vec![ep("/etc", MountMode::Rw)];
        let parent = vec![pv("/etc", true)];
        assert!(validate_cluufile_against_parent(&cluufile, &parent).is_ok());
    }

    #[test]
    fn root_catchall_satisfies_any_path() {
        // Supervisor envelope: rw:/ → covers everything.
        let cluufile = vec![ep("/etc", MountMode::Rw)];
        let parent = vec![pv("/", true)];
        assert!(validate_cluufile_against_parent(&cluufile, &parent).is_ok());
    }

    #[test]
    fn empty_parent_view_rejects_any_demand() {
        let cluufile = vec![ep("/etc", MountMode::Rw)];
        let parent: Vec<(String, String, bool, u64)> = Vec::new();
        let r = validate_cluufile_against_parent(&cluufile, &parent);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("not provided"));
    }

    #[test]
    fn boundary_safety_etc_vs_etcetera() {
        // Cluufile asks /etcetera (rw), parent only provides /etc (ro).
        // /etc must NOT match /etcetera as a prefix.
        let cluufile = vec![ep("/etcetera", MountMode::Rw)];
        let parent = vec![pv("/etc", false)];
        let r = validate_cluufile_against_parent(&cluufile, &parent);
        assert!(r.is_err());
        assert!(r.unwrap_err().contains("not provided"));
    }

    #[test]
    fn longest_prefix_wins() {
        // Parent: /usr (ro), /usr/local (rw). Cluufile asks /usr/local/bin rw → ok.
        let cluufile = vec![ep("/usr/local/bin", MountMode::Rw)];
        let parent = vec![pv("/usr", false), pv("/usr/local", true)];
        assert!(validate_cluufile_against_parent(&cluufile, &parent).is_ok());
    }
}
