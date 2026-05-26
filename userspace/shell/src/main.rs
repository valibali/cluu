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

use io::write_stdout;

use alloc::format;
#[cfg(feature = "lang-parser")]
use alloc::string::String;
#[cfg(feature = "lang-parser")]
use alloc::string::ToString;
#[cfg(feature = "lang-parser")]
use alloc::vec::Vec;
#[cfg(feature = "lang-parser")]
use commands::{BuiltinFactory, CommandContext, CommandExecutor, ExecResult};
#[cfg(feature = "lang-parser")]
use commands::builtins::jobs::{drain_job_notifications, reap_done_jobs};
use libcluu::boot::{
    process_info, PARAM_ARGC, PARAM_ARGV_OFFSET, PARAM_TTY_INSTANCE, TOKEN_STDERR, TOKEN_STDIN,
    TOKEN_STDLOG, TOKEN_STDOUT,
};
use libcluu::ipc::{send_with_payload, TTY_WRITE_LABEL};
use libcluu::registry;
use libcluu::{debug_print, yield_cpu, Result};

extern "C" {
    fn _read(fd: core::ffi::c_int, buf: *mut core::ffi::c_void, count: usize) -> isize;
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

    // Patch VFS-backed stdio fds: init_stdio() runs before the registry is
    // available so it stores stdin/stdout IPC tokens in FdEntry::endpoint.
    // After registry::init() we can resolve the real VFS endpoint and fix up
    // any fds that carry a remote_fd (those need to send FS_READ_GRANT to VFS,
    // not to the stdin IPC token).
    if let Some(vfs_ep) = registry::lookup_service("vfs:main") {
        libcluu::fd_table::patch_vfs_stdio_endpoints(vfs_ep);
    }

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

    // UE18+UE19: source /etc/shellrc and $HOME/.shellrc before the
    // prompt fires. We build the registry once for sourcing and let
    // the per-line REPL keep its current behavior of rebuilding
    // inline. Sourcing is best-effort: a missing file, a broken line,
    // or even a missing VFS endpoint just logs and moves on so a
    // stale userdisk can't lock anyone out of their shell.
    #[cfg(feature = "lang-parser")]
    {
        match registry::subscribe_output("vfs", "main") {
            Ok(vfs_ep) => match libcluu::fs::client::VfsClient::new_from_registry(vfs_ep) {
                Ok(vfs) => {
                    let factory = BuiltinFactory::new();
                    let rc_registry = factory.build();
                    let _ = shellrc::source_file(
                        "/etc/shellrc",
                        stdout,
                        &mut command_context,
                        &rc_registry,
                        &vfs,
                    );
                    if let Some(home) = shellrc::home_from_env() {
                        let path = format!("{}/.shellrc", home);
                        let _ = shellrc::source_file(
                            &path,
                            stdout,
                            &mut command_context,
                            &rc_registry,
                            &vfs,
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
        let session_id = info.params[PARAM_TTY_INSTANCE] as usize;
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

    debug_print("shell: ready")?;
    write_stdout(b"\x1b[2J\x1b[H");
    let _ = print_prompt(stdout);
    #[cfg(feature = "lang-parser")]
    {
        if let Some(startup_cmd) = startup_command_from_process_info() {
            let _ = debug_print(&format!("shell: startup command '{}'", startup_cmd));
            let mut line = startup_cmd;
            line.push('\n');
            let _ = parse_and_execute_line(stdout, stdlog, &mut command_context, line.as_bytes());
            let _ = print_prompt(stdout);
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
    let mut buf = [0u8; 256];
    let _ = debug_print("shell: entering read(0) loop");
    loop {
        let n = unsafe { _read(0, buf.as_mut_ptr() as *mut core::ffi::c_void, buf.len()) };
        if n > 0 {
            let _ = debug_print(&format!("shell: read(0) got {} bytes", n));
            handle_line_payload(stdout, stdlog, &mut command_context, &buf[..n as usize])?;
            #[cfg(feature = "lang-parser")]
            {
                drain_job_notifications(&mut command_context);
                reap_done_jobs(stdout, &mut command_context);
            }
        } else if n == 0 {
            let _ = debug_print("shell: stdin EOF, exiting");
            return Ok(());
        } else {
            // Negative = errno set. Yield and retry — typically transient.
            let _ = yield_cpu();
        }
    }
}

fn print_prompt(_endpoint: usize) -> Result<()> {
    let user = read_env_var("USER").unwrap_or_else(|| String::from("cluu"));
    let cwd = libcluu::posix::current_dir_string();
    let prompt = format!("{}:{}> ", user, cwd);
    crate::write_stdout(prompt.as_bytes());
    Ok(())
}

/// Resolve a tab-completion query forwarded from TTY.
///
/// Currently unreachable: the TTY_TAB_QUERY_LABEL recv path was removed
/// with the shell stdin migration to POSIX read(0). Kept for the future
/// fd-0-based completion plumbing (cluuterm or /dev/ttyN); will be wired
/// up again once a TAB injection mechanism exists on the new path.
#[allow(dead_code)]
fn handle_tab_query(payload: &[u8], mode: u32, stdout: usize) -> Vec<u8> {
    let buf = match core::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    // Identify the last whitespace-delimited token. Anything before it is
    // preserved verbatim; we only complete/list against the trailing token.
    let token_start = buf
        .as_bytes()
        .iter()
        .rposition(|&b| b == b' ' || b == b'\t')
        .map(|p| p + 1)
        .unwrap_or(0);
    let token = &buf[token_start..];

    // Split token into (parent_dir_str, prefix).
    let (parent_dir_str, prefix) = match token.rfind('/') {
        Some(idx) => (&token[..=idx], &token[idx + 1..]),
        None => ("", token),
    };

    // Resolve parent against CWD if relative.
    let cwd = libcluu::posix::current_dir_string();
    let resolved_parent: String = if parent_dir_str.is_empty() {
        cwd
    } else if parent_dir_str.starts_with('/') {
        String::from(parent_dir_str)
    } else {
        let mut s = cwd;
        if !s.ends_with('/') {
            s.push('/');
        }
        s.push_str(parent_dir_str);
        s
    };

    let vfs_endpoint = match cached_vfs_endpoint() {
        Some(ep) => ep,
        None => return Vec::new(),
    };
    let vfs = match libcluu::fs::client::VfsClient::new_from_registry(vfs_endpoint) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    let entries = match vfs.readdir(&resolved_parent) {
        Ok(e) => e,
        Err(_) => return Vec::new(),
    };

    let matches: Vec<&libcluu::fs::client::VfsDirEntry> =
        entries.iter().filter(|e| e.name.starts_with(prefix)).collect();

    if matches.is_empty() {
        return Vec::new();
    }

    if matches.len() == 1 {
        let only = matches[0];
        let mut out: Vec<u8> = only.name[prefix.len()..].as_bytes().to_vec();
        out.push(if only.is_dir { b'/' } else { b' ' });
        return out;
    }

    // 2+ matches.
    if mode == 1 {
        // Double TAB: write list + redrawn prompt + current buffer to stdout.
        emit_tab_list(stdout, &matches, buf);
        return Vec::new();
    }

    // Single TAB with multiple matches: extend by common prefix beyond what
    // the user has typed (silent if no advance possible).
    let common = longest_common_prefix(matches.iter().map(|e| e.name.as_str()));
    if common.len() > prefix.len() {
        common.as_bytes()[prefix.len()..].to_vec()
    } else {
        Vec::new()
    }
}

/// Length of the longest string prefix shared by all `names`. Empty if no
/// common prefix or `names` is empty.
#[allow(dead_code)]
fn longest_common_prefix<'a, I: IntoIterator<Item = &'a str>>(names: I) -> String {
    let mut iter = names.into_iter();
    let first = match iter.next() {
        Some(s) => s,
        None => return String::new(),
    };
    let mut end = first.len();
    for n in iter {
        let limit = end.min(n.len());
        let fb = first.as_bytes();
        let nb = n.as_bytes();
        let mut i = 0;
        while i < limit && fb[i] == nb[i] {
            i += 1;
        }
        end = i;
        if end == 0 {
            break;
        }
    }
    // Walk back to a UTF-8 char boundary.
    while end > 0 && !first.is_char_boundary(end) {
        end -= 1;
    }
    String::from(&first[..end])
}

/// Emit the double-TAB redraw: newline, formatted match list, newline,
/// fresh prompt, then the current buffer so the user can keep typing.
#[allow(dead_code)]
fn emit_tab_list(stdout: usize, matches: &[&libcluu::fs::client::VfsDirEntry], buf: &str) {
    let mut out = String::from("\r\n");
    let mut first = true;
    for e in matches {
        if !first {
            out.push_str("  ");
        }
        first = false;
        out.push_str(&e.name);
        if e.is_dir {
            out.push('/');
        }
    }
    out.push_str("\r\n");
    write_stdout(out.as_bytes());

    let _ = print_prompt(stdout);
    if !buf.is_empty() {
        write_stdout(buf.as_bytes());
    }
}

/// Return a process-cached `vfs:main` endpoint token, fetching it on first
/// call. Returns None only if registry isn't yet wired (e.g. extremely early
/// boot races) — every subsequent call returns the same token.
///
/// Shell is single-threaded, so the cache uses a plain AtomicUsize:
///   0  = not yet acquired (first call should fetch)
///   !0 = endpoint token (reuse forever)
#[allow(dead_code)]
fn cached_vfs_endpoint() -> Option<usize> {
    use core::sync::atomic::{AtomicUsize, Ordering};
    static VFS_ENDPOINT: AtomicUsize = AtomicUsize::new(0);
    let cached = VFS_ENDPOINT.load(Ordering::Relaxed);
    if cached != 0 {
        return Some(cached);
    }
    match libcluu::registry::subscribe_output("vfs", "main") {
        Ok(ep) if ep != 0 => {
            VFS_ENDPOINT.store(ep, Ordering::Relaxed);
            Some(ep)
        }
        _ => None,
    }
}

/// Read an environment variable from the ProcessInfo page.
pub(crate) fn read_env_var(name: &str) -> Option<String> {
    use libcluu::boot::{process_info, PARAM_ENVC, PARAM_ENV_OFFSET};

    let info = process_info();
    let envc = info.params[PARAM_ENVC] as usize;
    let env_offset = info.params[PARAM_ENV_OFFSET] as usize;
    if envc == 0 || env_offset == 0 {
        return None;
    }

    let page_base = libcluu::boot::PROCESS_INFO_ADDR & !(4096 - 1);
    let page_end = page_base + 4096;
    let mut ptr = (page_base + env_offset) as *const u8;
    let prefix_len = name.len();

    for _ in 0..envc {
        if (ptr as usize) >= page_end { break; }
        let start = ptr;
        let mut len = 0usize;
        unsafe {
            while (start.add(len) as usize) < page_end && *start.add(len) != 0 {
                len += 1;
            }
        }
        if len == 0 { break; }
        let kv = unsafe { core::slice::from_raw_parts(start, len) };
        // Check "NAME=value"
        if kv.len() > prefix_len && kv[prefix_len] == b'=' && &kv[..prefix_len] == name.as_bytes() {
            if let Ok(val) = core::str::from_utf8(&kv[prefix_len + 1..]) {
                return Some(String::from(val));
            }
        }
        ptr = unsafe { start.add(len + 1) };
    }
    None
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

            parse_and_execute_line(stdout, stdlog, context, payload)?;

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
    stdlog: usize,
    context: &mut CommandContext,
    payload: &[u8],
) -> Result<()> {
    let line = strip_trailing_newline(payload);
    match core::str::from_utf8(line) {
        Ok(text) => {
            // Push non-empty lines into history before execution.
            if !text.trim().is_empty() {
                context.history.push(String::from(text));
                context.cmd_count += 1;
                if context.cmd_count % 10 == 0 {
                    crate::commands::builtins::history::save_history(context);
                }
            }
            match cluu_lang::parse_program(text) {
                Ok(ast) => {
                    let factory = BuiltinFactory::new();
                    let registry = factory.build();
                    match registry.execute(stdout, context, &ast) {
                        Ok(ExecResult::Handled) => {}
                        Ok(ExecResult::NotHandled) => {
                            let _ = debug_print("shell: unsupported command");
                            let _ = send_with_payload(
                                stdlog,
                                TTY_WRITE_LABEL,
                                b"shell: unsupported command\n",
                            );
                        }
                        Err(err) => {
                            let _ = send_with_payload(
                                stdlog,
                                TTY_WRITE_LABEL,
                                err.to_string().as_bytes(),
                            );
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
                    let _ = send_with_payload(stdlog, TTY_WRITE_LABEL, err.to_string().as_bytes());
                    let _ = debug_print(&format!("shell: parse error {}", err));
                }
            }
        }
        Err(_) => {
            let _ = send_with_payload(stdlog, TTY_WRITE_LABEL, b"shell: invalid utf-8\n");
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
