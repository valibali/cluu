#![no_std]
#![no_main]

extern crate alloc;

#[cfg(feature = "lang-parser")]
mod commands;
#[cfg(feature = "lang-parser")]
mod commands_old;
#[cfg(feature = "lang-parser")]
mod path_lookup;
#[cfg(feature = "lang-parser")]
mod pipeline;
#[cfg(feature = "lang-parser")]
mod shellrc;

use alloc::format;
#[cfg(feature = "lang-parser")]
use alloc::string::String;
#[cfg(feature = "lang-parser")]
use alloc::string::ToString;
#[cfg(feature = "lang-parser")]
use alloc::vec::Vec;
#[cfg(feature = "lang-parser")]
use commands::{poll_background_jobs, BuiltinFactory, CommandContext, CommandExecutor, ExecResult};
use libcluu::boot::{
    process_info, PARAM_ARGC, PARAM_ARGV_OFFSET, TOKEN_STDERR, TOKEN_STDIN, TOKEN_STDLOG,
    TOKEN_STDOUT,
};
use libcluu::ipc::{
    extract_reply_id, parse_message, reply_with_payload, send_with_payload, TTY_READ_LABEL,
    TTY_TAB_QUERY_LABEL, TTY_WRITE_LABEL,
};
use libcluu::registry;
use libcluu::types::Message;
use libcluu::{debug_print, yield_cpu, Error, Result};

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
    registry::register_default_outputs()?;
    let stdin = info.tokens[TOKEN_STDIN];
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

    debug_print("shell: ready")?;
    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"\x1b[2J\x1b[H");
    let _ = print_banner(stdout);
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

    let mut buf = [0u8; 128];
    loop {
        // Wait for either keyboard input via stdin or registry control traffic.
        let tokens = [stdin, registry_endpoint];
        match libcluu::syscall::ipc_recv_any(&tokens, &mut buf, 250) {
            Ok((index, len)) => {
                if let Some((msg, payload)) = parse_message(&buf[..len]) {
                    if index == 1 {
                        // Registry control messages (grants/status).
                        let _ = registry::handle_incoming_message(&msg, payload);
                        continue;
                    }
                    if msg.tag.label == TTY_READ_LABEL {
                        if !payload.is_empty() {
                            handle_line_payload(stdout, stdlog, &mut command_context, payload)?;
                        } else if msg.tag.words >= 2 {
                            let ch = msg.words[1] as u8;
                            handle_line_payload(stdout, stdlog, &mut command_context, &[ch])?;
                        }
                    } else if msg.tag.label == TTY_TAB_QUERY_LABEL {
                        // TTY asked us to compute a tab completion using OUR
                        // view + CWD (which TTY can't see).  Sync reply over
                        // the call mechanism — typical readdir is sub-ms.
                        if let Some(rid) = extract_reply_id(&msg) {
                            let suffix = handle_tab_query(payload);
                            let reply_msg = Message::new(TTY_TAB_QUERY_LABEL, [0; 6], 1);
                            let _ = reply_with_payload(rid, &reply_msg, &suffix);
                        }
                    }
                }
            }
            Err(Error::Timeout) => {
                #[cfg(feature = "lang-parser")]
                {
                    let _ = poll_background_jobs(stdout, &mut command_context);
                }
                let _ = yield_cpu();
            }
            Err(Error::WouldBlock) => {
                let _ = yield_cpu();
            }
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    }
}

fn print_prompt(endpoint: usize) -> Result<()> {
    let user = read_env_var("USER").unwrap_or_else(|| String::from("cluu"));
    let cwd = libcluu::posix::current_dir_string();
    let prompt = format!("{}:{}> ", user, cwd);
    send_with_payload(endpoint, TTY_WRITE_LABEL, prompt.as_bytes())?;
    Ok(())
}

/// Resolve a tab-completion query forwarded from TTY.
///
/// Input: the partial last-token bytes the user is typing (no NUL).
/// Output: the suffix to append after that token (with trailing '/' for
/// directories or ' ' for files), or empty for "no unique completion."
///
/// Splits the token at the rightmost '/' into (parent_dir, prefix). If
/// parent_dir is absent or relative, resolves against shell's CWD —
/// which is exactly the TTY-side limitation we're working around. Calls
/// VFS readdir using shell's view, so /etc and /var (and anything else
/// the user's envelope grants) are visible to tab.
///
/// Failure modes return empty (no completion). Caller (TTY) treats
/// empty as "do nothing," matching the silent-no-op behavior the user
/// already expects.
fn handle_tab_query(payload: &[u8]) -> Vec<u8> {
    let token = match core::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };

    // Split into (parent_dir_str, prefix).
    let (parent_dir_str, prefix) = match token.rfind('/') {
        Some(idx) => (&token[..=idx], &token[idx + 1..]),
        None => ("", token),
    };

    // Resolve parent against CWD if relative.
    let cwd = libcluu::posix::current_dir_string();
    let resolved_parent: String = if parent_dir_str.is_empty() {
        // No '/' in token: search the current working directory.
        cwd
    } else if parent_dir_str.starts_with('/') {
        // Absolute path.
        String::from(parent_dir_str)
    } else {
        // Relative path with a slash: prefix with CWD.
        let mut s = cwd;
        if !s.ends_with('/') {
            s.push('/');
        }
        s.push_str(parent_dir_str);
        s
    };

    // Get a VFS client. The endpoint is cached process-wide — without this,
    // every TAB does a fresh registry::subscribe_output("vfs", "main") and
    // leaks a new derived grant token (199, 200, 201, ... in the boot log).
    // Cache survives as long as the shell does.
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

    let mut matches = entries.iter().filter(|e| e.name.starts_with(prefix));
    let first = match matches.next() {
        Some(m) => m,
        None => return Vec::new(),
    };
    if matches.next().is_some() {
        return Vec::new(); // 2+ matches — silent no-op
    }

    // Build suffix: bytes after the already-typed prefix, plus '/' (dir) or ' ' (file).
    let mut out: Vec<u8> = first.name[prefix.len()..].as_bytes().to_vec();
    out.push(if first.is_dir { b'/' } else { b' ' });
    out
}

/// Return a process-cached `vfs:main` endpoint token, fetching it on first
/// call. Returns None only if registry isn't yet wired (e.g. extremely early
/// boot races) — every subsequent call returns the same token.
///
/// Shell is single-threaded, so the cache uses a plain AtomicUsize:
///   0  = not yet acquired (first call should fetch)
///   !0 = endpoint token (reuse forever)
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

fn print_banner(tty_endpoint: usize) -> Result<()> {
    // ASCII-only banner, stored in a separate file for easy editing.
    const BANNER: &str = include_str!("banner.txt");
    // Send per line to avoid splitting UTF-8 sequences across IPC messages.
    for line in BANNER.lines() {
        send_with_payload(tty_endpoint, TTY_WRITE_LABEL, line.as_bytes())?;
        send_with_payload(tty_endpoint, TTY_WRITE_LABEL, b"\n")?;
    }
    send_with_payload(tty_endpoint, TTY_WRITE_LABEL, b"\n")?;
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
            parse_and_execute_line(stdout, stdlog, context, payload)?;
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
        Ok(text) => match cluu_lang::parse_program(text) {
            Ok(ast) => {
                let factory = BuiltinFactory::new();
                let registry = factory.build();
                match registry.execute(stdout, context, &ast) {
                    Ok(ExecResult::Handled) => return Ok(()),
                    Ok(ExecResult::NotHandled) => {}
                    Err(err) => {
                        let _ =
                            send_with_payload(stdlog, TTY_WRITE_LABEL, err.to_string().as_bytes());
                        let _ = debug_print(&format!("shell: builtin error {}", err));
                        return Ok(());
                    }
                }
                let _ = debug_print("shell: unsupported command");
                let _ = send_with_payload(stdlog, TTY_WRITE_LABEL, b"shell: unsupported command\n");
            }
            Err(err) => {
                let _ = send_with_payload(stdlog, TTY_WRITE_LABEL, err.to_string().as_bytes());
                let _ = debug_print(&format!("shell: parse error {}", err));
            }
        },
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
