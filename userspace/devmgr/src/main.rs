#![no_std]
#![no_main]

// Device manager: owns block devices and grants block-region capability tokens
// to sessions at spawn time.

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::format;
use libcluu::boot::{process_info, TOKEN_EXTRA_0, TOKEN_EXTRA_1};
use libcluu::ipc::{
    extract_reply_id, parse_message, reply, DEVMGR_GRANT_REGION_LABEL,
    DEVMGR_REGISTER_LABEL, DEVMGR_REVOKE_LABEL,
};
use libcluu::registry;
use libcluu::rights::Rights;
use libcluu::syscall::{
    ipc_recv_any_with_sender, token_derive_scoped_block_region, token_revoke,
};
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Error, Result};

struct DeviceEntry {
    total_sectors: u64,
    root_token: usize,
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
    let endpoint = info.tokens[TOKEN_EXTRA_0];
    // TOKEN_EXTRA_1 holds the root BlockRegion token for device 0, if the
    // kernel/procmgr granted one at boot.  0 means no root token available
    // yet — GRANT_REGION will return an error until one is provided.
    let boot_root_block_token = info.tokens[TOKEN_EXTRA_1];

    registry::init("devmgr")?;
    registry::register_output("main", endpoint)?;
    debug_print("devmgr: ready")?;

    let control_endpoint = registry::control_endpoint();
    let mut devices: BTreeMap<u32, DeviceEntry> = BTreeMap::new();
    let mut buf = [0u8; 256];

    loop {
        let tokens = [endpoint, control_endpoint];
        match ipc_recv_any_with_sender(&tokens, &mut buf, u64::MAX) {
            Ok((idx, len, _sender_tid)) => {
                if len < core::mem::size_of::<Message>() {
                    continue;
                }
                if idx == 1 {
                    if let Some((msg, payload)) = parse_message(&buf[..len]) {
                        let _ = registry::handle_incoming_message(&msg, payload);
                    }
                    continue;
                }
                let msg = unsafe { &*(buf.as_ptr() as *const Message) };
                let reply_token = extract_reply_id(msg);
                match msg.tag.label {
                    DEVMGR_REGISTER_LABEL => {
                        handle_register(&mut devices, msg, reply_token, boot_root_block_token);
                    }
                    DEVMGR_GRANT_REGION_LABEL => {
                        handle_grant_region(&devices, msg, reply_token);
                    }
                    DEVMGR_REVOKE_LABEL => {
                        handle_revoke(msg, reply_token);
                    }
                    _ => {}
                }
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

fn handle_register(
    devices: &mut BTreeMap<u32, DeviceEntry>,
    msg: &Message,
    reply_token: Option<usize>,
    boot_root_block_token: usize,
) {
    let device_id = msg.words[0] as u32;
    let total_sectors = msg.words[1] as u64;

    // If this is device 0 and we have a boot-time root BlockRegion token,
    // attach it.  Other devices get root_token = 0 until a kernel-side
    // mint path exists for per-device root tokens.
    let root_token = if device_id == 0 {
        boot_root_block_token
    } else {
        0
    };

    devices.insert(device_id, DeviceEntry {
        total_sectors,
        root_token,
    });

    let _ = debug_print(&format!(
        "devmgr: registered device {} ({} sectors, root_token={})",
        device_id, total_sectors, root_token
    ));

    let reply_msg = Message::new(DEVMGR_REGISTER_LABEL, [0, 0, 0, 0, 0, 0], 1);
    if let Some(token) = reply_token {
        let _ = reply(token, &reply_msg, IpcFlags::empty());
    }
}

fn handle_grant_region(
    devices: &BTreeMap<u32, DeviceEntry>,
    msg: &Message,
    reply_token: Option<usize>,
) {
    let device_id = msg.words[0] as u32;
    let start_sector = msg.words[1] as u64;
    let sector_count_arg = msg.words[2] as u64;

    let (status, token_handle) = match devices.get(&device_id) {
        Some(dev) => {
            if dev.root_token == 0 {
                let _ = debug_print(&format!(
                    "devmgr: GRANT_REGION device {} — no root BlockRegion token",
                    device_id
                ));
                (Error::NotFound as i32 as usize, 0usize)
            } else {
                let sector_count = if sector_count_arg == 0 {
                    dev.total_sectors
                } else {
                    sector_count_arg
                };
                let rights = (Rights::READ | Rights::WRITE).bits() as usize;
                match token_derive_scoped_block_region(
                    dev.root_token,
                    rights as u32,
                    0,
                    start_sector,
                    sector_count,
                ) {
                    Ok(handle) => {
                        let _ = debug_print(&format!(
                            "devmgr: GRANT_REGION device {} start={} count={} → handle {}",
                            device_id, start_sector, sector_count, handle
                        ));
                        (0usize, handle)
                    }
                    Err(e) => {
                        let _ = debug_print(&format!(
                            "devmgr: GRANT_REGION derive failed {:?}", e
                        ));
                        (e as i32 as usize, 0usize)
                    }
                }
            }
        }
        None => {
            let _ = debug_print(&format!(
                "devmgr: GRANT_REGION unknown device {}",
                device_id
            ));
            (Error::NotFound as i32 as usize, 0usize)
        }
    };

    let reply_msg = Message::new(
        DEVMGR_GRANT_REGION_LABEL,
        [status, token_handle, 0, 0, 0, 0],
        2,
    );
    if let Some(token) = reply_token {
        let _ = reply(token, &reply_msg, IpcFlags::empty());
    }
}

fn handle_revoke(msg: &Message, reply_token: Option<usize>) {
    let token_handle = msg.words[0];
    let status = match token_revoke(token_handle) {
        Ok(_) => 0usize,
        Err(e) => e as i32 as usize,
    };
    let reply_msg = Message::new(DEVMGR_REVOKE_LABEL, [status, 0, 0, 0, 0, 0], 1);
    if let Some(token) = reply_token {
        let _ = reply(token, &reply_msg, IpcFlags::empty());
    }
}
