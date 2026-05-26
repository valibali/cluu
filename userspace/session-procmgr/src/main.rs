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
    boot::{process_info, PARAM_ARGC, PARAM_ARGV_OFFSET, PROCESS_INFO_ADDR},
    debug_print, registry,
    ipc::{extract_reply_id, parse_message},
    mem::PAGE_SIZE,
    syscall::endpoint_create,
    types::Message,
    Result,
};
#[cfg(not(feature = "host-test"))]
use procmgr_common::{
    handler::{InboundMsg, Reply},
    wire::SessionEnvelope,
};

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
fn send_reply(reply_opt: Option<usize>, fallback_ep: usize, r: &Reply) -> Result<()> {
    let token = match reply_opt {
        Some(t) => t,
        None => return Ok(()), // one-way message — no reply expected
    };
    let mut msg = Message::new(r.label, [0; 6], 1);
    msg.words[0] = r.payload.len();
    for (i, &w) in r.words.iter().enumerate().take(5) {
        msg.words[i + 1] = w;
    }
    if r.payload.is_empty() {
        let bytes = unsafe {
            core::slice::from_raw_parts(
                &msg as *const Message as *const u8,
                size_of::<Message>(),
            )
        };
        let _ = libcluu::syscall::ipc_reply(token, bytes);
    } else {
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
    let _ = fallback_ep; // suppress unused warning
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

    // Register as "session-procmgr:<sid>:spawn" so root-procmgr and
    // cluuterm can discover us by session id.
    let ep_name = alloc::format!("spawn:{}", sid);
    registry::register_output(&ep_name, ep)?;

    let _ = debug_print(&alloc::format!(
        "session-procmgr: registered sid={} ep={}",
        sid, ep
    ));

    // Build the handler state.
    // NOTE: SessionState.kernel is typed as MockKernel in dispatch.rs
    // for host-test builds.  In the no_std production binary we keep the
    // same type for now — spawn.rs will forward to root-procmgr via IPC in
    // Phase 12.4 when real kernel wiring lands.
    let mut state = spm::dispatch::SessionState {
        sid,
        generation: envelope.generation,
        child_table: spm::child_table::ChildTable::new(sid),
        kernel: procmgr_common::test_kernel::MockKernel::new(),
        vfs_cap: envelope.caps.iter().find(|(n, _)| n == "vfs").map(|(_, h)| *h).unwrap_or(0),
        registry_cap: envelope.caps.iter().find(|(n, _)| n == "registry").map(|(_, h)| *h).unwrap_or(0),
        timeserver_cap: envelope.caps.iter().find(|(n, _)| n == "timeserver").map(|(_, h)| *h).unwrap_or(0),
        restart: spm::restart::RestartTracker::new(),
        pipes: spm::pipe_registry::PipeRegistry::new(),
        ctty: None,
    };

    let control_ep = registry::control_endpoint();
    let endpoints: [usize; 2] = [ep, control_ep];
    let mut buf = [0u8; 4096];

    let _ = debug_print(&alloc::format!("session-procmgr: recv loop sid={}", sid));

    loop {
        let (idx, len, sender_tid) =
            match libcluu::syscall::ipc_recv_any_with_sender(&endpoints, &mut buf, u64::MAX) {
                Ok(res) => res,
                Err(_) => continue,
            };

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
            Ok(reply) => {
                let _ = send_reply(reply_token, ep, &reply);
            }
            Err(e) => {
                // Log the error and send an error reply if the sender is waiting.
                let _ = debug_print(&alloc::format!(
                    "session-procmgr: handler error label=0x{:x} {:?}",
                    msg.tag.label, e
                ));
                if let Some(tok) = reply_token {
                    // Send a minimal error reply so the caller unblocks.
                    let err_msg = Message::new(msg.tag.label, [0xFFFF_FFFF; 6], 1);
                    let bytes = unsafe {
                        core::slice::from_raw_parts(
                            &err_msg as *const Message as *const u8,
                            size_of::<Message>(),
                        )
                    };
                    let _ = libcluu::syscall::ipc_reply(tok, bytes);
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
