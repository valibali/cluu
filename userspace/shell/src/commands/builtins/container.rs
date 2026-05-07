//! `container` and `heap` builtins.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::mem::size_of;

use libcluu::boot::{process_info, TOKEN_STDIN};
use libcluu::ipc::{
    build_container_run_payload_with_argv, call, call_with_payload, call_with_reply_buf, recv,
    send_with_payload, PROCMGR_CONTAINER_LIST_LABEL, PROCMGR_CONTAINER_RUN_LABEL,
    TTY_FG_FLAG_FORWARD_CTRL_C, TTY_WRITE_LABEL,
};
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{IpcFlags, Result, TOKEN_IPC};

use crate::commands::exec::set_tty_foreground;
use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry};

const PROCMGR_KILL_LABEL: u32 = 3;
const SIGTERM: usize = 15;

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(HeapBuiltin));
    registry.register(Box::new(ContainerBuiltin));
}

// ---------------------------------------------------------------------------
// heap
// ---------------------------------------------------------------------------

pub(crate) struct HeapBuiltin;

impl BuiltinCommand for HeapBuiltin {
    fn name(&self) -> &'static str {
        "heap"
    }

    fn run(&self, stdout: usize, _context: &mut CommandContext, _args: &[String]) -> Result<()> {
        let stats = libcluu::allocator::stats();
        let line = format!(
            "heap: used={} total={} peak={} free={}\n",
            stats.used, stats.total, stats.peak, stats.free
        );
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// container
// ---------------------------------------------------------------------------

pub(crate) struct ContainerBuiltin;

impl BuiltinCommand for ContainerBuiltin {
    fn name(&self) -> &'static str {
        "container"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let subcmd = args.first().map(|s| s.as_str()).unwrap_or("");
        match subcmd {
            "run" => container_run(stdout, context, &args[1..]),
            "list" => container_list(stdout, context),
            "stop" => container_stop(stdout, context, &args[1..]),
            _ => {
                send_with_payload(
                    stdout,
                    TTY_WRITE_LABEL,
                    b"usage: container run|list|stop\n",
                )?;
                Ok(())
            }
        }
    }
}

fn build_container_run_payload(name: &str) -> Vec<u8> {
    build_container_run_payload_with_argv(name, &[]).0
}

fn container_run(stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
    let Some(name) = args.first() else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"container run: missing image name\n")?;
        return Ok(());
    };

    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
    let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;
    let payload = build_container_run_payload(name);
    let mut msg = Message::new(PROCMGR_CONTAINER_RUN_LABEL, [0; 6], 3);
    msg.words[0] = payload.len();
    msg.words[1] = notify_endpoint;
    msg.words[2] = 0;
    let mut reply = Message::new(0, [0; 6], 0);

    call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

    let status = reply.words[0];
    if status != 0 {
        let line = format!("container run: error {}\n", status);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
    } else {
        let pid = reply.words[1];
        let _cookie = reply.words[2];
        let cid = reply.words[3];
        let child_stdin = reply.words[4];
        let line = format!("container '{}' started pid={} cid={}\n", name, pid, cid);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;

        if child_stdin != 0 {
            set_tty_foreground(stdout, child_stdin, 0, TTY_FG_FLAG_FORWARD_CTRL_C)?;

            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());

            let shell_stdin = process_info().tokens[TOKEN_STDIN];
            let _ = set_tty_foreground(stdout, shell_stdin, 0, TTY_FG_FLAG_FORWARD_CTRL_C);
        } else {
            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());
        }
    }
    Ok(())
}

fn container_list(stdout: usize, context: &mut CommandContext) -> Result<()> {
    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
    let msg = Message::new(PROCMGR_CONTAINER_LIST_LABEL, [0; 6], 0);
    let mut reply_buf = [0u8; 4096];

    let (reply_msg, payload_len) =
        call_with_reply_buf(procmgr_endpoint, &msg, &[], &mut reply_buf)?;

    if reply_msg.words[1] != 0 {
        let line = format!("container list: error {}\n", reply_msg.words[1]);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        return Ok(());
    }

    if payload_len == 0 {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"no containers running\n")?;
        return Ok(());
    }

    let hdr_len = size_of::<Message>();
    let payload = &reply_buf[hdr_len..hdr_len + payload_len];
    send_with_payload(stdout, TTY_WRITE_LABEL, payload)?;
    Ok(())
}

fn container_stop(stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
    let Some(name) = args.first() else {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"container stop: missing name\n")?;
        return Ok(());
    };

    let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
    let msg = Message::new(PROCMGR_CONTAINER_LIST_LABEL, [0; 6], 0);
    let mut reply_buf = [0u8; 4096];
    let (reply_msg, payload_len) =
        call_with_reply_buf(procmgr_endpoint, &msg, &[], &mut reply_buf)?;

    if reply_msg.words[1] != 0 || payload_len == 0 {
        send_with_payload(stdout, TTY_WRITE_LABEL, b"container stop: no containers found\n")?;
        return Ok(());
    }

    let hdr_len = size_of::<Message>();
    let payload = &reply_buf[hdr_len..hdr_len + payload_len];
    let listing = core::str::from_utf8(payload).unwrap_or("");

    let mut target_pid = None;
    let by_cid = name.starts_with('@');
    for line in listing.lines() {
        let mut parts = line.split_whitespace();
        let inst_name = parts.next().unwrap_or("");
        let pid_str = parts.next().unwrap_or("");
        let cid_str = parts.next().unwrap_or("");

        let matched = if by_cid {
            cid_str == &name[1..]
        } else {
            inst_name == name.as_str()
        };

        if matched {
            if let Ok(pid) = usize::from_str_radix(pid_str, 10) {
                target_pid = Some(pid);
                break;
            }
        }
    }

    let Some(pid) = target_pid else {
        let line = format!("container stop: '{}' not found\n", name);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
        return Ok(());
    };

    let mut kill_msg = Message::new(PROCMGR_KILL_LABEL, [0; 6], 2);
    kill_msg.words[0] = pid;
    kill_msg.words[1] = SIGTERM;
    call(procmgr_endpoint, &mut kill_msg, IpcFlags::empty())?;

    if kill_msg.words[0] != 0 {
        let line = format!("container stop: kill failed ({})\n", kill_msg.words[0]);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
    } else {
        let line = format!("container '{}' (pid={}) stopped\n", name, pid);
        send_with_payload(stdout, TTY_WRITE_LABEL, line.as_bytes())?;
    }
    Ok(())
}
