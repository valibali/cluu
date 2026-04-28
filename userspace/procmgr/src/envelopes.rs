//! Per-profile-class user envelope definitions.
//!
//! Loaded from /etc/envelopes.toml at procmgr boot. Each user record in
//! /etc/users.toml has a profile field that selects an envelope. The
//! envelope provides the mount view + env defaults at session-login.
//!
//! See docs/superpowers/specs/2026-04-28-user-envelope-design.md.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MountMode {
    Ro,
    Rw,
}

#[derive(Clone, Debug)]
pub struct MountSpec {
    pub path: String,
    pub mode: MountMode,
}

#[derive(Clone, Debug)]
pub struct Envelope {
    pub name: String,
    pub mounts: Vec<MountSpec>,
    pub env: BTreeMap<String, String>,
    pub env_template: BTreeMap<String, String>,
}

/// Apply `{user}` substitution to env_template, merging with static env.
/// Static env wins on key conflict (matches spec §6 step 3).
#[allow(dead_code)]
pub fn resolve_env(envelope: &Envelope, user: &str) -> BTreeMap<String, String> {
    let mut out = envelope.env.clone();
    for (k, template) in &envelope.env_template {
        let resolved = template.replace("{user}", user);
        out.entry(k.clone()).or_insert(resolved);
    }
    out
}
