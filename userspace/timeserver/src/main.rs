#![no_std]
#![no_main]

mod subscribers;

extern crate alloc;

use libcluu::boot::{process_info, TOKEN_CLOCK, TOKEN_EXTRA_0};
use libcluu::ipc::extract_reply_id;
use libcluu::time::{TIME_GETCLOCK, TIME_GETTIMEOFDAY, TIME_SUBSCRIBE_PERIODIC_LABEL, TIME_TICK_LABEL, TIME_UNSUBSCRIBE_LABEL};
use libcluu::types::Message;
use libcluu::ipc::parse_message;
use libcluu::{clock_frequency, clock_now, debug_print, registry, Result};

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
    let control_endpoint = registry::control_endpoint();

    let ticks_per_sec = match clock_frequency(clock_token) {
        Ok(hz) if hz > 0 => hz,
        _ => 1_000_000_000,
    };

    let mut subs = subscribers::SubscriberTable::new();
    let mut buf = [0u8; 256];
    let endpoints: [usize; 2] = [endpoint, control_endpoint];

    loop {
        let now_ms = monotonic_now_ms(clock_token, ticks_per_sec);
        let next_deadline = subs.next_deadline_ms();
        let timeout_ms = next_deadline.saturating_sub(now_ms);

        match libcluu::syscall::ipc_recv_any_with_sender(&endpoints, &mut buf, timeout_ms) {
            Ok((idx, len, sender_tid)) => {
                if len < core::mem::size_of::<Message>() {
                    // short message — fall through to tick firing
                } else if idx == 1 {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                } else {
                    let msg = unsafe { (buf.as_ptr() as *const Message).read_unaligned() };
                    let reply_token = extract_reply_id(&msg).unwrap_or(endpoint);
                    match msg.tag.label {
                        TIME_GETTIMEOFDAY => reply_time(reply_token, clock_token, ticks_per_sec, false)?,
                        TIME_GETCLOCK    => reply_time(reply_token, clock_token, ticks_per_sec, true)?,
                        TIME_SUBSCRIBE_PERIODIC_LABEL => {
                            let now_ms2 = monotonic_now_ms(clock_token, ticks_per_sec);
                            handle_subscribe(&mut subs, &msg, reply_token, now_ms2, sender_tid);
                        }
                        TIME_UNSUBSCRIBE_LABEL => {
                            handle_unsubscribe(&mut subs, &msg, reply_token, sender_tid);
                        }
                        _ => {}
                    }
                }
            }
            Err(libcluu::Error::Timeout) | Err(libcluu::Error::WouldBlock) => {
                // No message arrived before deadline — fall through to tick firing.
            }
            Err(_) => continue,
        }

        let now_ms = monotonic_now_ms(clock_token, ticks_per_sec);
        fire_due_ticks(&mut subs, now_ms);
    }
}

fn reply_time(
    reply_token: usize,
    clock_token: usize,
    ticks_per_sec: u64,
    monotonic: bool,
) -> Result<()> {
    let now = clock_now(clock_token).unwrap_or(0);
    let seconds = now / ticks_per_sec;
    let nanos = ((now % ticks_per_sec) as u128 * 1_000_000_000u128 / ticks_per_sec as u128) as u64;

    let label = if monotonic {
        TIME_GETCLOCK
    } else {
        TIME_GETTIMEOFDAY
    };
    let mut reply = Message::new(label, [0; 6], 3);
    reply.words[0] = 0;
    reply.words[1] = seconds as usize;
    reply.words[2] = nanos as usize;
    libcluu::ipc::reply(reply_token, &reply, libcluu::IpcFlags::empty())
}

fn monotonic_now_ms(clock_token: usize, ticks_per_sec: u64) -> u64 {
    let now = clock_now(clock_token).unwrap_or(0);
    (now * 1_000) / ticks_per_sec.max(1)
}

fn handle_subscribe(
    subs: &mut subscribers::SubscriberTable,
    msg: &Message,
    reply_token: usize,
    now_ms: u64,
    sender_tid: usize,
) {
    // Payload layout:
    //   words[0] = period_ms
    //   words[1] = notify_ep
    //
    // sender_tid comes from ipc_recv_any_with_sender (kernel-authenticated).
    // If sender_tid is 0 (unauthenticated legacy send), fall back to
    // words[2] as self-reported tid for callers that cannot use IPC_CALL.
    let period_ms = msg.words[0] as u32;
    let notify_ep = msg.words[1];
    let tid = if sender_tid != 0 { sender_tid } else { msg.words[2] };
    let err = match subs.insert(tid, notify_ep, period_ms, now_ms) {
        Ok(()) => 0u64,
        Err(e) => e,
    };
    let reply = Message::new(0, [err as usize, 0, 0, 0, 0, 0], 1);
    let _ = libcluu::ipc::reply(reply_token, &reply, libcluu::IpcFlags::empty());
    let _ = debug_print(&alloc::format!(
        "timeserver: subscribe tid={} period={}ms ep={} errno={}",
        tid, period_ms, notify_ep, err
    ));
}

fn handle_unsubscribe(
    subs: &mut subscribers::SubscriberTable,
    msg: &Message,
    reply_token: usize,
    sender_tid: usize,
) {
    // Use kernel-authenticated sender_tid; fall back to words[0] for legacy callers.
    let tid = if sender_tid != 0 { sender_tid } else { msg.words[0] };
    let err = subs.remove(tid).err().unwrap_or(0);
    let reply = Message::new(0, [err as usize, 0, 0, 0, 0, 0], 1);
    let _ = libcluu::ipc::reply(reply_token, &reply, libcluu::IpcFlags::empty());
}

fn fire_due_ticks(subs: &mut subscribers::SubscriberTable, now_ms: u64) {
    let mut to_remove: alloc::vec::Vec<usize> = alloc::vec::Vec::new();
    for s in subs.iter_due_mut(now_ms) {
        let msg = Message::new(
            TIME_TICK_LABEL,
            [s.tick_count_for_next() as usize, now_ms as usize, 0, 0, 0, 0],
            2,
        );
        let send_result = libcluu::ipc::send(s.notify_ep, &msg, libcluu::IpcFlags::empty());
        let should_remove = s.record_send(send_result.is_ok());
        s.advance_deadline(now_ms);
        if should_remove {
            to_remove.push(s.tid);
        }
    }
    for tid in to_remove {
        subs.remove_tid(tid);
        let _ = debug_print(&alloc::format!(
            "timeserver: subscriber tid={} removed (3x send fail)", tid
        ));
    }
}
