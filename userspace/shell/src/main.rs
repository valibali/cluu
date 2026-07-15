#![no_std]
#![no_main]

extern crate alloc;

#[cfg(feature = "lang-parser")]
mod commands;
mod io;
#[cfg(feature = "lang-parser")]
mod path_lookup;
#[cfg(feature = "lang-parser")]
mod pipeline;
#[cfg(feature = "lang-parser")]
mod shellrc;
#[cfg(feature = "lang-parser")]
mod completion;

use io::{write_stderr, write_stdout};

use alloc::boxed::Box;
use alloc::format;
#[cfg(feature = "lang-parser")]
use alloc::string::String;
#[cfg(feature = "lang-parser")]
use alloc::vec::Vec;
#[cfg(feature = "lang-parser")]
use commands::{BuiltinFactory, BuiltinRegistry, CommandContext, CommandExecutor, ExecResult};
#[cfg(feature = "lang-parser")]
use commands::builtins::jobs::{drain_job_notifications, reap_done_jobs};
use libcluu::boot::{
    process_info, PARAM_ARGC, PARAM_ARGV_OFFSET, TOKEN_STDERR, TOKEN_STDIN,
    TOKEN_STDLOG, TOKEN_STDOUT,
};
use libcluu::registry;
use libcluu::{debug_print, yield_cpu, Result};

extern "C" {
    fn _read(fd: core::ffi::c_int, buf: *mut core::ffi::c_void, count: usize) -> isize;
}

#[cfg(feature = "lang-parser")]
struct StdinRead {
    data: Vec<u8>,
}

#[cfg(feature = "lang-parser")]
static COMPLETION_EP: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

#[cfg(feature = "lang-parser")]
fn announce_completion_ep() {
    use cluu_wire::pts::SHELL_COMPLETION_ANNOUNCE_LABEL;
    use libcluu::ipc;
    use libcluu::types::{IpcFlags, Message};

    let ep = COMPLETION_EP.load(core::sync::atomic::Ordering::Acquire);
    if ep == 0 {
        return;
    }
    let cluuterm_ep = match libcluu::posix::read_env_var("CLUU_CLUUTERM_EP")
        .and_then(|s| s.parse::<usize>().ok())
    {
        Some(e) => e,
        None => {
            let _ = debug_print("shell: CLUU_CLUUTERM_EP not set");
            return;
        }
    };
    let msg = Message::new(
        SHELL_COMPLETION_ANNOUNCE_LABEL,
        [ep, 0, 0, 0, 0, 0],
        0,
    );
    let _ = ipc::send(cluuterm_ep, &msg, IpcFlags::empty());
}

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let info = process_info();
    registry::init("shell")?;
    // No register_default_outputs: shell:{stdin,stdout,stderr,stdlog} are
    // never consumed, and registering them globally collides between
    // concurrent shell sessions (second cluuterm's shell would fail to start).

    // Procmgr seeds fd 0/1/2 via FdInherit at every spawn. Shell unconditionally
    // reads stdin via POSIX read(0). If the assertion ever trips, procmgr
    // failed to wire FdInherit and the child should exit rather than spin.
    let fd0_is_vfs_backed = libcluu::fd_table::FD_TABLE
        .lock()
        .get(0)
        .map(|e| e.remote_fd.is_some())
        .unwrap_or(false);
    if !fd0_is_vfs_backed {
        let _ = debug_print("shell: FATAL fd 0 not VFS-backed; parent FdInherit missing");
        return Err(libcluu::Error::InvalidState);
    }
    let _ = debug_print("shell: stdin path = vfs-backed");

    let _stdin = info.tokens[TOKEN_STDIN];
    let _stderr = info.tokens[TOKEN_STDERR];
    let stdlog = info.tokens[TOKEN_STDLOG];
    // stdout is already connected to the correct tty:N by procmgr.
    let stdout = info.tokens[TOKEN_STDOUT];
    // Best-effort pre-subscribe to procmgr:spawn to avoid first-command timeout.
    let procmgr_spawn = {
        let mut ep = 0usize;
        for _ in 0..20 {
            match registry::subscribe_output("procmgr", "spawn") {
                Ok(token) => { ep = token; break; }
                Err(_) => { let _ = yield_cpu(); }
            }
        }
        ep
    };
    let registry_endpoint = registry::control_endpoint();
    let mut command_context = CommandContext::new();
    command_context.set_procmgr_spawn(procmgr_spawn);

    #[cfg(feature = "lang-parser")]
    let registry: &'static BuiltinRegistry =
        Box::leak(Box::new(BuiltinFactory::new().build()));

    // UE18+UE19: source /etc/shellrc and $HOME/.shellrc before the
    // prompt fires. The registry is built once (above) and shared with
    // the per-line REPL and the completion thread. Sourcing is
    // best-effort: a missing file, a broken line, or even a missing VFS
    // endpoint just logs and moves on so a stale userdisk can't lock
    // anyone out of their shell.
    #[cfg(feature = "lang-parser")]
    {
        match registry::subscribe_output("vfs", "main") {
            Ok(vfs_ep) => match libcluu::fs::client::VfsClient::new_from_registry(vfs_ep) {
                Ok(vfs) => {
                    io::report_err(
                        shellrc::source_file(
                            "/etc/shellrc",
                            stdout,
                            &mut command_context,
                            registry,
                            &vfs,
                        ),
                        "shellrc: /etc/shellrc",
                    );
                    if let Some(home) = shellrc::home_from_env() {
                        let path = format!("{}/.shellrc", home);
                        io::report_err(
                            shellrc::source_file(
                                &path,
                                stdout,
                                &mut command_context,
                                registry,
                                &vfs,
                            ),
                            "shellrc: ~/.shellrc",
                        );
                    } else {
                        let _ = debug_print(
                            "shellrc: HOME unset, skipping ~/.shellrc",
                        );
                    }
                }
                Err(_) => {
                    let _ = debug_print(
                        "shellrc: VfsClient setup failed, skipping rc files",
                    );
                }
            },
            Err(_) => {
                let _ = debug_print(
                    "shellrc: vfs endpoint unavailable, skipping rc files",
                );
            }
        }
    }

    // ── History: load from ~/.cluu_history ────────────────────────────────────
    #[cfg(feature = "lang-parser")]
    {
        crate::commands::builtins::history::load_history(&mut command_context);
    }

    // ── Job control init ──────────────────────────────────────────────────────
    // Set shell_pgid for use by background-pipeline machinery. We do NOT
    // call tty_set_fg here: the shell holds only the TTY *write* token
    // (info.tokens[TOKEN_STDOUT]); TTY_SET_FG_LABEL needs the TTY service
    // control endpoint, which Plan D never plumbed through. The call was
    // silently failing and confusing readers. Background pipelines that
    // need fg switching set it via their own tty_endpoint inside
    // pipeline.rs; the shell-level fg track is unnecessary for now.
    #[cfg(feature = "lang-parser")]
    {
        let session_id = libcluu::posix::read_env_var("CLUU_SESSION_ID")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        // tty_stdout addresses a real TTY-service endpoint (one that speaks
        // TTY_CTL_LABEL / TTY_REGISTER_LABEL). In the cluuterm/pts flow the
        // shell's stdout token points at a VFS-routed pts endpoint that
        // does NOT speak the legacy TTY protocol — sending TTY_CTL there
        // hangs in `call()` waiting for a reply that never comes. The
        // fd0_is_vfs_backed assertion above guarantees we're in that flow,
        // so leave tty_stdout = 0 and let downstream guards skip TTY work.
        command_context.tty_stdout = 0;
        let _ = stdout; // not used as a TTY-service endpoint
        command_context.session_id = session_id;

        if let Ok(ep) = command_context.procmgr_spawn_endpoint() {
            if let Ok(shell_pgid) = libcluu::posix::jobs::pg_create(ep) {
                command_context.shell_pgid = shell_pgid;
            }
        }
    }

    #[cfg(feature = "lang-parser")]
    let mut rt = None;
    #[cfg(feature = "lang-parser")]
    {
        let completion_ep = match libcluu::syscall::endpoint_create(
            info.tokens[libcluu::boot::TOKEN_IPC],
        ) {
            Ok(ep) => ep,
            Err(e) => {
                let _ = debug_print(&format!("shell: endpoint_create failed: {:?}", e));
                0
            }
        };
        if completion_ep != 0 {
            COMPLETION_EP.store(completion_ep, core::sync::atomic::Ordering::Release);
            announce_completion_ep();

            match libcluu::async_runtime::Runtime::new(info.tokens[libcluu::boot::TOKEN_IPC]) {
                Ok(runtime) => {
                    rt = Some(runtime);
                }
                Err(e) => {
                    let _ = debug_print(&format!("shell: async runtime init failed: {:?}", e));
                }
            }
        }
    }

    debug_print("shell: ready")?;
    write_stdout(b"\x1b[2J\x1b[H");
    io::report_err(print_prompt(stdout), "print_prompt");
    #[cfg(feature = "lang-parser")]
    {
        if let Some(startup_cmd) = startup_command_from_process_info() {
            let _ = debug_print(&format!("shell: startup command '{}'", startup_cmd));
            let mut line = startup_cmd;
            line.push('\n');
            io::report_err(
                parse_and_execute_line(stdout, stdlog, &mut command_context, line.as_bytes(), registry),
                "startup command",
            );
            io::report_err(print_prompt(stdout), "print_prompt");
        }
    }

    // ── Main loop: POSIX read(0) ────────────────────────────────────────────
    // Path A: stdin is fd 0, served by either cluuterm (pts) or the tty
    // service (via /dev/ttyN through VFS). Both block our read until at
    // least one byte is available, so no busy-poll is needed.
    //
    // Registry control traffic is drained on demand by the various
    // `registry::subscribe_output` calls inside builtins (each one calls
    // `wait_for_grant` which pops grant messages from our control endpoint).
    // Job-control notifications are drained between commands. With the
    // recv_any-on-stdin loop gone there is no longer a periodic timeout
    // to reap background jobs proactively; that's acceptable for v1 and
    // can be reintroduced via a poll() with timeout once /dev/ttyN poll
    // semantics are wired (see plan 2026-05-14-shell-stdio-posix-unify).
    let _ = registry_endpoint; // silence warning until something needs it again
    let _ = debug_print("shell: entering main loop");

    #[cfg(feature = "lang-parser")]
    {
        if let Some(mut runtime) = rt {
            return run_async_loop(
                &mut runtime,
                stdout,
                stdlog,
                &mut command_context,
                registry,
            );
        }
    }

    // Fallback: blocking read(0) loop (no lang-parser or runtime init failed)
    let mut buf = [0u8; 256];
    loop {
        let n = unsafe { _read(0, buf.as_mut_ptr() as *mut core::ffi::c_void, buf.len()) };
        if n > 0 {
            handle_line_payload(stdout, stdlog, &mut command_context, &buf[..n as usize], registry)?;
        } else if n == 0 {
            return Ok(());
        } else {
            let _ = yield_cpu();
        }
    }
}

#[cfg(feature = "lang-parser")]
fn run_async_loop(
    rt: &mut libcluu::async_runtime::Runtime,
    stdout: usize,
    stdlog: usize,
    command_context: &mut CommandContext,
    registry: &'static BuiltinRegistry,
) -> Result<()> {
    use libcluu::fs::client::VfsClient;
    use libcluu::ipc;
    use libcluu::syscall::ipc_recv_any_with_sender;

    let completion_ep = COMPLETION_EP.load(core::sync::atomic::Ordering::Acquire);
    let reply_ep = rt.reply_endpoint();

    let vfs_ep = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(_) => {
            let _ = debug_print("shell: vfs endpoint unavailable, no completion");
            0
        }
    };

    if vfs_ep != 0 {
        if let Ok(vfs) = VfsClient::new_from_registry(vfs_ep) {
            completion::set_vfs_client(vfs);
        }
    }

    let grant_base = match libcluu::posix::ensure_grant_buffer() {
        Some(base) => base,
        None => {
            let _ = debug_print("shell: grant buffer init failed, falling back to blocking read");
            let mut buf = [0u8; 256];
            loop {
                let n = unsafe { _read(0, buf.as_mut_ptr() as *mut core::ffi::c_void, buf.len()) };
                if n > 0 {
                    handle_line_payload(stdout, stdlog, command_context, &buf[..n as usize], registry)?;
                } else if n == 0 {
                    return Ok(());
                } else {
                    let _ = yield_cpu();
                }
            }
        }
    };

    let space_token = libcluu::boot::space_token();
    let stdin_fd_entry = {
        let table = libcluu::fd_table::FD_TABLE.lock();
        table.get(0).cloned()
    };
    let stdin_entry = match stdin_fd_entry {
        Some(e) => e,
        None => {
            let _ = debug_print("shell: fd 0 missing");
            return Err(libcluu::Error::InvalidState);
        }
    };
    let stdin_remote_fd = match stdin_entry.remote_fd {
        Some(fd) => fd,
        None => {
            let _ = debug_print("shell: fd 0 not VFS-backed");
            return Err(libcluu::Error::InvalidState);
        }
    };
    let stdin_vfs = VfsClient::new(stdin_entry.endpoint, stdin_entry.client_id);
    let stdin_file = libcluu::fs::client::VfsFile { fd: stdin_remote_fd, size: 0 };

    rt.spawn(async move {
        let mut offset = 0usize;
        loop {
            let fut = stdin_vfs.read_grant_async(stdin_file, offset, 256, space_token, grant_base);
            match fut.await {
                Ok((reply, _)) => {
                    let grant = match VfsClient::parse_read_grant_async_reply(&reply, grant_base) {
                        Ok(g) => g,
                        Err(_) => {
                            libcluu::async_runtime::push_completion(StdinRead { data: Vec::new() });
                            break;
                        }
                    };
                    if grant.len == 0 {
                        libcluu::async_runtime::push_completion(StdinRead { data: Vec::new() });
                        break;
                    }
                    let data = unsafe {
                        core::slice::from_raw_parts(
                            (grant.base + grant.offset) as *const u8,
                            grant.len,
                        )
                    }.to_vec();
                    offset += grant.len;
                    libcluu::async_runtime::push_completion(StdinRead { data });
                }
                Err(_) => {
                    libcluu::async_runtime::push_completion(StdinRead { data: Vec::new() });
                    break;
                }
            }
        }
    });

    let mut buf = [0u8; 4096];
    loop {
        rt.poll_ready();

        while let Some(comp) = rt.pop_completion() {
            if let Ok(stdin_read) = comp.downcast::<StdinRead>() {
                if stdin_read.data.is_empty() {
                    let _ = debug_print("shell: stdin EOF, exiting");
                    return Ok(());
                }
                handle_line_payload(stdout, stdlog, command_context, &stdin_read.data, registry)?;
                drain_job_notifications(command_context);
                reap_done_jobs(stdout, command_context);
            }
        }

        let tokens = [completion_ep, reply_ep];
        match ipc_recv_any_with_sender(&tokens, &mut buf, 50) {
            Ok((0, len, _)) => {
                if let Some((msg, payload)) = ipc::parse_message(&buf[..len]) {
                    if msg.tag.label == cluu_wire::pts::SHELL_COMPLETE_QUERY_LABEL {
                        completion::handle_completion_query(&msg, payload, registry);
                    }
                }
            }
            Ok((1, len, _)) => {
                if let Some((msg, _)) = ipc::parse_message(&buf[..len]) {
                    let cookie = msg.words[5];
                    let payload_start = core::mem::size_of::<libcluu::types::Message>();
                    let payload_bytes: Vec<u8> = if len > payload_start {
                        buf[payload_start..len].to_vec()
                    } else {
                        Vec::new()
                    };
                    rt.deliver_reply(cookie, msg, payload_bytes);
                }
            }
            Ok(_) => {}
            Err(libcluu::Error::Timeout) | Err(libcluu::Error::WouldBlock) => {
                let _ = libcluu::yield_cpu();
            }
            Err(e) => return Err(e),
        }
    }
}

fn print_prompt(_endpoint: usize) -> Result<()> {
    #[cfg(feature = "lang-parser")]
    announce_completion_ep();

    let user = libcluu::posix::read_env_var("USER").unwrap_or_else(|| String::from("cluu"));
    let cwd = libcluu::posix::current_dir_string();
    let prompt = format!("{}:{}> ", user, cwd);
    crate::write_stdout(prompt.as_bytes());
    Ok(())
}

#[cfg(feature = "lang-parser")]
fn startup_command_from_process_info() -> Option<String> {
    let info = process_info();
    let argc = info.params[PARAM_ARGC] as usize;
    let argv_offset = info.params[PARAM_ARGV_OFFSET] as usize;
    if argc <= 1 || argv_offset == 0 {
        return None;
    }

    let info_page_base = libcluu::boot::PROCESS_INFO_ADDR & !0xfff;
    let page = unsafe { core::slice::from_raw_parts(info_page_base as *const u8, 4096) };
    if argv_offset >= page.len() {
        return None;
    }
    let mut cursor = argv_offset;
    let mut argv = Vec::new();
    for _ in 0..argc {
        if cursor >= page.len() {
            break;
        }
        let start = cursor;
        while cursor < page.len() && page[cursor] != 0 {
            cursor += 1;
        }
        if cursor > start {
            if let Ok(text) = core::str::from_utf8(&page[start..cursor]) {
                argv.push(text);
            }
        }
        if cursor < page.len() {
            cursor += 1;
        }
    }
    if argv.len() <= 1 {
        return None;
    }
    let mut cmd = String::new();
    for (idx, arg) in argv.iter().enumerate().skip(1) {
        if idx > 1 {
            cmd.push(' ');
        }
        cmd.push_str(arg);
    }
    if cmd.is_empty() {
        None
    } else {
        Some(cmd)
    }
}

#[allow(dead_code)]
fn print_banner(_tty_endpoint: usize) -> Result<()> {
    // ASCII-only banner, stored in a separate file for easy editing.
    const BANNER: &str = include_str!("banner.txt");
    // Send per line to avoid splitting UTF-8 sequences across IPC messages.
    for line in BANNER.lines() {
        crate::write_stdout(line.as_bytes());
        crate::write_stdout(b"\n");
    }
    crate::write_stdout(b"\n");
    Ok(())
}

/// Update the prompt when a complete line is received from tty.
///
/// The tty sends line-buffered input; we only need to react to newline markers.
fn handle_line_payload(
    stdout: usize,
    stdlog: usize,
    context: &mut CommandContext,
    payload: &[u8],
    registry: &'static BuiltinRegistry,
) -> Result<()> {
    let _ = debug_print(&format!("shell: read {} bytes from fd 0", payload.len()));
    #[cfg(not(feature = "lang-parser"))]
    let _ = (stdlog, context);
    // Ctrl-C in canonical mode is delivered as a single 0x03 marker byte.
    if payload.contains(&0x03) {
        print_prompt(stdout)?;
        return Ok(());
    }

    // Print a new prompt after each completed line.
    if payload.contains(&b'\n') {
        #[cfg(feature = "lang-parser")]
        {
            // Drain any pending job notifications from background jobs before
            // executing the next command, so state is fresh.
            crate::commands::builtins::jobs::drain_job_notifications(context);
            // Print and remove any newly-done background jobs.
            crate::commands::builtins::jobs::reap_done_jobs(stdout, context);

            parse_and_execute_line(stdout, stdlog, context, payload, registry)?;

            // Drain again after the command completes (catches fast bg exits).
            crate::commands::builtins::jobs::drain_job_notifications(context);
            crate::commands::builtins::jobs::reap_done_jobs(stdout, context);
        }
        print_prompt(stdout)?;
    }
    Ok(())
}

#[cfg(feature = "lang-parser")]
fn parse_and_execute_line(
    stdout: usize,
    _stdlog: usize,
    context: &mut CommandContext,
    payload: &[u8],
    registry: &'static BuiltinRegistry,
) -> Result<()> {
    let line = strip_trailing_newline(payload);
    match core::str::from_utf8(line) {
        Ok(text) => {
            if text.trim().is_empty() {
                return Ok(());
            }
            context.history.push(String::from(text));
            context.cmd_count += 1;
            if context.cmd_count % 10 == 0 {
                crate::commands::builtins::history::save_history(context);
            }
            match cluu_lang::parse_program(text) {
                Ok(ast) => {
                    match registry.execute(stdout, context, &ast) {
                        Ok(ExecResult::Handled) => {}
                        Ok(ExecResult::NotHandled) => {
                            let _ = debug_print("shell: unsupported command");
                            write_stderr(b"shell: unsupported command\n");
                        }
                        Err(err) => {
                            write_stderr(format!("{}\n", err).as_bytes());
                            let _ = debug_print(&format!("shell: builtin error {}", err));
                        }
                    }
                    // Check exit_requested flag set by `exit` builtin.
                    if let Some(code) = context.exit_requested.take() {
                        crate::commands::builtins::history::save_history(context);
                        libcluu::posix::_exit(code);
                    }
                }
                Err(err) => {
                    write_stderr(format!("{}\n", err).as_bytes());
                    let _ = debug_print(&format!("shell: parse error {}", err));
                }
            }
        }
        Err(_) => {
            write_stderr(b"shell: invalid utf-8\n");
            let _ = debug_print("shell: invalid utf-8");
        }
    }
    Ok(())
}

#[cfg(feature = "lang-parser")]
fn strip_trailing_newline(payload: &[u8]) -> &[u8] {
    let mut end = payload.len();
    if end > 0 && payload[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && payload[end - 1] == b'\r' {
        end -= 1;
    }
    &payload[..end]
}
