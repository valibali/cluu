#![no_std]
#![no_main]

extern crate alloc;

use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_EXTRA_0, TOKEN_IPC, TOKEN_REGISTRY};
use libcluu::ipc::extract_reply_token;
use libcluu::time::{TIME_GETCLOCK, TIME_GETTIMEOFDAY};
use libcluu::types::Message;
use libcluu::{clock_now, debug_print, registry, Result};

const TICKS_PER_SEC_ASSUMED: u64 = 1_000_000_000;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    match run() {
        Ok(_) => 0,
        Err(_) => -1,
    }
}

fn run() -> Result<()> {
    let info = process_info();
    
    registry::init("timeserver")?;
    registry::register_output("main", info.tokens[TOKEN_EXTRA_0])?;
    debug_print("timeserver: ready")?;

    let endpoint = info.tokens[TOKEN_EXTRA_0];
    let clock_token = info.tokens[TOKEN_CLOCK];

    let mut buf = [0u8; 256];
    loop {
        let len = libcluu::syscall::ipc_recv(endpoint, &mut buf)?;
        if len < core::mem::size_of::<Message>() {
            continue;
        }

        let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
        let reply_token = extract_reply_token(&msg).unwrap_or(endpoint);

        match msg.tag.label {
            TIME_GETTIMEOFDAY => reply_time(reply_token, clock_token, false)?,
            TIME_GETCLOCK => reply_time(reply_token, clock_token, true)?,
            _ => {}
        }
    }
}

fn reply_time(reply_token: usize, clock_token: usize, monotonic: bool) -> Result<()> {
    let now = clock_now(clock_token).unwrap_or(0);
    let seconds = now / TICKS_PER_SEC_ASSUMED;
    let nanos = now % TICKS_PER_SEC_ASSUMED;

    let label = if monotonic { TIME_GETCLOCK } else { TIME_GETTIMEOFDAY };
    let mut reply = Message::new(label, [0; 6], 3);
    reply.words[0] = 0;
    reply.words[1] = seconds as usize;
    reply.words[2] = nanos as usize;
    libcluu::ipc::reply(reply_token, &reply, libcluu::IpcFlags::empty())
}
