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

use crate::commands::{build_redir_actions, render_word_public, spawn_process_with_argv_and_redirs, CommandContext};
use libcluu::ipc::{
    build_container_run_payload_full, call_with_payload, send_with_payload,
    FdAction, PROCMGR_CONTAINER_RUN_LABEL, PROCMGR_PIPE_CLOSE_LABEL, PROCMGR_PIPE_CREATE_LABEL,
    TTY_WRITE_LABEL,
};
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
    ) -> Result<i32> {
        // Single-command pipeline with no redirections: caller shouldn't reach us.
        if pipeline.commands.len() == 1 && pipeline.commands[0].redirs.is_empty() {
            return Ok(0);
        }
        // Single-command pipeline with redirections: handle directly without pipes.
        if pipeline.commands.len() == 1 {
            return Self::run_single_with_redirs(stdout, context, pipeline);
        }
        Self::run_multi(stdout, context, pipeline)
    }

    fn run_single_with_redirs(
        stdout: usize,
        context: &mut CommandContext,
        pipeline: &Pipeline,
    ) -> Result<i32> {
        let cmd = &pipeline.commands[0];
        let mut argv: Vec<String> = Vec::new();
        for elem in &cmd.elems {
            match elem {
                CmdElem::Word(w) => argv.push(render_word_public(context, w)),
                CmdElem::Subshell(_) => {
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"shell: subshells not supported\n");
                    return Ok(2);
                }
            }
        }
        if argv.is_empty() {
            let _ = send_with_payload(stdout, TTY_WRITE_LABEL, b"shell: empty command\n");
            return Ok(2);
        }
        let name = argv[0].as_str();
        let image_name = name.strip_prefix("/bin/").unwrap_or(name);
        let arg_refs: Vec<&str> = argv.iter().skip(1).map(|s| s.as_str()).collect();
        let redirs = build_redir_actions(context, &cmd.redirs);

        let spawn = match spawn_process_with_argv_and_redirs(context, image_name, 200, &arg_refs, &redirs) {
            Ok(s) => s,
            Err(e) => {
                let line = alloc::format!("shell: spawn error {:?}\n", e);
                let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
                return Ok(127);
            }
        };
        if spawn.status_word != 0 {
            let line = alloc::format!("shell: '{}' failed to start (status={})\n", image_name, spawn.status_word);
            let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
            return Ok(127);
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
    ) -> Result<i32> {
        let n = pipeline.commands.len();
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
                        let _ = send_with_payload(
                            stdout,
                            TTY_WRITE_LABEL,
                            b"shell: subshells inside pipelines not supported\n",
                        );
                        return Ok(2);
                    }
                }
            }
            if argv.is_empty() {
                let _ = send_with_payload(
                    stdout,
                    TTY_WRITE_LABEL,
                    b"shell: empty command in pipeline\n",
                );
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

        // Spawn each command with FDAC entries threading stdin/stdout.
        let mut notify_endpoints: Vec<usize> = Vec::with_capacity(n);

        for (i, argv) in argvs.iter().enumerate() {
            let name = argv[0].as_str();
            // For external binaries, image name is the basename: strip leading
            // /bin/ prefix if present, since container names are the bare name.
            let image_name = name.strip_prefix("/bin/").unwrap_or(name);
            // Skip argv[0] (command name) — the container binary path is the
            // canonical argv[0] and is prepended by procmgr's container_run
            // handler. Passing user args starting at index 1 matches what
            // spawn_process_with_argv does.
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
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
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
                    let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
                    for p in &pipes {
                        let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
                    }
                    return Ok(1);
                }
            }

            let mut fdac: Vec<FdAction> = Vec::with_capacity(2);
            // Wire read end of upstream pipe as stdin (not for first stage).
            if i > 0 {
                fdac.push(FdAction {
                    target_fd: 0,
                    is_pipe: true,
                    endpoint: pipes[i - 1].read_token,
                });
            }
            // Wire write end of downstream pipe as stdout (not for last stage).
            if i < n - 1 {
                fdac.push(FdAction {
                    target_fd: 1,
                    is_pipe: true,
                    endpoint: pipes[i].write_token,
                });
            }

            let (payload, _argc, fdac_offset) =
                build_container_run_payload_full(image_name, &arg_refs, &fdac, stage_redirs, &env_refs);

            let notify_endpoint = match endpoint_create(process_info().tokens[TOKEN_IPC]) {
                Ok(ep) => ep,
                Err(e) => {
                    for p in &pipes {
                        let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
                    }
                    return Err(e);
                }
            };

            let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
            msg.words[0] = payload.len();
            msg.words[1] = notify_endpoint;
            msg.words[2] = fdac_offset;

            let mut reply = Message::new(0, [0; 6], 0);
            if let Err(e) = call_with_payload(procmgr_ep, &msg, &payload, &mut reply) {
                for p in &pipes {
                    let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
                }
                return Err(e);
            }
            let status = reply.words[0];
            if status != 0 {
                let line = format!(
                    "shell: pipeline stage {} ('{}') failed to start (status={})\n",
                    i, image_name, status
                );
                let _ = send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes());
                for p in &pipes {
                    let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
                }
                return Ok(127);
            }
            notify_endpoints.push(notify_endpoint);
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
        let mut last_status: i32 = 0;
        let mut buf = [0u8; 256];
        for (i, &notify) in notify_endpoints.iter().enumerate() {
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

        // Close the shell's own pipe tokens. Children received fresh derived
        // tokens from procmgr via the FDAC path, so revoking the shell's tokens
        // here does not disturb children that are already running. Closing is
        // done after the wait so pipe endpoints remain live while children run.
        for p in &pipes {
            let _ = Self::pipe_close(procmgr_ep, p.pipe_id);
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
