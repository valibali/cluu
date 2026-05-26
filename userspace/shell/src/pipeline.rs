//! Pipeline executor — turns a multi-command Pipeline AST into spawn calls
//! wired with pipes between stages.
//!
//! See `docs/superpowers/specs/2026-04-27-pipes-design.md` §6.
//! Single-command pipelines stay on the existing single-command path in
//! `commands.rs`. This module owns the multi-command (`a | b | c ...`) case.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use cluu_lang::ast::{CmdElem, Pipeline};

use crate::commands::{build_redir_actions, render_word_public, spawn_process_with_argv_and_redirs, BuiltinRegistry, CommandContext, WriteSink};
use libcluu::ipc::{
    PROCMGR_PIPE_CLOSE_LABEL, PROCMGR_PIPE_CREATE_LABEL,
};
use cluu_wire::spawn::{FdInherit, FdRights, FdSource, SpawnEnvelope, ViewSource};
use libcluu::posix::jobs::{pg_attach, pg_create, tty_set_fg};
use libcluu::syscall::endpoint_create;
use libcluu::types::Message;
use libcluu::{boot::process_info, debug_print, Error, IpcFlags, Result, TOKEN_IPC};
use libcluu::posix::snapshot_env;

struct PipeHandles {
    write_token: usize,
    read_token: usize,
    pipe_id: usize,
}

/// Walk the parsed `Pipeline` and execute each stage with stdin/stdout
/// wired to a fresh pipe between adjacent commands.
pub struct PipelineExecutor;

impl PipelineExecutor {
    /// Run a multi-command pipeline.
    ///
    /// Caller is responsible for routing single-command pipelines through
    /// the existing single-command path; this entry point accepts any
    /// pipeline length but is a no-op when `commands.len() < 2`.
    ///
    /// Returns the exit status of the LAST command (POSIX default; `set -o
    /// pipefail` is deferred per spec §10.2).
    pub fn run(
        stdout: usize,
        context: &mut CommandContext,
        pipeline: &Pipeline,
        registry: &BuiltinRegistry,
    ) -> Result<i32> {
        // Single-command pipeline with no redirections AND not bg: caller
        // (registry dispatcher) handles via the single-command path. We get
        // here only when the dispatcher routed bg, redirs, or multi-stage.
        if pipeline.commands.len() == 1 && pipeline.commands[0].redirs.is_empty() && !pipeline.bg {
            return Ok(0);
        }
        // Single-command pipeline with redirections OR bg: handle directly
        // without pipes; run_single_with_redirs handles both.
        if pipeline.commands.len() == 1 {
            return Self::run_single_with_redirs(stdout, context, pipeline, registry);
        }
        Self::run_multi(stdout, context, pipeline, registry)
    }

    fn run_single_with_redirs(
        stdout: usize,
        context: &mut CommandContext,
        pipeline: &Pipeline,
        registry: &BuiltinRegistry,
    ) -> Result<i32> {
        let cmd = &pipeline.commands[0];
        let bg = pipeline.bg;
        let mut argv: Vec<String> = Vec::new();
        for elem in &cmd.elems {
            match elem {
                CmdElem::Word(w) => argv.push(render_word_public(context, w)),
                CmdElem::Subshell(_) => {
                    crate::write_stdout(b"shell: subshells not supported\n");
                    return Ok(2);
                }
            }
        }
        if argv.is_empty() {
            crate::write_stdout(b"shell: empty command\n");
            return Ok(2);
        }
        let vfs_client = libcluu::registry::subscribe_output("vfs", "main")
            .ok()
            .and_then(|ep| libcluu::fs::client::VfsClient::new_from_registry(ep).ok());
        let name = argv[0].as_str();
        let image_name_owned = match vfs_client.as_ref() {
            Some(vfs) => crate::path_lookup::resolve_to_image_name(name, vfs),
            None => alloc::string::String::from(name),
        };
        let image_name = image_name_owned.as_str();
        let cmd_line = argv.join(" ");

        // Dispatch to builtin if registered. Three cases:
        //   1) no redirs       → WriteSink::Tty
        //   2) only stdout (>) → Capture sink, then VFS-write to file
        //   3) any other redir → fall through to container spawn (legacy)
        if let Some(builtin) = registry.find(image_name) {
            let arg_refs: Vec<String> = argv.iter().skip(1).cloned().collect();

            // Case 2: stdout redirected, no other redirs.
            let stdout_redirs: Vec<&cluu_lang::ast::Redir> = cmd
                .redirs
                .iter()
                .filter(|r| matches!(r.op, cluu_lang::ast::RedirOp::OutTrunc | cluu_lang::ast::RedirOp::OutAppend))
                .collect();
            let only_stdout_redir = !cmd.redirs.is_empty()
                && cmd.redirs.len() == stdout_redirs.len()
                && stdout_redirs.len() == 1;

            if cmd.redirs.is_empty() {
                let sink = WriteSink::Tty(stdout);
                if let Err(e) = builtin.run_with_sink(&sink, context, &arg_refs) {
                    let line = alloc::format!("shell: builtin '{}' failed: {:?}\n", image_name, e);
                    crate::write_stdout(line.as_bytes());
                    return Ok(1);
                }
                return Ok(0);
            } else if only_stdout_redir {
                let redir = stdout_redirs[0];
                let raw_target = render_word_public(context, &redir.target);
                // VFS rejects relative paths; resolve against shell cwd.
                let target_path = libcluu::posix::resolve_path(&raw_target);
                let append = matches!(redir.op, cluu_lang::ast::RedirOp::OutAppend);

                // Capture builtin output into a stack-owned Vec.
                let mut buf: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
                let sink = WriteSink::Capture(&mut buf as *mut _);
                if let Err(e) = builtin.run_with_sink(&sink, context, &arg_refs) {
                    let line = alloc::format!("shell: builtin '{}' failed: {:?}\n", image_name, e);
                    crate::write_stdout(line.as_bytes());
                    return Ok(1);
                }

                // Flush captured bytes to VFS file at target_path.
                let vfs_endpoint = match libcluu::registry::subscribe_output("vfs", "main") {
                    Ok(ep) => ep,
                    Err(e) => {
                        let line = alloc::format!("shell: vfs unavailable: {:?}\n", e);
                        crate::write_stdout(line.as_bytes());
                        return Ok(1);
                    }
                };
                let vfs = match libcluu::fs::client::VfsClient::new_from_registry(vfs_endpoint) {
                    Ok(c) => c,
                    Err(e) => {
                        let line = alloc::format!("shell: vfs client: {:?}\n", e);
                        crate::write_stdout(line.as_bytes());
                        return Ok(1);
                    }
                };

                // O_WRONLY=1, O_CREAT=0o1000, O_TRUNC=0o2000, O_APPEND=0o10.
                let flags = 1 | 0o1000 | if append { 0o10 } else { 0o2000 };
                let file = match vfs.open_with(&target_path, flags, 0o644) {
                    Ok(f) => f,
                    Err(e) => {
                        let line = alloc::format!("shell: '{}': {:?}\n", target_path, e);
                        crate::write_stdout(line.as_bytes());
                        return Ok(1);
                    }
                };

                // For append, write at end-of-file (size). Otherwise at offset 0.
                let off: usize = if append { file.size } else { 0 };
                if let Err(e) = vfs.write(file, off, &buf) {
                    let line = alloc::format!("shell: write '{}': {:?}\n", target_path, e);
                    crate::write_stdout(line.as_bytes());
                }
                let _ = vfs.close(file);
                return Ok(0);
            }
            // case 3: fall through (other redir kinds — stdin, stderr) to spawn
            // path. Builtins don't currently have stdin or stderr file paths
            // wired; reaches the spawn-not-found error below for now.
        }

        let arg_refs: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();
        let redirs = build_redir_actions(context, &cmd.redirs);

        let spawn = match spawn_process_with_argv_and_redirs(context, image_name, 200, &arg_refs, &redirs) {
            Ok(s) => s,
            Err(e) => {
                let line = alloc::format!("shell: spawn error {:?}\n", e);
                crate::write_stdout(line.as_bytes());
                return Ok(127);
            }
        };
        if spawn.status_word != 0 {
            let line = alloc::format!("shell: '{}' failed to start (status={})\n", image_name, spawn.status_word);
            crate::write_stdout(line.as_bytes());
            return Ok(127);
        }

        // Assign pgid and attach this process to it.
        let pgid = if let Ok(ep) = context.procmgr_spawn_endpoint() {
            match pg_create(ep) {
                Ok(id) => {
                    let _ = pg_attach(ep, id, spawn.pid);
                    id
                }
                Err(_) => 0,
            }
        } else {
            0
        };

        if bg {
            // Background: add to job table and return immediately.
            if pgid != 0 && context.tty_stdout != 0 && context.session_id != 0 && context.shell_pgid != 0 {
                let _ = tty_set_fg(context.tty_stdout, context.session_id, context.shell_pgid);
            }
            let job_id = context.jobs.add(
                pgid,
                alloc::vec![spawn.pid],
                alloc::vec![spawn.notify_endpoint],
                cmd_line.clone(),
                true,
            );
            let line = format!("[{}] {}", job_id, spawn.pid);
            let _ = libcluu::debug_print(&line);
            crate::write_stdout((line + "\n").as_bytes());
            return Ok(0);
        }

        // Foreground: set TTY fg pgid, wait for exit, restore shell.
        let want_fg_swap = pgid != 0 && context.tty_stdout != 0 && context.session_id != 0;
        if want_fg_swap {
            let _ = tty_set_fg(context.tty_stdout, context.session_id, pgid);
        }
        // pts mode: push fg pgid via PTS_SET_PGRP_LABEL on fd 0.
        let want_pts_fg_swap = pgid != 0 && context.tty_stdout == 0;
        if want_pts_fg_swap {
            let _ = libcluu::posix::termios::tcsetpgrp(0, pgid as i32);
        }

        // Wait for exit notification.
        let mut buf = [0u8; 256];
        let _ = libcluu::syscall::ipc_recv(spawn.notify_endpoint, &mut buf);
        let exit_code = if buf.len() >= 24 {
            let bytes = [buf[16], buf[17], buf[18], buf[19], buf[20], buf[21], buf[22], buf[23]];
            i64::from_le_bytes(bytes) as i32
        } else {
            0
        };

        // Restore shell as TTY foreground.
        if want_fg_swap && context.shell_pgid != 0 {
            let _ = tty_set_fg(context.tty_stdout, context.session_id, context.shell_pgid);
        }
        // Restore shell in pts mode.
        if want_pts_fg_swap && context.shell_pgid != 0 {
            let _ = libcluu::posix::termios::tcsetpgrp(0, context.shell_pgid as i32);
        }

        let _ = libcluu::debug_print(&alloc::format!(
            "shell: pipeline done stages=1 status={}",
            exit_code
        ));
        Ok(exit_code)
    }

    fn run_multi(
        stdout: usize,
        context: &mut CommandContext,
        pipeline: &Pipeline,
        registry: &BuiltinRegistry,
    ) -> Result<i32> {
        let n = pipeline.commands.len();
        let bg = pipeline.bg;
        let procmgr_ep = context.procmgr_spawn_endpoint()?;

        // Render every command's argv (and reject unsupported features for v1).
        let mut argvs: Vec<Vec<String>> = Vec::with_capacity(n);
        let mut redir_lists: Vec<Vec<libcluu::ipc::RedirAction>> = Vec::with_capacity(n);
        for cmd in &pipeline.commands {
            let mut argv: Vec<String> = Vec::new();
            for elem in &cmd.elems {
                match elem {
                    CmdElem::Word(w) => {
                        argv.push(render_word_public(context, w));
                    }
                    CmdElem::Subshell(_) => {
                        crate::write_stdout(b"shell: subshells inside pipelines not supported\n");
                        return Ok(2);
                    }
                }
            }
            if argv.is_empty() {
                crate::write_stdout(b"shell: empty command in pipeline\n");
                return Ok(2);
            }
            argvs.push(argv);
            redir_lists.push(build_redir_actions(context, &cmd.redirs));
        }

        // Allocate N-1 pipes.
        let mut pipes: Vec<PipeHandles> = Vec::with_capacity(n - 1);
        let mut alloc_failed: Option<Error> = None;
        for _ in 0..(n - 1) {
            match Self::pipe_create(procmgr_ep) {
                Ok(p) => pipes.push(p),
                Err(e) => {
                    alloc_failed = Some(e);
                    break;
                }
            }
        }
        if let Some(e) = alloc_failed {
            for p in &pipes {
                let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
            }
            return Err(e);
        }

        // Build the env trailer once for all stages — same semantics as
        // single-command spawn in commands/exec.rs: base from snapshot_env()
        // overlaid with any exported shell vars.
        let mut env_pairs: Vec<(String, String)> = snapshot_env();
        for (k, v) in context.exported_pairs() {
            if let Some(idx) = env_pairs.iter().position(|(ek, _)| ek == &k) {
                env_pairs[idx].1 = v;
            } else {
                env_pairs.push((k, v));
            }
        }
        let env_refs: Vec<(&str, &str)> = env_pairs
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();

        // Build a human-readable command line for the job table.
        let cmd_line: String = argvs
            .iter()
            .map(|argv| argv.join(" "))
            .collect::<Vec<_>>()
            .join(" | ");

        // Allocate a pgid for this pipeline.
        let pipeline_pgid = pg_create(procmgr_ep).unwrap_or(0);

        // For foreground pipelines, point TTY fg at this pgid before spawning.
        if !bg && pipeline_pgid != 0 && context.tty_stdout != 0 && context.session_id != 0 {
            let _ = tty_set_fg(context.tty_stdout, context.session_id, pipeline_pgid);
        }
        // pts mode: push fg pgid via PTS_SET_PGRP_LABEL on fd 0.
        if !bg && pipeline_pgid != 0 && context.tty_stdout == 0 {
            let _ = libcluu::posix::termios::tcsetpgrp(0, pipeline_pgid as i32);
        }

        // Build a VfsClient once for path → image-name resolution.
        // Some pipelines may run before VFS is up; fall back to passing the
        // raw name through (procmgr will reject invalid names).
        let vfs_client = libcluu::registry::subscribe_output("vfs", "main")
            .ok()
            .and_then(|ep| libcluu::fs::client::VfsClient::new_from_registry(ep).ok());

        // Classify each stage as builtin or container. Builtin stages run
        // in-process; container stages are spawned via procmgr. We must
        // spawn container stages FIRST so they block on ipc_recv before we
        // run any inline builtin that writes to a pipe.
        //
        // `is_builtin[i] = true` means stage i runs inline.
        let mut is_builtin: Vec<bool> = Vec::with_capacity(n);
        for argv in &argvs {
            let name = argv[0].as_str();
            let image_name_owned = match vfs_client.as_ref() {
                Some(vfs) => crate::path_lookup::resolve_to_image_name(name, vfs),
                None => alloc::string::String::from(name),
            };
            let image_name = image_name_owned.as_str();
            is_builtin.push(registry.find(image_name).is_some());
        }

        // Pass 1: spawn all container stages.
        // notify_endpoints[i] is 0 for builtin stages (no wait needed).
        let mut notify_endpoints: Vec<usize> = alloc::vec![0usize; n];
        // pids for job table
        let mut spawned_pids: Vec<usize> = Vec::with_capacity(n);

        for (i, argv) in argvs.iter().enumerate() {
            if is_builtin[i] {
                // Will be run inline in pass 2. Skip spawn.
                continue;
            }

            let name = argv[0].as_str();
            let image_name_owned = match vfs_client.as_ref() {
                Some(vfs) => crate::path_lookup::resolve_to_image_name(name, vfs),
                None => alloc::string::String::from(name),
            };
            let image_name = image_name_owned.as_str();
            let arg_refs: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();

            // Check for conflicts: a stage cannot have both a pipe-wired fd
            // AND an explicit redir targeting the same fd.
            let stage_redirs = &redir_lists[i];
            if i > 0 {
                if stage_redirs.iter().any(|r| r.target_fd == 0) {
                    let line = format!(
                        "shell: pipeline stage {} ('{}') cannot redirect stdin and receive pipe input\n",
                        i, image_name
                    );
                    crate::write_stdout(line.as_bytes());
                    for p in &pipes {
                        let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
                    }
                    return Ok(1);
                }
            }
            if i < n - 1 {
                if stage_redirs.iter().any(|r| r.target_fd == 1) {
                    let line = format!(
                        "shell: pipeline stage {} ('{}') cannot redirect stdout and feed pipe output\n",
                        i, image_name
                    );
                    crate::write_stdout(line.as_bytes());
                    for p in &pipes {
                        let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
                    }
                    return Ok(1);
                }
            }

            let mut fd_inherit: Vec<FdInherit> = Vec::with_capacity(2);
            // Wire read end of upstream pipe as stdin (not for first stage).
            if i > 0 {
                fd_inherit.push(FdInherit {
                    child_fd: 0,
                    source: FdSource::EndpointCap {
                        endpoint_token: pipes[i - 1].read_token as u64,
                    },
                    rights: FdRights::READ_ONLY,
                });
            }
            // Wire write end of downstream pipe as stdout (not for last stage).
            if i < n - 1 {
                fd_inherit.push(FdInherit {
                    child_fd: 1,
                    source: FdSource::EndpointCap {
                        endpoint_token: pipes[i].write_token as u64,
                    },
                    rights: FdRights::WRITE_ONLY,
                });
            }

            // Build env pairs from refs
            let env: Vec<(String, String)> = env_refs.iter()
                .map(|(k, v)| (String::from(*k), String::from(*v)))
                .collect();

            let notify_endpoint = match endpoint_create(process_info().tokens[TOKEN_IPC]) {
                Ok(ep) => ep,
                Err(e) => {
                    for p in &pipes {
                        let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
                    }
                    return Err(e);
                }
            };

            let image: String = image_name.into();
            let args: Vec<String> = arg_refs.iter().map(|s| String::from(*s)).collect();

            let envelope = SpawnEnvelope {
                image,
                args,
                env: env,
                view: ViewSource::Derive(libcluu::token(libcluu::boot::TOKEN_EXTRA_0) as u64),
                fd_inherit,
                session: None,
                notify: Some(notify_endpoint as u64),
            };

            let reply = match libcluu::spawn::spawn(envelope) {
                Ok(r) => r,
                Err(e) => {
                    let _ = debug_print(&format!(
                        "shell: pipeline stage '{}' spawn failed: {:?}\n",
                        image_name, e));
                    for p in &pipes {
                        let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
                    }
                    return Ok(127);
                }
            };
            let child_pid = reply.pid as usize;
            if pipeline_pgid != 0 {
                let _ = pg_attach(procmgr_ep, pipeline_pgid, child_pid);
            }
            spawned_pids.push(child_pid);
            notify_endpoints[i] = notify_endpoint;
        }

        // Pass 2: run inline builtin stages in forward order.
        // Container stages are already running and blocked on ipc_recv, so
        // sends from builtins will be delivered.
        let mut last_builtin_status: i32 = 0;
        for (i, argv) in argvs.iter().enumerate() {
            if !is_builtin[i] {
                continue;
            }
            let name = argv[0].as_str();
            let image_name_owned = match vfs_client.as_ref() {
                Some(vfs) => crate::path_lookup::resolve_to_image_name(name, vfs),
                None => alloc::string::String::from(name),
            };
            let image_name = image_name_owned.as_str();
            let builtin = match registry.find(image_name) {
                Some(b) => b,
                None => continue, // classified as builtin but not found; skip
            };
            let sink = if i < n - 1 {
                WriteSink::Pipe(pipes[i].write_token)
            } else {
                WriteSink::Tty(stdout)
            };
            let arg_refs: Vec<String> = argv.iter().skip(1).cloned().collect();
            if let Err(e) = builtin.run_with_sink(&sink, context, &arg_refs) {
                let line = format!(
                    "shell: builtin '{}' failed: {:?}\n",
                    image_name, e
                );
                crate::write_stdout(line.as_bytes());
                last_builtin_status = 1;
            }
            // Send EOF on the downstream pipe so the next stage sees EOF.
            sink.close();
        }

        // Wait for each child's exit notification in spawn order.
        //
        // Phase 4 Plan E decision: keep sequential. Multiplexed wait via
        // poll() is technically possible (Phase 3 shipped poll), but only
        // matters for pathological cases like `yes | head -1` where stage 0
        // blocks on EPIPE before the shell drains. Correctness is unaffected.
        // If a soak workload exposes a real hang, revisit.
        //
        // `cat | head -3` example:
        //   - head finishes after 3 lines
        //   - cat sees EPIPE on next write and exits
        //   - we wait cat → head sequentially, both reaped
        let _ = last_builtin_status; // consumed below if all stages are builtins
        let mut last_status: i32 = last_builtin_status;
        let mut buf = [0u8; 256];
        let mut waited_any = false;
        for (i, &notify) in notify_endpoints.iter().enumerate() {
            if is_builtin[i] {
                // No notify endpoint for builtin stages.
                continue;
            }
            waited_any = true;
            // ipc_recv blocks (with rolling 30-second timeouts) until a message arrives.
            let _ = libcluu::syscall::ipc_recv(notify, &mut buf);

            // Parse the exit notification: Message layout is
            //   [u32 label][u8 words][u8 extra][u16 pad] = 8 bytes tag
            //   [usize words[0]] = cookie      (offset 8)
            //   [usize words[1]] = exit_code   (offset 16)
            let exit_code = if buf.len() >= 24 {
                let bytes = [
                    buf[16], buf[17], buf[18], buf[19],
                    buf[20], buf[21], buf[22], buf[23],
                ];
                i64::from_le_bytes(bytes) as i32
            } else {
                0
            };
            if i == n - 1 {
                last_status = exit_code;
            }
        }
        // If the last stage was a builtin, last_status already holds its exit code.
        // If no containers were waited, keep the builtin status as-is.
        let _ = waited_any;

        // Close the shell's own pipe tokens. Children received fresh derived
        // tokens from procmgr via the FdInherit path, so revoking the shell's tokens
        // here does not disturb children that are already running. Closing is
        // done after the wait so pipe endpoints remain live while children run.
        for p in &pipes {
            let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
        }

        // Job table registration and TTY fg restore.
        // Collect non-zero notify endpoints for this pipeline's spawned stages.
        let job_notify_eps: Vec<usize> = notify_endpoints
            .iter()
            .copied()
            .filter(|&ep| ep != 0)
            .collect();

        if bg && pipeline_pgid != 0 {
            // Background pipeline: add to job table, print [N] PID.
            let job_id = context.jobs.add(
                pipeline_pgid,
                spawned_pids.clone(),
                job_notify_eps,
                cmd_line.clone(),
                true,
            );
            let first_pid = spawned_pids.first().copied().unwrap_or(0);
            let line = format!("[{}] {}", job_id, first_pid);
            let _ = libcluu::debug_print(&line);
            crate::write_stdout((line + "\n").as_bytes());
            // Restore TTY foreground to shell.
            if context.tty_stdout != 0 && context.session_id != 0 && context.shell_pgid != 0 {
                let _ = tty_set_fg(context.tty_stdout, context.session_id, context.shell_pgid);
            }
            // pts mode: restore shell fg.
            if context.tty_stdout == 0 && context.shell_pgid != 0 {
                let _ = libcluu::posix::termios::tcsetpgrp(0, context.shell_pgid as i32);
            }
        } else if !bg && pipeline_pgid != 0 {
            // Foreground pipeline: restore TTY fg to shell after wait completes.
            if context.tty_stdout != 0 && context.session_id != 0 && context.shell_pgid != 0 {
                let _ = tty_set_fg(context.tty_stdout, context.session_id, context.shell_pgid);
            }
            // pts mode: restore shell fg.
            if context.tty_stdout == 0 && context.shell_pgid != 0 {
                let _ = libcluu::posix::termios::tcsetpgrp(0, context.shell_pgid as i32);
            }
        }

        let _ = debug_print(&format!(
            "shell: pipeline done stages={} status={}",
            n, last_status
        ));

        Ok(last_status)
    }

    fn pipe_create(procmgr_ep: usize) -> Result<PipeHandles> {
        let mut req = Message::new(PROCMGR_PIPE_CREATE_LABEL, [0; 6], 0);
        libcluu::ipc::call(procmgr_ep, &mut req, IpcFlags::empty())?;
        let status = req.words[0];
        if status != 0 {
            return Err(Error::OutOfMemory);
        }
        Ok(PipeHandles {
            write_token: req.words[1],
            read_token: req.words[2],
            pipe_id: req.words[3],
        })
    }

    fn pipe_close(procmgr_ep: usize, pipe_id: usize) -> Result<()> {
        let mut req = Message::new(PROCMGR_PIPE_CLOSE_LABEL, [0; 6], 1);
        req.words[0] = pipe_id;
        libcluu::ipc::call(procmgr_ep, &mut req, IpcFlags::empty())?;
        Ok(())
    }
}
