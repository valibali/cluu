#![no_std]
#![no_main]

extern crate alloc;

// Pull in the shared userspace runtime (panic handler + _start).
#[allow(unused_imports)]
use libcluu::runtime as _;

use libcluu::debug_print;
use libcluu::fs::VfsClient;
use libcluu::ipc::{send_with_payload, tty_write_sync, CONSOLE_WRITE_LABEL, TTY_WRITE_LABEL};
use libcluu::mem::PAGE_SIZE;
use libcluu::{process_info, registry, Error, Result, TOKEN_SPACE, TOKEN_STDLOG, TOKEN_STDOUT};

const GRANT_BASE: usize = 0x3FF0_0000;
const TARGET_PATH: &str = "/mnt/disk/hello";

#[no_mangle]
pub extern "C" fn main() -> i32 {
    if let Err(err) = run_demo() {
        let _ = send_with_payload_stdout(err.message());
        return 1;
    }
    0
}

fn run_demo() -> Result<()> {
    registry::init("vfs-demo")?;
    registry::register_default_outputs()?;

    let vfs_endpoint = registry::subscribe_output("vfs", "main")?;
    let client = VfsClient::new_from_registry(vfs_endpoint)?;

    let info = process_info();
    let space_token = info.tokens[TOKEN_SPACE];
    let _ = debug_print(&alloc::format!(
        "vfs-demo: tokens stdout={} stdlog={} space={}",
        info.tokens[TOKEN_STDOUT],
        info.tokens[TOKEN_STDLOG],
        space_token
    ));

    let file = client.open(TARGET_PATH)?;
    if file.size == 0 {
        return Ok(());
    }

    let grant = client.read_grant(file, 0, file.size, space_token, align_grant_base())?;
    let data =
        unsafe { core::slice::from_raw_parts((grant.base + grant.offset) as *const u8, grant.len) };
    send_with_payload_stdout_bytes(data)?;
    Ok(())
}

fn align_grant_base() -> usize {
    GRANT_BASE & !(PAGE_SIZE - 1)
}

fn send_with_payload_stdout(message: &str) -> Result<()> {
    send_with_payload_stdout_bytes(message.as_bytes())
}

fn send_with_payload_stdout_bytes(message: &[u8]) -> Result<()> {
    let info = process_info();
    let stdout = info.tokens[TOKEN_STDOUT];
    if stdout != 0 {
        // Use sync write to ensure output is flushed before we return
        if tty_write_sync(stdout, message).is_ok() {
            return Ok(());
        }
        // Fall back to async write if sync fails
        if send_with_payload(stdout, TTY_WRITE_LABEL, message).is_ok() {
            return Ok(());
        }
    }

    let stdlog = info.tokens[TOKEN_STDLOG];
    if stdlog != 0 && send_with_payload(stdlog, TTY_WRITE_LABEL, message).is_ok() {
        return Ok(());
    }

    if let Ok(console_write) = registry::subscribe_output("console:0", "write") {
        let _ = send_with_payload(console_write, CONSOLE_WRITE_LABEL, message);
        return Ok(());
    }
    Err(Error::InvalidState)
}
