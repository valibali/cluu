//! `su` and `sudo` builtins.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use libcluu::boot::{process_info, TOKEN_STDIN};
use libcluu::ipc::{
    recv, send_with_payload, call_with_payload,
    PROCMGR_ESCALATE_LABEL, PROCMGR_SU_LABEL,
    TTY_FG_FLAG_FORWARD_CTRL_C, TTY_READ_LABEL,
};
use libcluu::syscall;
use libcluu::types::Message;
use libcluu::{debug_print, IpcFlags, Result, TOKEN_IPC};

use crate::commands::exec::set_tty_foreground;
use super::registry::CommandContext;
use super::registry::{BuiltinCommand, BuiltinRegistry};

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(SuBuiltin));
    registry.register(Box::new(SudoBuiltin));
}

// ---------------------------------------------------------------------------
// sudo
// ---------------------------------------------------------------------------

pub(crate) struct SudoBuiltin;

impl BuiltinCommand for SudoBuiltin {
    fn name(&self) -> &'static str {
        "sudo"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let command_path = if args.is_empty() || (args.len() == 1 && args[0] == "-s") {
            "/bin/shell"
        } else {
            args[0].as_str()
        };

        let password = "";

        let mut payload = Vec::new();
        payload.extend_from_slice(password.as_bytes());
        payload.push(0);
        payload.extend_from_slice(command_path.as_bytes());
        payload.push(0);

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;

        let mut msg = Message::new(PROCMGR_ESCALATE_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = notify_endpoint;
        let mut reply = Message::new(0, [0; 6], 0);

        call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

        let status = reply.words[0];
        if status != 0 {
            let line = format!("sudo: permission denied (error {})\n", status);
            crate::write_stdout(line.as_bytes());
            return Ok(());
        }

        let pid = reply.words[1];
        let cid = reply.words[4];
        let child_stdin = reply.words[3];

        let _ = debug_print(&format!("sudo: escalated cmd={} pid={} cid={}", command_path, pid, cid));

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
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// su
// ---------------------------------------------------------------------------

pub(crate) struct SuBuiltin;

impl BuiltinCommand for SuBuiltin {
    fn name(&self) -> &'static str {
        "su"
    }

    fn run(&self, stdout: usize, context: &mut CommandContext, args: &[String]) -> Result<()> {
        let username = match args.first() {
            Some(u) => u.as_str(),
            None => {
                crate::write_stdout(b"usage: su <username> [-c <command>]\n");
                return Ok(());
            }
        };

        let inline_command: Option<String> = match args.get(1).map(|s| s.as_str()) {
            Some("-c") => {
                if args.len() < 3 {
                    crate::write_stdout(b"usage: su <username> -c <command>\n");
                    return Ok(());
                }
                Some(args[2..].join(" "))
            }
            _ => None,
        };

        let password = "";

        let mut payload = Vec::new();
        payload.extend_from_slice(username.as_bytes());
        payload.push(0);
        payload.extend_from_slice(password.as_bytes());
        payload.push(0);

        let procmgr_endpoint = context.procmgr_spawn_endpoint()?;
        let notify_endpoint = syscall::endpoint_create(process_info().tokens[TOKEN_IPC])?;

        let mut msg = Message::new(PROCMGR_SU_LABEL, [0; 6], 2);
        msg.words[0] = payload.len();
        msg.words[1] = notify_endpoint;
        let mut reply = Message::new(0, [0; 6], 0);

        call_with_payload(procmgr_endpoint, &msg, &payload, &mut reply)?;

        let status = reply.words[0];
        if status != 0 {
            let line = format!("su: authentication failure (error {})\n", status);
            crate::write_stdout(line.as_bytes());
            return Ok(());
        }

        let pid = reply.words[1];
        let cid = reply.words[4];
        let child_stdin = reply.words[3];

        let _ = debug_print(&format!(
            "su: nested session user={} pid={} cid={}",
            username, pid, cid
        ));

        if let Some(cmd) = inline_command {
            if child_stdin != 0 {
                let mut line = cmd.clone();
                line.push('\n');
                let _ = send_with_payload(child_stdin, TTY_READ_LABEL, line.as_bytes());
                let _ = send_with_payload(child_stdin, TTY_READ_LABEL, b"exit\n");
            }
            let mut notify_msg = Message::new(0, [0; 6], 0);
            let _ = recv(notify_endpoint, &mut notify_msg, IpcFlags::empty());
            return Ok(());
        }

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
        Ok(())
    }
}
