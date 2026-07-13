//! TAB completion candidate computation + lazy directory cache.
//!
//! Directory listings are fetched lazily via sync VFS readdir on first
//! TAB press for each directory — no pre-caching, no startup cost.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::commands::BuiltinRegistry;
use libcluu::types::Message;
use libcluu::{debug_print, ipc};

use alloc::collections::BTreeMap;
use spin::Mutex;

use cluu_wire::pts::{
    CompleteReply, CompleteRequest, SHELL_COMPLETE_QUERY_LABEL,
};
use cluu_wire::ABI_VERSION;

static DIR_CACHE: Mutex<BTreeMap<String, Vec<String>>> = Mutex::new(BTreeMap::new());

static VFS_CLIENT: Mutex<Option<libcluu::fs::client::VfsClient>> = Mutex::new(None);

pub fn set_vfs_client(client: libcluu::fs::client::VfsClient) {
    *VFS_CLIENT.lock() = Some(client);
}

fn ensure_dir_cached(dir: &str) {
    if DIR_CACHE.lock().contains_key(dir) {
        return;
    }
    let vfs_ep = VFS_CLIENT.lock().as_ref().map(|v| v.endpoint());
    let vfs_ep = match vfs_ep {
        Some(ep) => ep,
        None => return,
    };
    let vfs = libcluu::fs::client::VfsClient::new(vfs_ep, libcluu::registry::control_endpoint());
    let mut retries = 0u32;
    loop {
        match vfs.readdir(dir) {
            Ok(entries) => {
                let names: Vec<String> = entries.iter()
                    .map(|e| {
                        if e.is_dir {
                            format!("{}/", e.name)
                        } else {
                            e.name.clone()
                        }
                    })
                    .collect();
                let _ = debug_print(&format!(
                    "completion: cached {} entries for {}",
                    names.len(), dir
                ));
                DIR_CACHE.lock().insert(String::from(dir), names);
                return;
            }
            Err(libcluu::Error::Busy) if retries < 3 => {
                retries += 1;
                let _ = libcluu::yield_cpu();
                continue;
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "completion: readdir {} failed: {:?}",
                    dir, e
                ));
                DIR_CACHE.lock().insert(String::from(dir), Vec::new());
                return;
            }
        }
    }
}

fn lookup_cached_dir(dir: &str) -> Vec<String> {
    if dir == "/" {
        let mut top: Vec<String> = Vec::new();
        for d in DIR_CACHE.lock().keys() {
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
    if DIR_CACHE.lock().contains_key(dir) {
        return DIR_CACHE.lock().get(dir).cloned().unwrap_or_default();
    }
    ensure_dir_cached(dir);
    DIR_CACHE.lock().get(dir).cloned().unwrap_or_default()
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
