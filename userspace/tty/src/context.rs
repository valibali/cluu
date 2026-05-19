//! TTY runtime context and registry wiring.
//!
//! This module owns endpoint creation, registry subscription state, and
//! buffered output so the main loop can focus on routing and discipline.

extern crate alloc;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::vec::Vec;
use libcluu::boot::{process_info, PARAM_TTY_INSTANCE, TOKEN_EXTRA_0, TOKEN_IPC};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    send_with_retry_timeout, CONSOLE_WRITE_LABEL, IPC_CHUNK_BYTES_DEFAULT,
    IPC_SEND_RETRIES_DEFAULT,
};
use libcluu::registry;
use libcluu::types::Message;
use libcluu::{debug_print, yield_cpu, Result};
use cluu_wire::session::{ProfileSpec, SessionCreateRequest};
use cluu_wire::ViewSource;

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
    requested_console: bool,
    /// Instance index (0-3) for this tty, used to subscribe to the matching console.
    instance_id: u64,
    /// procmgr "spawn" endpoint for requesting shell creation.
    procmgr_spawn: usize,
    requested_procmgr: bool,
    /// Whether we've already shown/suppressed the login prompt for this VT.
    shell_spawn_requested: bool,
    /// VT:0 at boot expects auto-login via TTY_REGISTER from procmgr.
    /// Suppresses login prompt until auto-login arrives or session dies.
    auto_login_pending: bool,
    pending_console_output: Vec<u8>,
    console_credit: usize,
    /// Queue of pending read requests waiting for input data.
    pub pending_reads: VecDeque<PendingRead>,
    /// Input bytes queued for pending readers (raw mode or canonical leftovers).
    pub input_queue: VecDeque<u8>,
    pub mode: TtyMode,
    pub login_username: Vec<u8>,
    pub login_password: Vec<u8>,
    /// Lazily-initialized VFS client for tab completion path resolution.
    #[allow(dead_code)]
    vfs_client: Option<VfsClient>,
    /// Foreground pgid per session (session = VT instance_id).
    pub fg_pgid_per_session: BTreeMap<usize, usize>,
    /// Procmgr main endpoint for sending PROCMGR_PG_SIGNAL (obtained via registry).
    pub procmgr_main: usize,
    requested_procmgr_main: bool,
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
            requested_console: false,
            instance_id,
            procmgr_spawn: 0,
            requested_procmgr: false,
            shell_spawn_requested: false,
            auto_login_pending: instance_id == 0 && libcluu::build_env::HARNESS_AUTOLOGIN_ARMED,
            pending_console_output: Vec::new(),
            console_credit: CONSOLE_CREDIT_WINDOW,
            pending_reads: VecDeque::new(),
            input_queue: VecDeque::new(),
            mode: TtyMode::Login(LoginState::Username),
            login_username: Vec::new(),
            login_password: Vec::new(),
            vfs_client: None,
            fg_pgid_per_session: BTreeMap::new(),
            procmgr_main: 0,
            requested_procmgr_main: false,
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
        // Subscribe to procmgr main endpoint for PG_SIGNAL (job control).
        if self.procmgr_main == 0 && !self.requested_procmgr_main {
            if registry::request_subscription("procmgr", "main").is_ok() {
                self.requested_procmgr_main = true;
            }
        }
        // Show login prompt once console is available (procmgr not needed for prompt).
        self.maybe_show_login_prompt();
    }

    fn maybe_show_login_prompt(&mut self) {
        if self.shell_spawn_requested || self.console_endpoint == 0 {
            return;
        }
        self.shell_spawn_requested = true;
        // If auto-login already wired a shell, don't override Terminal mode.
        if self.mode == TtyMode::Terminal {
            return;
        }
        // VT:0 at boot expects auto-login from procmgr — don't show a login
        // prompt that would race with the incoming TTY_REGISTER.
        if self.auto_login_pending {
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
                    } else if name == "main" {
                        self.procmgr_main = token;
                        let _ = debug_print("tty: procmgr main subscribed");
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

    /// Configure foreground route (Path A: no-op — input is delivered via POSIX read(0)).
    ///
    /// The TTY_READ_LABEL push path was retired in favour of fd-0-based
    /// delivery.  TTY_REGISTER messages are still accepted for compatibility
    /// (Ctrl-C / SIGTSTP routing etc.) but the stdin-routing side is ignored.
    pub fn configure_foreground(&mut self, _endpoint: usize, _ctrl_c_notify: usize, _flags: usize) {
        // Drop stale buffered bytes from the previous foreground owner.
        self.input_queue.clear();
        let _ = debug_print("tty: TTY_REGISTER_LABEL push-config ignored (Path A)");
    }

    /// Queue output for the console — actual send is deferred to
    /// `flush_pending_console`. Batching multiple keystrokes (or a burst
    /// of program output) into one CONSOLE_WRITE drastically reduces the
    /// per-message render overhead in the console, which would otherwise
    /// turn fast typing into a multi-second backlog.
    ///
    /// Pre-subscribe (console_endpoint == 0): cap buffering at 2KiB to
    /// avoid runaway growth before console is wired.
    pub fn forward_to_console(&mut self, payload: &[u8]) {
        if self.console_endpoint == 0 {
            if self.pending_console_output.len() + payload.len() <= 2048 {
                self.pending_console_output.extend_from_slice(payload);
            }
        } else {
            self.pending_console_output.extend_from_slice(payload);
        }
    }

    /// Forward output for sync write (now always async — caller replies immediately).
    pub fn forward_to_console_sync(&mut self, payload: &[u8]) {
        self.forward_to_console(payload);
    }

    /// Flush queued console output as a single CONSOLE_WRITE.
    ///
    /// Called by the main loop after draining all currently-pending events,
    /// so a burst of N keystrokes ends up as one console message instead
    /// of N (which would each trigger a full render pipeline).
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
        if !self.pending_reads.is_empty() && !self.input_queue.is_empty() {
            let _ = debug_print(&format!(
                "tty: try_satisfy_reads pending={} queue_len={}",
                self.pending_reads.len(), self.input_queue.len()
            ));
        }
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
            let reply_msg = Message::new(cluu_wire::pts::PTS_READ_LABEL, [0; 6], 0);

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

    fn send_to_console(&mut self, payload: &[u8]) {
        for chunk in payload.chunks(CONSOLE_MAX_PAYLOAD) {
            if self.console_credit < chunk.len() {
                // Console never sends CONSOLE_CREDIT_REFILL yet — self-replenish
                // so output is not silently queued forever.
                self.console_credit = CONSOLE_CREDIT_WINDOW;
            }
            let result = send_with_retry_timeout(
                self.console_endpoint,
                CONSOLE_WRITE_LABEL,
                chunk,
                CONSOLE_SEND_RETRIES,
            );
            if let Err(e) = result {
                let _ = debug_print(&format!(
                    "tty: send_to_console FAIL after retries len={} err={:?}",
                    chunk.len(),
                    e
                ));
            }
            self.console_credit = self.console_credit.saturating_sub(chunk.len());
        }
    }

    /// Handle a credit refill notification from the console.
    pub fn handle_credit_refill(&mut self, refill_amount: usize) {
        self.console_credit = self.console_credit.saturating_add(refill_amount)
            .min(CONSOLE_CREDIT_WINDOW);
    }

    pub fn write_to_console(&mut self, data: &[u8]) {
        self.forward_to_console(data);
    }

    /// Transitional: send a SESSION_CREATE request to procmgr using the new
    /// cluu_wire session protocol.  Full credential verification and shell
    /// spawning will move into the getty binary (Task 10).  For now we create
    /// a minimal session with a placeholder profile; the VFS-backed session
    /// path continues to work.
    pub fn send_login_request(&mut self) {
        // Clear password buffer immediately so it doesn't outlive this call.
        for b in self.login_password.iter_mut() { *b = 0; }
        self.login_password.clear();

        let user_name: alloc::string::String = core::str::from_utf8(&self.login_username).unwrap_or("").into();
        let _instance = self.instance_id; // tty instance (kept local for future use)

        // Build a minimal ProfileSpec with BootstrapRoot — the full envelope
        // (home, env, umask) will be applied by getty (Task 10).
        let profile = ProfileSpec {
            home: alloc::format!("/home/{}", user_name),
            initial_view: ViewSource::BootstrapRoot,
            env: alloc::vec![
                (alloc::string::String::from("HOME"),
                 alloc::format!("/home/{}", user_name)),
                (alloc::string::String::from("USER"), user_name.clone()),
                (alloc::string::String::from("TERM"),
                 alloc::string::String::from("xterm-256color")),
            ],
            umask: 0o022,
        };
        let req = SessionCreateRequest {
            user_name: user_name.clone(),
            profile,
        };

        self.mode = TtyMode::Login(LoginState::Authenticating);
        self.login_username.clear();

        match libcluu::session::create(req) {
            Ok(_ok) => {
                let _ = debug_print(&format!(
                    "tty:{}: SESSION_CREATE ok session_id={}", self.instance_id, _ok.session_id
                ));
                // Login succeeded — enter Terminal mode (Path A: stdin via fd 0).
                self.mode = TtyMode::Terminal;
                let _ = debug_print(&format!(
                    "tty:{}: login success, terminal mode", self.instance_id
                ));
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "tty:{}: SESSION_CREATE failed {:?}", self.instance_id, e
                ));
                self.mode = TtyMode::Login(LoginState::Username);
                self.write_to_console(b"Login incorrect\r\nlogin: ");
            }
        }
    }

    /// Enter Terminal mode (called on auto-login TTY_REGISTER; Path A — push wiring dropped).
    pub fn enter_terminal_mode(&mut self) {
        self.auto_login_pending = false;
        if self.mode != TtyMode::Terminal {
            self.mode = TtyMode::Terminal;
            self.shell_spawn_requested = true;
            let _ = debug_print(&format!(
                "tty:{}: auto-login, terminal mode (Path A)", self.instance_id
            ));
        }
    }

    pub fn handle_session_death(&mut self) {
        let _ = debug_print(&format!("tty:{}: session died, returning to login", self.instance_id));
        self.auto_login_pending = false;
        // Show login prompt and mark as requested so maybe_show_login_prompt
        // doesn't fire a duplicate on the next loop iteration.
        self.shell_spawn_requested = true;
        self.mode = TtyMode::Login(LoginState::Username);
        self.login_username.clear();
        self.login_password.clear();
        self.write_to_console(b"\r\nlogin: ");
    }

    /// Return the VT instance id, which doubles as the session id for fg-pgid tracking.
    pub fn session_id(&self) -> usize {
        self.instance_id as usize
    }

    /// Return a reference to the VFS client, initializing it on first use.
    ///
    /// Uses registry::subscribe_output which blocks on the registry control
    /// endpoint until VFS grants a token. This is a one-time cost on the first
    /// TAB press; subsequent calls return the cached client immediately.
    /// Returns None if VFS is not available (e.g., still starting up).
    #[allow(dead_code)]
    pub fn vfs_client_lazy(&mut self) -> Option<&VfsClient> {
        if self.vfs_client.is_none() {
            let endpoint = registry::subscribe_output("vfs", "main").ok()?;
            self.vfs_client = VfsClient::new_from_registry(endpoint).ok();
        }
        self.vfs_client.as_ref()
    }
}
const CONSOLE_MAX_PAYLOAD: usize = IPC_CHUNK_BYTES_DEFAULT;
const CONSOLE_CREDIT_WINDOW: usize = IPC_CHUNK_BYTES_DEFAULT * 4;
const CONSOLE_SEND_RETRIES: u32 = IPC_SEND_RETRIES_DEFAULT;
