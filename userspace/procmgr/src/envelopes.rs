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
    pub vt_text_mounts: Vec<String>,
    pub vt_graphical_mounts: Vec<String>,
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

/// Parse the contents of /etc/envelopes.toml into a list of Envelopes.
/// Returns Err(reason) on malformed input — caller should panic on Err
/// since boot can't proceed without valid envelopes.
#[allow(dead_code)]
pub fn parse_envelopes(toml_str: &str) -> Result<Vec<Envelope>, String> {
    use alloc::format;
    use alloc::string::ToString;

    let doc = libcluu::toml::parse(toml_str)
        .map_err(|e| format!("envelopes.toml parse error: {:?}", e))?;

    // Find every "envelope.<name>" main table (exactly one dot after prefix).
    let mut envelopes = Vec::new();
    for table in &doc.tables {
        if !table.name.starts_with("envelope.") { continue; }
        let suffix = &table.name["envelope.".len()..];
        if suffix.contains('.') { continue; } // sub-table (e.g. envelope.user.env), skip

        let name = suffix.to_string();

        // Parse mounts from "mode:path" string array (optional; defaults to empty).
        let mounts = if let Some(mounts_raw) = table.get_array("mounts") {
            let mut parsed = Vec::with_capacity(mounts_raw.len());
            for (idx, raw) in mounts_raw.iter().enumerate() {
                let (mode_str, path) = raw.split_once(':')
                    .ok_or_else(|| format!("envelope '{}' mount {} not in 'mode:path' form: '{}'", name, idx, raw))?;
                let mode = match mode_str {
                    "ro" | "readonly" => MountMode::Ro,
                    "rw" | "readwrite" => MountMode::Rw,
                    other => return Err(format!("envelope '{}' mount {} unknown mode '{}'", name, idx, other)),
                };
                if path.is_empty() {
                    return Err(format!("envelope '{}' mount {} has empty path", name, idx));
                }
                parsed.push(MountSpec { path: path.to_string(), mode });
            }
            parsed
        } else {
            Vec::new()
        };

        // Parse vt_text and vt_graphical sub-tables' mounts arrays (raw strings).
        let vt_text_mounts = parse_raw_mounts(&doc, &format!("envelope.{}.vt_text", name));
        let vt_graphical_mounts = parse_raw_mounts(&doc, &format!("envelope.{}.vt_graphical", name));

        // Parse env and env_template from the corresponding sub-tables.
        let env = parse_string_table(&doc, &format!("envelope.{}.env", name));
        let env_template = parse_string_table(&doc, &format!("envelope.{}.env_template", name));

        envelopes.push(Envelope { name, mounts, env, env_template, vt_text_mounts, vt_graphical_mounts });
    }
    Ok(envelopes)
}

/// Helper: pull every key/value pair from a named table as String→String.
fn parse_string_table(doc: &libcluu::toml::TomlDoc, name: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if let Some(table) = doc.table(name) {
        for entry in &table.entries {
            if let libcluu::toml::TomlValue::String(ref s) = entry.value {
                out.insert(entry.key.clone(), s.clone());
            }
        }
    }
    out
}

/// Helper: pull the `mounts` array from a named sub-table as raw strings.
fn parse_raw_mounts(doc: &libcluu::toml::TomlDoc, table_name: &str) -> Vec<String> {
    if let Some(table) = doc.table(table_name) {
        if let Some(raw_vec) = table.get_array("mounts") {
            return raw_vec.into_iter().map(|s| s.clone()).collect();
        }
    }
    Vec::new()
}

/// Resolve the mount list for a session, applying `{vt}` substitution.
///
/// - `session_kind == 1` selects the graphical mount list; any other value selects text.
/// - If the selected list is empty, falls back to the legacy `env.mounts` field.
/// - `{vt}` in every entry is replaced with the decimal representation of `vt`.
#[allow(dead_code)]
pub fn resolve_session_mounts(env: &Envelope, session_kind: u8, vt: usize) -> Vec<String> {
    use alloc::format;

    let chosen = if session_kind == 1 {
        &env.vt_graphical_mounts
    } else {
        &env.vt_text_mounts
    };

    let vt_str = format!("{}", vt);

    if !chosen.is_empty() {
        chosen.iter().map(|s| s.replace("{vt}", &vt_str)).collect()
    } else {
        // Fall back to legacy mounts.
        env.mounts.iter().map(|m| {
            let prefix = match m.mode {
                MountMode::Ro => "ro",
                MountMode::Rw => "rw",
            };
            format!("{}:{}", prefix, m.path).replace("{vt}", &vt_str)
        }).collect()
    }
}

/// Look up an envelope by name in a parsed list.
#[allow(dead_code)]
pub fn lookup_envelope<'a>(envelopes: &'a [Envelope], name: &str) -> Option<&'a Envelope> {
    envelopes.iter().find(|e| e.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    const SAMPLE: &str = r#"
[envelope.user]
mounts = ["ro:/etc", "rw:/tmp"]

[envelope.user.env]
PATH = "/bin:/usr/bin"

[envelope.user.env_template]
HOME = "/home/{user}"
"#;

    #[test]
    fn parses_basic_envelope() {
        let envs = parse_envelopes(SAMPLE).expect("parse");
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].name, "user");
        assert_eq!(envs[0].mounts.len(), 2);
        assert_eq!(envs[0].mounts[0].path, "/etc");
        assert_eq!(envs[0].mounts[0].mode, MountMode::Ro);
        assert_eq!(envs[0].mounts[1].path, "/tmp");
        assert_eq!(envs[0].mounts[1].mode, MountMode::Rw);
        assert_eq!(envs[0].env.get("PATH").map(String::as_str), Some("/bin:/usr/bin"));
        assert_eq!(envs[0].env_template.get("HOME").map(String::as_str), Some("/home/{user}"));
    }

    #[test]
    fn substitutes_user_template() {
        let envs = parse_envelopes(SAMPLE).expect("parse");
        let resolved = resolve_env(&envs[0], "balazs");
        assert_eq!(resolved.get("HOME").map(String::as_str), Some("/home/balazs"));
        assert_eq!(resolved.get("PATH").map(String::as_str), Some("/bin:/usr/bin"));
    }

    #[test]
    fn rejects_bad_mode() {
        let bad = r#"
[envelope.x]
mounts = ["weird:/etc"]
"#;
        assert!(parse_envelopes(bad).is_err());
    }

    #[test]
    fn vt_substitution_in_mount_paths() {
        let toml_input = r#"
[envelope.user]
[envelope.user.env]
SHELL = "/bin/shell"
[envelope.user.env_template]
HOME = "/home/{user}"
[envelope.user.vt_text]
mounts = ["ro:/dev/tty{vt}", "ro:/dev/null"]
[envelope.user.vt_graphical]
mounts = ["rw:/dev/pts", "rw:/dev/fb0"]
"#;
        let envs = parse_envelopes(toml_input).expect("parse must succeed");
        let env = &envs[0];

        let mounts_text = resolve_session_mounts(env, /* session_kind */ 0, /* vt */ 2);
        assert_eq!(mounts_text, vec![
            String::from("ro:/dev/tty2"),
            String::from("ro:/dev/null"),
        ]);

        let mounts_graphical = resolve_session_mounts(env, /* session_kind */ 1, /* vt */ 4);
        assert_eq!(mounts_graphical, vec![
            String::from("rw:/dev/pts"),
            String::from("rw:/dev/fb0"),
        ]);
    }
}
