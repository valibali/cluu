//! TTY runtime context and registry wiring.
//!
//! This module owns endpoint creation, registry subscription state, and
//! buffered output so the main loop can focus on routing and discipline.

extern crate alloc;

use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, PARAM_TTY_INSTANCE, TOKEN_PROC_CAP};
use libcluu::ipc::{
    call_with_payload, send_with_retry_timeout, CONSOLE_WRITE_LABEL, CONSOLE_WRITE_SYNC_LABEL,
    IPC_CHUNK_BYTES_DEFAULT, IPC_SEND_RETRIES_DEFAULT,
};
use libcluu::registry;
use libcluu::types::Message;
use libcluu::{debug_print, yield_cpu, Result};

// Token indices (set by init).
const SVC_TOKEN_LISTEN: usize = 7;

/// TTY context shared by the main loop.
pub struct TtyContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    pub console_endpoint: usize,
    pub shell_stdin: usize,
    requested_console: bool,
    requested_shell: bool,
    pending_console_output: Vec<u8>,
    /// Deferred sync write reply token (if console wasn't ready)
    pending_sync_reply: Option<usize>,
    console_credit: usize,
}

impl TtyContext {
    /// Initialize registry wiring and select the listen endpoint.
    pub fn new() -> Result<Self> {
        let info = process_info();
        let proc_cap = info.tokens[TOKEN_PROC_CAP];
        // Prefer a fresh endpoint created from proc_cap so tty can grant send-only
        // tokens to subscribers via the registry.
        let endpoint = if proc_cap != 0 {
            match libcluu::syscall::endpoint_create(proc_cap) {
                Ok(token) => token,
                Err(_) => info.tokens[SVC_TOKEN_LISTEN],
            }
        } else {
            info.tokens[SVC_TOKEN_LISTEN]
        };

        let instance_id = info.params[PARAM_TTY_INSTANCE] as u64;
        let service_name = format!("tty:{}", instance_id);
        registry::init(&service_name)?;
        registry::register_default_outputs()?;
        // Expose the tty input for kbd/shell subscriptions.
        registry::register_output("main", endpoint)?;
        let registry_endpoint = registry::control_endpoint();

        debug_print(&format!(
            "tty: endpoint {} registry {}",
            endpoint, registry_endpoint
        ))?;
        debug_print("tty: ready")?;
        yield_cpu()?;

        Ok(Self {
            endpoint,
            registry_endpoint,
            console_endpoint: 0,
            shell_stdin: 0,
            requested_console: false,
            requested_shell: false,
            pending_console_output: Vec::new(),
            pending_sync_reply: None,
            console_credit: CONSOLE_CREDIT_WINDOW,
        })
    }

    /// Request console and shell subscriptions if they are missing.
    pub fn request_subscriptions(&mut self) {
        if self.console_endpoint == 0
            && !self.requested_console
            && registry::request_subscription("console:0", "write").is_ok()
        {
            self.requested_console = true;
        }
        if self.shell_stdin == 0
            && !self.requested_shell
            && registry::request_subscription("shell", "stdin").is_ok()
        {
            self.requested_shell = true;
        }
    }

    /// Handle registry control traffic and update subscriptions.
    pub fn handle_registry_event(&mut self, msg: &libcluu::types::Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { name, token } => {
                    if name == "write" {
                        self.console_endpoint = token;
                        let _ = debug_print("tty: console subscribed");
                        self.flush_pending_console();
                    } else if name == "stdin" {
                        self.shell_stdin = token;
                        let _ = debug_print("tty: shell stdin subscribed");
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_console = false;
                        self.requested_shell = false;
                    }
                }
            }
        }
    }

    /// Forward output to the console or buffer it until the console is ready.
    pub fn forward_to_console(&mut self, payload: &[u8]) {
        if self.console_endpoint != 0 {
            self.send_to_console(payload);
        } else if self.pending_console_output.len() + payload.len() <= 2048 {
            // Keep a small buffer so early shell output is not lost.
            self.pending_console_output.extend_from_slice(payload);
        }
    }

    /// Forward output for sync write. Returns true if output was sent to console,
    /// false if it was buffered (caller should defer reply).
    pub fn forward_to_console_sync(&mut self, payload: &[u8], reply_token: usize) -> bool {
        if self.console_endpoint != 0 {
            // Use sync write to console so we wait for it to render
            self.send_to_console_sync(payload);
            true
        } else {
            // Buffer the output and defer the reply
            if self.pending_console_output.len() + payload.len() <= 2048 {
                self.pending_console_output.extend_from_slice(payload);
            }
            self.pending_sync_reply = Some(reply_token);
            false
        }
    }

    /// Flush any pending console output once the console is subscribed.
    /// Also sends any deferred sync write reply.
    pub fn flush_pending_console(&mut self) {
        if self.console_endpoint == 0 {
            return;
        }
        if !self.pending_console_output.is_empty() {
            let pending = core::mem::take(&mut self.pending_console_output);
            // Use sync write for pending output that has a deferred reply
            if self.pending_sync_reply.is_some() {
                self.send_to_console_sync(&pending);
            } else {
                self.send_to_console(&pending);
            }
        }

        // Send deferred sync reply now that console is ready
        if let Some(reply_token) = self.pending_sync_reply.take() {
            use libcluu::ipc::{reply, TTY_WRITE_SYNC_LABEL};
            use libcluu::types::{IpcFlags, Message};
            let reply_msg = Message::new(TTY_WRITE_SYNC_LABEL, [0; 6], 0);
            let _ = reply(reply_token, &reply_msg, IpcFlags::empty());
        }
    }

    fn send_to_console(&mut self, payload: &[u8]) {
        for chunk in payload.chunks(CONSOLE_MAX_PAYLOAD) {
            if self.console_credit < chunk.len() {
                self.send_to_console_sync(chunk);
                self.console_credit = CONSOLE_CREDIT_WINDOW.saturating_sub(chunk.len());
                continue;
            }
            let _ = send_with_retry_timeout(
                self.console_endpoint,
                CONSOLE_WRITE_LABEL,
                chunk,
                CONSOLE_SEND_RETRIES,
            );
            self.console_credit = self.console_credit.saturating_sub(chunk.len());
        }
    }

    fn send_to_console_sync(&mut self, payload: &[u8]) {
        for chunk in payload.chunks(CONSOLE_MAX_PAYLOAD) {
            let mut msg = Message::new(CONSOLE_WRITE_SYNC_LABEL, [0; 6], 1);
            msg.words[0] = chunk.len();
            let mut reply_msg = Message::new(0, [0; 6], 0);
            // Use ipc_call to wait for console to render
            if call_with_payload(self.console_endpoint, &msg, chunk, &mut reply_msg).is_err() {
                // Fall back to async
                let _ = send_with_retry_timeout(
                    self.console_endpoint,
                    CONSOLE_WRITE_LABEL,
                    chunk,
                    CONSOLE_SEND_RETRIES,
                );
            }
        }
    }
}
const CONSOLE_MAX_PAYLOAD: usize = IPC_CHUNK_BYTES_DEFAULT;
const CONSOLE_CREDIT_WINDOW: usize = IPC_CHUNK_BYTES_DEFAULT * 4;
const CONSOLE_SEND_RETRIES: u32 = IPC_SEND_RETRIES_DEFAULT;
