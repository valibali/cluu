//! Build-time constants threaded from the build env into runtime code.
//!
//! Single source of truth for env-driven knobs so multiple crates do not
//! drift apart. Currently only the shell-autostart command, which gates
//! procmgr's auto-login path and tty's wait-for-autologin flag.

pub const SHELL_AUTOSTART_CMD: &str = match option_env!("CLUU_SHELL_AUTOSTART_CMD") {
    Some(cmd) => cmd,
    None => "",
};

pub const HARNESS_AUTOLOGIN_ARMED: bool = !SHELL_AUTOSTART_CMD.is_empty();
