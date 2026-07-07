//! TAB completion candidate computation + stateless thread handler.
//!
//! The shell main loop blocks in POSIX `_read(0)` and cannot react to
//! mid-line TAB. A dedicated raw thread (spawned from main) blocks on
//! `ipc_recv_any` over a registered completion endpoint and answers
//! `SHELL_COMPLETE_QUERY_LABEL` queries from cluuterm.
//!
//! VFS views are per-thread (set by session-procmgr for the main thread
//! only). The completion thread has no VFS view, so it cannot call
//! `vfs.readdir` directly. Instead, the main thread pre-caches directory
//! listings at startup into a static, and the completion thread reads
//! from that cache.
//!
//! Spec: doc/book/terminal.md §7

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::commands::BuiltinRegistry;
use libcluu::types::Message;
use libcluu::{debug_print, ipc, registry, syscall};
use core::sync::atomic::{AtomicBool, Ordering};

use cluu_wire::pts::{
    CompleteReply, CompleteRequest, SHELL_COMPLETE_QUERY_LABEL,
};
use cluu_wire::ABI_VERSION;

// ── Directory cache ─────────────────────────────────────────────────────────

/// Pre-cached directory listing: path → entry names.
/// Populated by the main thread at startup (which has VFS access).
/// The completion thread reads from this without needing its own VFS view.
/// Protected by a simple spinlock via CACHE_READY flag.
static CACHE_READY: AtomicBool = AtomicBool::new(false);
static mut DIR_CACHE: Vec<(String, Vec<String>)> = Vec::new();

/// Directories to pre-cache at startup. Covers the common completion cases.
const CACHED_DIRS: &[&str] = &[
    "/bin",
    "/etc",
    "/dev",
    "/tmp",
    "/var",
    "/var/images",
    "/home",
    "/home/root",
];

/// Called from the main thread (which has VFS access) to populate the cache.
pub fn populate_dir_cache(vfs: &libcluu::fs::client::VfsClient) {
    let mut cache: Vec<(String, Vec<String>)> = Vec::new();
    for dir in CACHED_DIRS {
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
                cache.push((dir.to_string(), names));
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "completion: readdir {} failed: {:?}",
                    dir, e
                ));
            }
        }
    }
    unsafe {
        DIR_CACHE = cache;
    }
    CACHE_READY.store(true, Ordering::Release);
}

fn lookup_cached_dir(dir: &str) -> Vec<String> {
    if !CACHE_READY.load(Ordering::Acquire) {
        return Vec::new();
    }
    if dir == "/" {
        return lookup_root_entries();
    }
    unsafe {
        DIR_CACHE.iter()
            .find(|(d, _)| d == dir)
            .map(|(_, names)| names.clone())
            .unwrap_or_default()
    }
}

fn lookup_root_entries() -> Vec<String> {
    let mut top: Vec<String> = Vec::new();
    unsafe {
        for (d, _) in DIR_CACHE.iter() {
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
    }
    top
}

// ── Pure-logic completion sources ──────────────────────────────────────────

/// Compute completion candidates for `word`.
/// - Word contains '/': filename completion only.
/// - Bare word: builtin names + PATH executables, deduped.
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

// ── Raw thread infrastructure ───────────────────────────────────────────────

static mut COMPLETION_ARG: Option<(usize, &'static BuiltinRegistry)> = None;

pub extern "C" fn completion_thread_entry() -> ! {
    let (entry_ep, registry) = unsafe {
        match COMPLETION_ARG.take() {
            Some(v) => v,
            None => {
                let _ = debug_print("completion: COMPLETION_ARG not set");
                loop {
                    libcluu::yield_cpu();
                }
            }
        }
    };
    completion_thread(entry_ep, registry);
    loop {
        libcluu::yield_cpu();
    }
}

pub fn spawn_completion_thread(entry_ep: usize, registry: &'static BuiltinRegistry) {
    unsafe {
        COMPLETION_ARG = Some((entry_ep, registry));
    }

    let space = libcluu::boot::space_token();

    let (stack_base, stack_top) = match libcluu::posix::pthread::alloc_thread_stack(16) {
        Some(s) => s,
        None => {
            let _ = debug_print("completion: alloc_thread_stack failed");
            return;
        }
    };

    match libcluu::syscall::thread_create(
        space,
        completion_thread_entry as *const () as usize,
        stack_top,
        128,
        0,
    ) {
        Ok(_tid) => {
            let _ = debug_print("completion: thread started");
        }
        Err(e) => {
            let _ = debug_print(&format!("completion: thread_create failed: {:?}", e));
            let _ = libcluu::syscall::space_unmap(space, stack_base, 16);
        }
    }
}

pub fn completion_thread(entry_ep: usize, registry: &'static BuiltinRegistry) {
    let _ = debug_print("completion: entering IPC loop");

    let mut buf = [0u8; 4096];
    let tokens = [entry_ep];
    loop {
        let _ = registry::handle_grant_requests();

        match syscall::ipc_recv_any(&tokens, &mut buf, 50) {
            Ok((_idx, len)) => {
                if let Some((msg, p)) = ipc::parse_message(&buf[..len]) {
                    if msg.tag.label == SHELL_COMPLETE_QUERY_LABEL {
                        handle_completion_query(&msg, p, registry);
                        continue;
                    }
                    let _ = registry::handle_incoming_message(&msg, p);
                }
                continue;
            }
            Err(_) => continue,
        }
    }
}

fn handle_completion_query(
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
