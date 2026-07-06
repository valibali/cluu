//! Session-procmgr main entry point.
//!
//! At boot, root-procmgr spawns one instance of this binary per authenticated
//! session.  The serialised `SessionEnvelope` is written into the ProcessInfo
//! page via the argv payload slot:
//!
//!   - `params[PARAM_ARGC]`        = envelope byte length (NOT arg count)
//!   - `params[PARAM_ARGV_OFFSET]` = byte offset of envelope within page
//!
//! The binary creates one IPC endpoint, registers it as
//! `session-procmgr:<sid>:spawn` in the registry, then loops on `ipc_recv`
//! dispatching the 7 session-scoped handlers.
#![cfg_attr(not(feature = "host-test"), no_std)]
#![cfg_attr(not(feature = "host-test"), no_main)]
extern crate alloc;

/// Production kernel adapter — compiled only for target (real x86-64 syscalls).
#[cfg(not(feature = "host-test"))]
mod real_kernel;

// Pull in the library crate under a short alias for readability.
#[cfg(not(feature = "host-test"))]
use session_procmgr as spm;

#[cfg(not(feature = "host-test"))]
use core::mem::size_of;

#[cfg(not(feature = "host-test"))]
use libcluu::{
    boot::{process_info, PARAM_ARGC, PARAM_ARGV_OFFSET, PROCESS_INFO_ADDR, TOKEN_SELF, TOKEN_VFS_VIEW_MGR},
    debug_print, registry,
    ipc::{extract_reply_id, parse_message},
    mem::PAGE_SIZE,
    rights::Rights,
    syscall::{endpoint_create, token_derive},
    types::Message,
    Result,
};
#[cfg(not(feature = "host-test"))]
use procmgr_common::{
    handler::{InboundMsg, Reply},
    wire::SessionEnvelope,
};

#[cfg(not(feature = "host-test"))]
use libcluu::async_runtime::{IpcCallFuture, Runtime};

#[cfg(not(feature = "host-test"))]
use alloc::collections::BTreeMap;

#[cfg(not(feature = "host-test"))]
use libcluu::ipc::VFS_DERIVE_CHILD_FD_LABEL;

#[cfg(not(feature = "host-test"))]
use procmgr_common::pid::LOCAL_MAX;

#[cfg(not(feature = "host-test"))]
use procmgr_common::wire::SpawnReply;

#[cfg(not(feature = "host-test"))]
use procmgr_common::kernel_iface::Kernel;

/// Read the `SessionEnvelope` serialised into the ProcessInfo page by
/// root-procmgr.  Returns `None` if the page slots are zero or the
/// deserialisation fails.
#[cfg(not(feature = "host-test"))]
fn read_envelope_from_process_info() -> Option<SessionEnvelope> {
    let info = process_info();
    let byte_len = info.params[PARAM_ARGC] as usize;
    let byte_off = info.params[PARAM_ARGV_OFFSET] as usize;
    if byte_len == 0 || byte_off == 0 || byte_off + byte_len > PAGE_SIZE {
        return None;
    }
    let page_base = PROCESS_INFO_ADDR & !(PAGE_SIZE - 1);
    let ptr = (page_base + byte_off) as *const u8;
    // Safety: page is mapped read-only by root-procmgr before this process
    // starts executing; bounds checked above.
    let raw = unsafe { core::slice::from_raw_parts(ptr, byte_len) };
    postcard::from_bytes(raw).ok()
}

/// Send a reply back to the caller using the reply token from `msg`.
/// Falls back to sending on `fallback_ep` if the message was a one-way send.
#[cfg(not(feature = "host-test"))]
fn send_reply(
    reply_opt: Option<usize>,
    fallback_ep: usize,
    r: &Reply,
    async_reply: Option<(usize, usize)>,
) -> Result<()> {
    if let Some(token) = reply_opt {
        let mut msg = Message::new(r.label, [0; 6], 1);
        if r.payload.is_empty() {
            for (i, &w) in r.words.iter().enumerate().take(6) {
                msg.words[i] = w;
            }
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    &msg as *const Message as *const u8,
                    size_of::<Message>(),
                )
            };
            let _ = libcluu::syscall::ipc_reply(token, bytes);
        } else {
            msg.words[0] = r.payload.len();
            for (i, &w) in r.words.iter().enumerate().take(5) {
                msg.words[i + 1] = w;
            }
            let header = unsafe {
                core::slice::from_raw_parts(
                    &msg as *const Message as *const u8,
                    size_of::<Message>(),
                )
            };
            let mut buf = alloc::vec::Vec::with_capacity(header.len() + r.payload.len());
            buf.extend_from_slice(header);
            buf.extend_from_slice(&r.payload);
            let _ = libcluu::syscall::ipc_reply(token, &buf);
        }
        return Ok(());
    }

    if let Some((reply_ep, cookie)) = async_reply {
        let mut msg = Message::new(r.label, [0; 6], 1);
        msg.words[5] = cookie;
        if r.payload.is_empty() {
            for (i, &w) in r.words.iter().enumerate().take(5) {
                msg.words[i] = w;
            }
            let bytes = unsafe {
                core::slice::from_raw_parts(
                    &msg as *const Message as *const u8,
                    size_of::<Message>(),
                )
            };
            let _ = libcluu::syscall::ipc_send(reply_ep, bytes);
        } else {
            msg.words[0] = r.payload.len();
            for (i, &w) in r.words.iter().enumerate().take(4) {
                msg.words[i + 1] = w;
            }
            let header = unsafe {
                core::slice::from_raw_parts(
                    &msg as *const Message as *const u8,
                    size_of::<Message>(),
                )
            };
            let mut buf = alloc::vec::Vec::with_capacity(header.len() + r.payload.len());
            buf.extend_from_slice(header);
            buf.extend_from_slice(&r.payload);
            let _ = libcluu::syscall::ipc_send(reply_ep, &buf);
        }
        return Ok(());
    }

    let _ = fallback_ep;
    Ok(())
}

#[cfg(not(feature = "host-test"))]
fn run() -> Result<()> {
    let envelope = match read_envelope_from_process_info() {
        Some(e) => e,
        None => {
            let _ = debug_print("session-procmgr: FATAL: no envelope in ProcessInfo");
            loop { let _ = libcluu::syscall::yield_cpu(); }
        }
    };

    let sid = envelope.sid;
    let _ = debug_print(&alloc::format!("session-procmgr: started sid={}", sid));

    // Initialise registry client.
    registry::init("session-procmgr")?;

    // Create our listening endpoint.
    let info = process_info();
    let ipc_cap = info.tokens[libcluu::boot::TOKEN_IPC];
    let ep = endpoint_create(ipc_cap)?;

    // Derive a grantable handle for register_output so that
    // handle_grant_request can call token_derive on it when a subscriber
    // arrives.  The registered token needs IPC_SEND|IPC_CALL|GRANT (no
    // IPC_RECV — subscribers call/send, they do not recv on our endpoint).
    // The recv loop below keeps using `ep` which has IPC_RECV.
    let ep_grantable = token_derive(
        ep,
        (Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT).bits() as usize,
        u64::MAX,
    ).unwrap_or_else(|_| {
        let _ = debug_print("session-procmgr: WARN token_derive grantable FAILED, using raw ep");
        ep
    });

    // Register as "session-procmgr:<sid>:spawn" so root-procmgr and
    // cluuterm can discover us by session id.
    let ep_name = alloc::format!("spawn:{}", sid);
    registry::register_output(&ep_name, ep_grantable)?;

    let main_name = alloc::format!("main:{}", sid);
    registry::register_output(&main_name, ep_grantable)?;

    let _ = debug_print(&alloc::format!(
        "session-procmgr: registered sid={} ep={}",
        sid, ep
    ));

    // Discover system services via registry (vfs is critical for elf_spawn;
    // timeserver/registry handles are session-procmgr's own ProcessInfo tokens).
    let vfs_cap = registry::lookup_service("vfs:main").unwrap_or(0) as u64;
    let session_vfs_cap = {
        let session_vfs_name = alloc::format!("session-vfs:main:{}", sid);
        let mut cap = 0u64;
        for _ in 0..200 {
            if let Some(ep) = registry::lookup_service(&session_vfs_name) {
                cap = ep as u64;
                break;
            }
            let _ = libcluu::syscall::yield_cpu();
        }
        cap
    };
    let registry_cap = info.tokens[libcluu::boot::TOKEN_REGISTRY] as u64;
    let timeserver_cap = info.tokens[libcluu::boot::TOKEN_CLOCK] as u64;
    let view_mgr_token = info.tokens[TOKEN_VFS_VIEW_MGR] as u64;

    let _ = debug_print(&alloc::format!(
        "session-procmgr: sid={} vfs_cap={} reg={} ts={}",
        sid, vfs_cap, registry_cap, timeserver_cap
    ));

    // Build the handler state.
    let mut state = spm::dispatch::SessionState {
        sid,
        generation: envelope.generation,
        child_table: spm::child_table::ChildTable::new(sid),
        kernel: spm::real_kernel::RealKernel,
        vfs_cap,
        session_vfs_cap,
        registry_cap,
        timeserver_cap,
        restart: spm::restart::RestartTracker::new(),
        pipes: spm::pipe_registry::PipeRegistry::new(),
        ctty: None,
        spawn_ep: ep as u64,
        view_mgr_token,
        pg_table: spm::pg_table::PgTable::new(),
    };

    let control_ep = registry::control_endpoint();
    let token_self = info.tokens[TOKEN_SELF];
    let mut runtime = Runtime::new(token_self)?;
    let reply_ep = runtime.reply_endpoint();
    let mut pending_spawns: BTreeMap<u64, spm::elf_spawn::PendingSpawn> = BTreeMap::new();
    let endpoints: [usize; 3] = [ep, control_ep, reply_ep];
    let mut buf = [0u8; 4096];

    let _ = debug_print(&alloc::format!("session-procmgr: recv loop sid={}", sid));

    loop {
        runtime.poll_ready();

        while let Some(comp) = runtime.pop_completion() {
            if let Ok(spm_comp) = comp.downcast::<spm::elf_spawn::SpmCompletion>() {
                handle_spm_completion(
                    &mut state,
                    &mut pending_spawns,
                    ep,
                    *spm_comp,
                );
            }
        }

        let (idx, len, sender_tid) =
            match libcluu::syscall::ipc_recv_any_with_sender(&endpoints, &mut buf, u64::MAX) {
                Ok(res) => res,
                Err(_) => continue,
            };

        if idx == 2 {
            if let Some((msg, _payload)) = parse_message(&buf[..len]) {
                let cookie = msg.words[5];
                let payload_start = size_of::<Message>();
                let payload_bytes: alloc::vec::Vec<u8> = if len > payload_start {
                    buf[payload_start..len].to_vec()
                } else {
                    alloc::vec::Vec::new()
                };
                runtime.deliver_reply(cookie, msg, payload_bytes);
            }
            continue;
        }

        // Registry events on control_ep — ignore for now.
        if idx == 1 {
            if let Some((msg, payload)) = parse_message(&buf[..len]) {
                let _ = registry::handle_incoming_message(&msg, payload);
            }
            continue;
        }

        // Main endpoint: dispatch.
        let (msg, payload) = match parse_message(&buf[..len]) {
            Some(p) => p,
            None => continue,
        };
        let reply_token = extract_reply_id(&msg);
        let async_reply = if reply_token.is_none()
            && msg.tag.extra == libcluu::ipc::ASYNC_REPLY_TAG
        {
            Some((msg.words[4], msg.words[5]))
        } else {
            None
        };

        let inbound = InboundMsg {
            label: msg.tag.label,
            words: {
                let mut w = [0usize; 6];
                for (i, &v) in msg.words.iter().enumerate().take(6) {
                    w[i] = v;
                }
                w
            },
            payload,
            sender_tid,
        };

        let result = spm::dispatch::dispatch(&mut state, &inbound);

        match result {
            Ok(spm::dispatch::DispatchOutcome::Reply(reply)) => {
                let _ = send_reply(reply_token, ep, &reply, async_reply);
            }
            Ok(spm::dispatch::DispatchOutcome::AlreadySent) => {}
            Ok(spm::dispatch::DispatchOutcome::NeedsAsyncSpawn(mut pending)) => {
                pending.reply_token = reply_token;
                pending.async_reply = async_reply;
                let spawn_cookie = pending.cookie;
                let fd_requests: alloc::vec::Vec<spm::elf_spawn::FdDeriveRequest> =
                    pending.fd_derive_requests.iter().copied().collect();
                pending_spawns.insert(spawn_cookie, pending);
                for req in fd_requests {
                    let spawn_cookie = spawn_cookie;
                    let fd = req.fd;
                    let vfs_ep = req.vfs_ep;
                    let mut request = Message::new(
                        VFS_DERIVE_CHILD_FD_LABEL,
                        [req.parent_tid, req.parent_rfd, req.rights, req.child_tid, 0, 0],
                        4,
                    );
                    runtime.spawn(async move {
                        let result = IpcCallFuture::new(vfs_ep, request).await;
                        let completion = match result {
                            Ok((reply, _)) => {
                                if reply.words[0] == 0 {
                                    spm::elf_spawn::SpmCompletion::SpawnDeriveChildFdReply {
                                        cookie: spawn_cookie,
                                        fd,
                                        result: Ok((reply.words[1], reply.words[2], reply.words[3])),
                                    }
                                } else {
                                    spm::elf_spawn::SpmCompletion::SpawnDeriveChildFdReply {
                                        cookie: spawn_cookie,
                                        fd,
                                        result: Err(spm::elf_spawn::RealSpawnError::VfsDeriveChildFd),
                                    }
                                }
                            }
                            Err(_) => {
                                spm::elf_spawn::SpmCompletion::SpawnDeriveChildFdReply {
                                    cookie: spawn_cookie,
                                    fd,
                                    result: Err(spm::elf_spawn::RealSpawnError::VfsDeriveChildFd),
                                }
                            }
                        };
                        libcluu::async_runtime::push_completion(completion);
                    });
                }
            }
            Err(e) => {
                let _ = debug_print(&alloc::format!(
                    "session-procmgr: handler error label=0x{:x} {:?}",
                    msg.tag.label, e
                ));
                if let Some(tok) = reply_token {
                    let err_msg = Message::new(msg.tag.label, [0xFFFF_FFFF; 6], 1);
                    let bytes = unsafe {
                        core::slice::from_raw_parts(
                            &err_msg as *const Message as *const u8,
                            size_of::<Message>(),
                        )
                    };
                    let _ = libcluu::syscall::ipc_reply(tok, bytes);
                } else if let Some((reply_ep, cookie)) = async_reply {
                    let mut err_msg = Message::new(msg.tag.label, [0xFFFF_FFFF; 6], 1);
                    err_msg.words[5] = cookie;
                    let bytes = unsafe {
                        core::slice::from_raw_parts(
                            &err_msg as *const Message as *const u8,
                            size_of::<Message>(),
                        )
                    };
                    let _ = libcluu::syscall::ipc_send(reply_ep, bytes);
                }
            }
        }
    }
}

#[cfg(not(feature = "host-test"))]
fn handle_spm_completion(
    state: &mut spm::dispatch::SessionState,
    pending_spawns: &mut BTreeMap<u64, spm::elf_spawn::PendingSpawn>,
    ep: usize,
    completion: spm::elf_spawn::SpmCompletion,
) {
    match completion {
        spm::elf_spawn::SpmCompletion::SpawnDeriveChildFdReply { cookie, fd, result } => {
            let pending = match pending_spawns.get_mut(&cookie) {
                Some(p) => p,
                None => return,
            };
            if fd < 4 {
                pending.fd_derive_results[fd] = Some(result);
            }
            pending.fd_derive_remaining = pending.fd_derive_remaining.saturating_sub(1);
            if pending.fd_derive_remaining != 0 {
                return;
            }

            let mut pending = match pending_spawns.remove(&cookie) {
                Some(p) => p,
                None => return,
            };
            let pid = pending.pid;
            let parent_pid = pending.parent_pid;
            let reply_token = pending.reply_token;
            let async_reply = pending.async_reply;
            let argv0 = pending.req.argv.first().cloned().unwrap_or_default();
            let notify_ep = pending.req.notify.unwrap_or(0);
            let minted = core::mem::take(&mut pending.minted);

            match spm::elf_spawn::finish_spawn(state, pending) {
                Ok(result) => {
                    let local = (pid as u32) & LOCAL_MAX;
                    state.child_table.insert(spm::child_table::ChildState {
                        pid,
                        local,
                        thread_tok: result.thread_tok,
                        space_tok: result.space_tok,
                        child_tid: result.child_tid,
                        cookie: result.cookie,
                        argv0,
                        start_ticks: 0,
                        minted_caps: minted,
                        pgid: None,
                        notify_ep,
                        parent_pid,
                    });
                    let reply = SpawnReply { pid, cookie: result.cookie };
                    let bytes = postcard::to_allocvec(&reply).unwrap_or_default();
                    let reply = Reply::ok(procmgr_common::labels::SESSION_PROCMGR_SPAWN_LABEL)
                        .with_payload(bytes);
                    let _ = send_reply(reply_token, ep, &reply, async_reply);
                }
                Err(e) => {
                    let _ = debug_print(&alloc::format!(
                        "session-procmgr: finish_spawn failed: {:?}", e
                    ));
                    for h in &minted {
                        state.kernel.revoke(*h);
                    }
                    if let Some(tok) = reply_token {
                        let err_msg = Message::new(
                            procmgr_common::labels::SESSION_PROCMGR_SPAWN_LABEL,
                            [0xFFFF_FFFF; 6],
                            1,
                        );
                        let bytes = unsafe {
                            core::slice::from_raw_parts(
                                &err_msg as *const Message as *const u8,
                                size_of::<Message>(),
                            )
                        };
                        let _ = libcluu::syscall::ipc_reply(tok, bytes);
                    } else if let Some((reply_ep, cookie)) = async_reply {
                        let mut err_msg = Message::new(
                            procmgr_common::labels::SESSION_PROCMGR_SPAWN_LABEL,
                            [0xFFFF_FFFF; 6],
                            1,
                        );
                        err_msg.words[5] = cookie;
                        let bytes = unsafe {
                            core::slice::from_raw_parts(
                                &err_msg as *const Message as *const u8,
                                size_of::<Message>(),
                            )
                        };
                        let _ = libcluu::syscall::ipc_send(reply_ep, bytes);
                    }
                }
            }
        }
    }
}

#[cfg(not(feature = "host-test"))]
#[unsafe(no_mangle)]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

#[cfg(feature = "host-test")]
pub fn main() -> i32 {
    0
}
