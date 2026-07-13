//! TAB completion candidate computation + lazy async directory cache.
//!
//! The shell main loop is async (libcluu::async_runtime). Completion queries
//! arrive on the completion endpoint and are handled inline. Directory
//! listings are fetched lazily via async VFS readdir on first TAB press —
//! no pre-caching thread, no startup cost.
//!
//! # Lock safety
//!
//! `DIR_CACHE` uses `spin::Mutex` but is never held across an `.await`.
//! The readdir task collects results in a local `Vec`, then locks the
//! cache only to insert — fully synchronous, no yield point. Since the
//! runtime is single-threaded, the lock is uncontended; it exists solely
//! to satisfy Rust's aliasing rules for the static.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::commands::BuiltinRegistry;
use libcluu::types::Message;
use libcluu::{debug_print, ipc};

use alloc::collections::{BTreeMap, BTreeSet};
use spin::Mutex;

use cluu_wire::pts::{
    CompleteReply, CompleteRequest, SHELL_COMPLETE_QUERY_LABEL,
};
use cluu_wire::ABI_VERSION;

// ── Lazy directory cache ────────────────────────────────────────────────────

static DIR_CACHE: Mutex<DirCache> = Mutex::new(DirCache::empty());

struct DirCache {
    entries: BTreeMap<String, Vec<String>>,
    pending: BTreeSet<String>,
}

impl DirCache {
    const fn empty() -> Self {
        Self {
            entries: BTreeMap::new(),
            pending: BTreeSet::new(),
        }
    }
}

const BASE_CACHED_DIRS: &[&str] = &["/bin", "/etc", "/dev", "/tmp", "/home"];

pub fn initial_dirs() -> Vec<String> {
    let home = crate::shellrc::home_from_env().unwrap_or_else(|| String::from("/"));
    let mut dirs: Vec<String> = BASE_CACHED_DIRS.iter().map(|s| String::from(*s)).collect();
    dirs.push(home);
    dirs.push(String::from("/var"));
    dirs.push(String::from("/var/images"));
    dirs
}

pub fn mark_pending(dir: &str) -> bool {
    let mut cache = DIR_CACHE.lock();
    if cache.entries.contains_key(dir) || cache.pending.contains(dir) {
        return false;
    }
    cache.pending.insert(String::from(dir));
    true
}

pub fn store_entries(dir: &str, entries: Vec<String>) {
    let mut cache = DIR_CACHE.lock();
    cache.pending.remove(dir);
    cache.entries.insert(String::from(dir), entries);
}

fn lookup_cached_dir(dir: &str) -> Vec<String> {
    let cache = DIR_CACHE.lock();
    if dir == "/" {
        let mut top: Vec<String> = Vec::new();
        for (d, _) in cache.entries.iter() {
            let seg = d.trim_start_matches('/');
            let first = match seg.split('/').next() {
                Some(s) if !s.is_empty() => s,
                _ => continue,
            };
            let labeled = format!("{}/", first);
            if !top.iter().any(|s| s == &labeled) {
                top.push(labeled);
            }
        }
        return top;
    }
    cache.entries.get(dir).cloned().unwrap_or_default()
}

// ── Pure-logic completion sources ──────────────────────────────────────────

pub fn complete(word: &str, registry: &BuiltinRegistry) -> Vec<String> {
    if word.contains('/') {
        complete_filename(word)
    } else {
        let mut cands = Vec::new();
        cands.extend(complete_builtins(word, registry));
        cands.extend(complete_path_executables(word));
        dedup(&mut cands);
        cands
    }
}

fn complete_builtins(word: &str, registry: &BuiltinRegistry) -> Vec<String> {
    registry.builtins
        .iter()
        .map(|b| b.name())
        .filter(|n| n.starts_with(word))
        .map(|n| n.to_string())
        .collect()
}

fn complete_path_executables(word: &str) -> Vec<String> {
    let path_env = crate::commands::exec::read_path_env();
    let mut cands = Vec::new();
    for dir in path_env.split(':') {
        if dir.is_empty() {
            continue;
        }
        let entries = lookup_cached_dir(dir);
        for name in entries {
            let name = name.trim_end_matches('/');
            if name.starts_with(word) {
                cands.push(name.to_string());
            }
        }
    }
    dedup(&mut cands);
    cands
}

fn complete_filename(word: &str) -> Vec<String> {
    let (dir, prefix) = match word.rfind('/') {
        Some(idx) => (&word[..idx], &word[idx + 1..]),
        None => ("", word),
    };
    let dir = if dir.is_empty() { "/" } else { dir };

    let entries = lookup_cached_dir(dir);
    let mut cands = Vec::new();
    for name in entries {
        if name.starts_with(prefix) {
            let full = if word.starts_with('/') {
                format!("{}/{}", dir.trim_end_matches('/'), name)
            } else {
                format!("{}/{}", dir, name)
            };
            cands.push(full);
        }
    }
    cands
}

pub fn longest_common_prefix(cands: &[String]) -> String {
    if cands.is_empty() {
        return String::new();
    }
    let first = cands[0].as_bytes();
    let mut len = first.len();
    for c in &cands[1..] {
        let b = c.as_bytes();
        len = len.min(b.len());
        let mut i = 0;
        while i < len && first[i] == b[i] {
            i += 1;
        }
        len = i;
        if len == 0 {
            break;
        }
    }
    while len > 0 && first[len - 1] >= 0x80 && first[len - 1] < 0xC0 {
        len -= 1;
    }
    String::from_utf8_lossy(&first[..len]).into_owned()
}

fn dedup(cands: &mut Vec<String>) {
    cands.sort();
    cands.dedup();
}

// ── Completion query handler (called from main loop) ───────────────────────

pub fn handle_completion_query(
    msg: &Message,
    payload: &[u8],
    registry: &'static BuiltinRegistry,
) {
    let req: CompleteRequest = match postcard::from_bytes(payload) {
        Ok(r) => r,
        Err(_) => {
            let _ = debug_print("completion: deserialize request failed");
            return;
        }
    };

    let candidates = complete(&req.word, registry);
    let mut common_prefix = longest_common_prefix(&candidates);
    if !common_prefix.is_empty() && common_prefix.starts_with(req.word.as_str()) {
        common_prefix = common_prefix[req.word.len()..].to_string();
    }

    let reply = CompleteReply {
        candidates,
        common_prefix,
    };
    let reply_payload = match postcard::to_allocvec(&reply) {
        Ok(p) => p,
        Err(_) => return,
    };

    let reply_msg = ipc::make_payload_message(
        SHELL_COMPLETE_QUERY_LABEL,
        reply_payload.len(),
        &[ABI_VERSION as usize],
    );
    let reply_token = match ipc::extract_reply_id(msg) {
        Some(t) => t,
        None => return,
    };
    let _ = ipc::reply_with_payload(reply_token, &reply_msg, &reply_payload);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lcp_empty() {
        assert_eq!(longest_common_prefix(&[]), "");
    }

    #[test]
    fn lcp_single() {
        let v = vec!["foobar".to_string()];
        assert_eq!(longest_common_prefix(&v), "foobar");
    }

    #[test]
    fn lcp_multi() {
        let v = vec!["echo".to_string(), "exit".to_string(), "env".to_string()];
        assert_eq!(longest_common_prefix(&v), "e");
    }

    #[test]
    fn lcp_no_common() {
        let v = vec!["foo".to_string(), "bar".to_string()];
        assert_eq!(longest_common_prefix(&v), "");
    }
}
