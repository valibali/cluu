//! TTY runtime context and registry wiring.
//!
//! This module owns endpoint creation, registry subscription state, and
//! buffered output so the main loop can focus on routing and discipline.

extern crate alloc;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, PARAM_TTY_INSTANCE, TOKEN_EXTRA_0, TOKEN_IPC};
use libcluu::ipc::{
    send_with_retry_timeout, CONSOLE_WRITE_LABEL, IPC_CHUNK_BYTES_DEFAULT,
    IPC_SEND_RETRIES_DEFAULT, TTY_FG_FLAG_FORWARD_CTRL_C, TTY_FG_FLAG_NOTIFY_CTRL_C,
};
use libcluu::registry;
use libcluu::types::Message;
use libcluu::{debug_print, yield_cpu, Result};

#[derive(Clone, Copy, PartialEq)]
pub enum LoginState {
    Username,
    Password,
    Authenticating,
}

#[derive(Clone, Copy, PartialEq)]
pub enum TtyMode {
    Login(LoginState),
    Terminal,
}

/// A pending read request from a process that called read(0, ...).
pub struct PendingRead {
    pub reply_token: usize,
    pub max_bytes: usize,
}

/// TTY context shared by the main loop.
pub struct TtyContext {
    pub endpoint: usize,
    pub registry_endpoint: usize,
    pub console_endpoint: usize,
    /// Current routing target for line-delivery fallback (legacy shell path).
    pub shell_stdin: usize,
    /// Last granted shell stdin endpoint discovered via registry subscription.
    /// Kept separate from `shell_stdin` so foreground handoff can set
    /// `shell_stdin=0` without triggering auto re-binding.
    shell_registered_stdin: usize,
    requested_console: bool,
    /// Instance index (0-3) for this tty, used to subscribe to the matching console.
    instance_id: u64,
    /// procmgr "spawn" endpoint for requesting shell creation.
    procmgr_spawn: usize,
    requested_procmgr: bool,
    /// Whether we've already requested a shell spawn for this VT.
    shell_spawn_requested: bool,
    pending_console_output: Vec<u8>,
    console_credit: usize,
    /// Queued console output waiting for credit refills from the console.
    console_output_queue: Vec<u8>,
    /// True when the output queue hit its cap and had to drop data.
    console_queue_overflow: bool,
    /// Queue of pending read requests waiting for input data.
    pub pending_reads: VecDeque<PendingRead>,
    /// Input bytes queued for pending readers (raw mode or canonical leftovers).
    pub input_queue: VecDeque<u8>,
    /// Optional endpoint notified when Ctrl-C is pressed while a foreground route is active.
    pub ctrl_c_notify: usize,
    /// Whether Ctrl-C should be forwarded to the current foreground input route.
    pub forward_ctrl_c: bool,
    pub mode: TtyMode,
    pub login_username: Vec<u8>,
    pub login_password: Vec<u8>,
}

impl TtyContext {
    /// Initialize registry wiring and select the listen endpoint.
    pub fn new() -> Result<Self> {
        let info = process_info();
        let ipc_cap = info.tokens[TOKEN_IPC];
        // Prefer a fresh endpoint created from TOKEN_IPC so tty can grant send-only
        // tokens to subscribers via the registry.
        let endpoint = if ipc_cap != 0 {
            match libcluu::syscall::endpoint_create(ipc_cap) {
                Ok(token) => token,
                Err(_) => info.tokens[TOKEN_EXTRA_0],
            }
        } else {
            info.tokens[TOKEN_EXTRA_0]
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
            shell_registered_stdin: 0,
            requested_console: false,
            instance_id,
            procmgr_spawn: 0,
            requested_procmgr: false,
            shell_spawn_requested: false,
            pending_console_output: Vec::new(),
            console_credit: CONSOLE_CREDIT_WINDOW,
            console_output_queue: Vec::new(),
            console_queue_overflow: false,
            pending_reads: VecDeque::new(),
            input_queue: VecDeque::new(),
            ctrl_c_notify: 0,
            forward_ctrl_c: true,
            mode: TtyMode::Login(LoginState::Username),
            login_username: Vec::new(),
            login_password: Vec::new(),
        })
    }

    /// Request console and procmgr subscriptions if they are missing.
    pub fn request_subscriptions(&mut self) {
        if self.console_endpoint == 0 && !self.requested_console {
            // All VTs are managed by the single "console:0" service.
            let console_name = "console:0";
            let output_name = format!("vt:{}", self.instance_id);
            if registry::request_subscription(&console_name, &output_name).is_ok() {
                self.requested_console = true;
            }
        }
        // Subscribe to procmgr so we can send login requests.
        if self.procmgr_spawn == 0 && !self.requested_procmgr {
            if registry::request_subscription("procmgr", "spawn").is_ok() {
                self.requested_procmgr = true;
            }
        }
        // Once we have both console and procmgr, show login prompt for this VT.
        self.maybe_show_login_prompt();
    }

    fn maybe_show_login_prompt(&mut self) {
        if self.shell_spawn_requested || self.procmgr_spawn == 0 || self.console_endpoint == 0 {
            return;
        }
        self.shell_spawn_requested = true;
        // If auto-login already wired a shell, don't override Terminal mode.
        if self.mode == TtyMode::Terminal {
            return;
        }
        self.mode = TtyMode::Login(LoginState::Username);
        self.login_username.clear();
        self.login_password.clear();
        let _ = debug_print(&format!("tty:{}: showing login prompt", self.instance_id));
        self.write_to_console(b"\r\nlogin: ");
    }

    /// Handle registry control traffic and update subscriptions.
    pub fn handle_registry_event(&mut self, msg: &libcluu::types::Message, payload: &[u8]) {
        if let Ok(Some(event)) = registry::handle_incoming_message(msg, payload) {
            match event {
                registry::RegistryEvent::Grant { service_name: _, name, token } => {
                    if name.starts_with("vt:") || name == "write" {
                        self.console_endpoint = token;
                        let _ = debug_print("tty: console subscribed");
                        self.flush_pending_console();
                        self.maybe_show_login_prompt();
                    } else if name == "spawn" {
                        self.procmgr_spawn = token;
                        let _ = debug_print("tty: procmgr spawn subscribed");
                        self.maybe_show_login_prompt();
                    }
                }
                registry::RegistryEvent::SubscribeStatus { code } => {
                    if code != 0 {
                        self.requested_console = false;
                    }
                }
            }
        }
    }

    /// Override shell stdin route for foreground handoff.
    ///
    /// Passing `0` disables legacy shell delivery and leaves input queued for
    /// pending readers (foreground process path). Passing a non-zero endpoint
    /// only updates the active route.
    pub fn set_shell_stdin_route(&mut self, endpoint: usize) {
        self.shell_stdin = endpoint;
    }

    /// Configure foreground route and Ctrl-C policy.
    ///
    /// `endpoint` is the active stdin delivery route.
    /// Passing 0 restores the registered shell stdin route.
    /// `ctrl_c_notify` receives an out-of-band Ctrl-C marker when enabled.
    /// `flags` controls whether Ctrl-C is forwarded to the route and/or notified.
    pub fn configure_foreground(&mut self, endpoint: usize, ctrl_c_notify: usize, flags: usize) {
        // Foreground switches define a new input session boundary. Drop stale
        // buffered bytes from the previous foreground owner but preserve
        // pending read waiters — the new foreground process may have already
        // enqueued a read (race between container start and fire-and-forget
        // TTY_REGISTER). Stale reads from dead processes are harmless:
        // try_satisfy_reads() handles reply failures by dropping the waiter
        // without consuming input data.
        self.input_queue.clear();

        if endpoint == 0 {
            if self.shell_registered_stdin != 0 {
                self.set_shell_stdin_route(self.shell_registered_stdin);
            } else {
                self.set_shell_stdin_route(0);
            }
        } else {
            self.set_shell_stdin_route(endpoint);
        }
        self.ctrl_c_notify = if (flags & TTY_FG_FLAG_NOTIFY_CTRL_C) != 0 {
            ctrl_c_notify
        } else {
            0
        };
        self.forward_ctrl_c = (flags & TTY_FG_FLAG_FORWARD_CTRL_C) != 0;
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

    /// Forward output for sync write (now always async — caller replies immediately).
    pub fn forward_to_console_sync(&mut self, payload: &[u8]) {
        self.forward_to_console(payload);
    }

    /// Flush any pending console output once the console is subscribed.
    pub fn flush_pending_console(&mut self) {
        if self.console_endpoint == 0 {
            return;
        }
        if !self.pending_console_output.is_empty() {
            let pending = core::mem::take(&mut self.pending_console_output);
            self.send_to_console(&pending);
        }
    }

    /// Try to satisfy pending read requests from the input queue.
    ///
    /// Drains bytes from `input_queue` into the oldest pending read,
    /// replies via `reply_with_payload`, and removes the satisfied request.
    pub fn try_satisfy_reads(&mut self) {
        while !self.pending_reads.is_empty() && !self.input_queue.is_empty() {
            let pending = match self.pending_reads.front() {
                Some(p) => p,
                None => break,
            };
            let n = pending.max_bytes.min(self.input_queue.len());
            if n == 0 {
                let _ = self.pending_reads.pop_front();
                continue;
            }

            // Keep bytes in the queue until reply succeeds; stale reply tokens
            // must not consume fresh keyboard input.
            let data: Vec<u8> = self.input_queue.iter().take(n).copied().collect();
            let reply_token = pending.reply_token;
            let reply_msg = Message::new(libcluu::ipc::TTY_READ_REQUEST_LABEL, [0; 6], 0);

            match libcluu::ipc::reply_with_payload(reply_token, &reply_msg, &data) {
                Ok(()) => {
                    let _ = self.pending_reads.pop_front();
                    self.input_queue.drain(..n);
                }
                Err(_) => {
                    // Reader likely died before consuming; drop waiter only.
                    let _ = self.pending_reads.pop_front();
                }
            }
        }
    }

    /// Deliver a line to the current shell route, with self-healing fallback.
    pub fn deliver_shell_line(&mut self, line: &[u8]) {
        if self.shell_stdin == 0 {
            return;
        }

        if send_with_retry_timeout(
            self.shell_stdin,
            libcluu::ipc::TTY_READ_LABEL,
            line,
            IPC_SEND_RETRIES_DEFAULT,
        )
        .is_ok()
        {
            return;
        }

        if self.shell_registered_stdin != 0
            && self.shell_registered_stdin != self.shell_stdin
            && send_with_retry_timeout(
                self.shell_registered_stdin,
                libcluu::ipc::TTY_READ_LABEL,
                line,
                IPC_SEND_RETRIES_DEFAULT,
            )
            .is_ok()
        {
            self.shell_stdin = self.shell_registered_stdin;
            let _ = debug_print("tty: repaired shell stdin route");
        }
    }

    fn send_to_console(&mut self, payload: &[u8]) {
        for chunk in payload.chunks(CONSOLE_MAX_PAYLOAD) {
            if self.console_credit < chunk.len() {
                // Credits exhausted — queue instead of blocking.
                self.enqueue_console_output(chunk);
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

    /// Append data to the output queue, dropping oldest on overflow.
    fn enqueue_console_output(&mut self, data: &[u8]) {
        let new_len = self.console_output_queue.len() + data.len();
        if new_len > CONSOLE_OUTPUT_QUEUE_CAP {
            // Drop oldest bytes to make room.
            let excess = new_len - CONSOLE_OUTPUT_QUEUE_CAP;
            if excess >= self.console_output_queue.len() {
                self.console_output_queue.clear();
            } else {
                self.console_output_queue.drain(..excess);
            }
            if !self.console_queue_overflow {
                self.console_queue_overflow = true;
                let _ = debug_print("tty: console output queue overflow, dropping oldest");
            }
        }
        self.console_output_queue.extend_from_slice(data);
    }

    /// Drain queued output when credits are available.
    pub fn drain_console_queue(&mut self) {
        if self.console_endpoint == 0 || self.console_output_queue.is_empty() {
            return;
        }
        while !self.console_output_queue.is_empty() && self.console_credit > 0 {
            let chunk_len = self.console_output_queue.len().min(CONSOLE_MAX_PAYLOAD).min(self.console_credit);
            if chunk_len == 0 {
                break;
            }
            let chunk: Vec<u8> = self.console_output_queue.drain(..chunk_len).collect();
            let _ = send_with_retry_timeout(
                self.console_endpoint,
                CONSOLE_WRITE_LABEL,
                &chunk,
                CONSOLE_SEND_RETRIES,
            );
            self.console_credit = self.console_credit.saturating_sub(chunk_len);
        }
        if self.console_output_queue.is_empty() {
            self.console_queue_overflow = false;
        }
    }

    /// Handle a credit refill notification from the console.
    pub fn handle_credit_refill(&mut self, refill_amount: usize) {
        self.console_credit = self.console_credit.saturating_add(refill_amount)
            .min(CONSOLE_CREDIT_WINDOW);
        self.drain_console_queue();
    }

    pub fn write_to_console(&mut self, data: &[u8]) {
        self.forward_to_console(data);
    }

    pub fn send_login_request(&mut self) {
        if self.procmgr_spawn == 0 { return; }
        let mut payload = Vec::new();
        payload.extend_from_slice(&self.login_username);
        payload.push(0);
        payload.extend_from_slice(&self.login_password);
        payload.push(0);
        // Zero password buffer
        for b in self.login_password.iter_mut() { *b = 0; }
        self.login_password.clear();

        let msg = libcluu::types::Message::new(
            libcluu::ipc::PROCMGR_SESSION_LOGIN_LABEL,
            [self.instance_id as usize, 0, 0, 0, 0, 0],
            1,
        );
        self.mode = TtyMode::Login(LoginState::Authenticating);
        let _ = debug_print(&format!("tty:{}: login request sent", self.instance_id));

        // Use call semantics so procmgr can reply with success/failure + shell stdin.
        let mut reply_msg = Message::new(0, [0; 6], 0);
        match libcluu::ipc::call_with_payload(self.procmgr_spawn, &msg, &payload, &mut reply_msg) {
            Ok(()) => {
                if reply_msg.words[0] != 0 {
                    let _ = debug_print(&format!(
                        "tty:{}: login failed (err={})", self.instance_id, reply_msg.words[0]
                    ));
                    self.mode = TtyMode::Login(LoginState::Username);
                    self.login_username.clear();
                    self.write_to_console(b"Login incorrect\r\nlogin: ");
                } else {
                    // Login succeeded — wire shell stdin from reply and enter Terminal mode.
                    let stdin_ep = reply_msg.words[2];
                    if stdin_ep != 0 {
                        self.shell_registered_stdin = stdin_ep;
                        self.shell_stdin = stdin_ep;
                    }
                    self.mode = TtyMode::Terminal;
                    let _ = debug_print(&format!(
                        "tty:{}: login success, terminal mode", self.instance_id
                    ));
                }
            }
            Err(_) => {
                self.mode = TtyMode::Login(LoginState::Username);
                self.login_username.clear();
                self.write_to_console(b"\r\nlogin: ");
            }
        }
    }

    /// Wire shell stdin directly (called when procmgr sends TTY_REGISTER for auto-login).
    pub fn wire_shell_stdin(&mut self, endpoint: usize) {
        self.shell_registered_stdin = endpoint;
        self.shell_stdin = endpoint;
        if self.mode != TtyMode::Terminal {
            self.mode = TtyMode::Terminal;
            self.shell_spawn_requested = true;
            let _ = debug_print(&format!(
                "tty:{}: auto-login wired, terminal mode", self.instance_id
            ));
        }
    }

    pub fn handle_session_death(&mut self) {
        let _ = debug_print(&format!("tty:{}: session died, returning to login", self.instance_id));
        self.shell_stdin = 0;
        self.shell_registered_stdin = 0;
        self.shell_spawn_requested = false;
        self.mode = TtyMode::Login(LoginState::Username);
        self.login_username.clear();
        self.login_password.clear();
        self.write_to_console(b"\r\nlogin: ");
    }
}
const CONSOLE_MAX_PAYLOAD: usize = IPC_CHUNK_BYTES_DEFAULT;
const CONSOLE_CREDIT_WINDOW: usize = IPC_CHUNK_BYTES_DEFAULT * 4;
const CONSOLE_SEND_RETRIES: u32 = IPC_SEND_RETRIES_DEFAULT;
/// Maximum console output queue size (16 KB). Oldest bytes are dropped on overflow.
const CONSOLE_OUTPUT_QUEUE_CAP: usize = 16 * 1024;
