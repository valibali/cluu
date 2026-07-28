#![no_std]
#![no_main]

//! drivermgr — CLUU driver manager.
//!
//! Enumerates PCI + ACPI devices, builds a device tree, binds drivers to
//! devices via bind rules, and spawns driver processes via procmgr.
//!
//! Phase D1: registry init + PCI/ACPI scan + recv loop answering
//! `/proc/devices` queries from VFS.
//! Phase D2: reads `[driver]` sections from container manifests, builds
//! a `BindRuleTable`, matches devices in observe mode (no spawn).
//!
//! Architecture (SOLID):
//! - `device_tree.rs` — domain types (DeviceNode, DeviceTree). No deps.
//! - `pci_scan.rs`    — PCI bus enumeration (D1.2).
//! - `acpi_scan.rs`   — ACPI RSDP/FADT/MCFG discovery (D1.3).
//! - `bind_rules.rs`  — BindRule, BindRuleTable, manifest parsing (D2.5).
//! - `main.rs`        — orchestration: init, scan, bind, recv, dispatch.

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

mod acpi_scan;
mod bind_rules;
mod device_tree;
mod pci_scan;
mod spawn;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use alloc::collections::BTreeSet;

use libcluu::boot::{
    process_info, INITRD_USER_BASE, PARAM_INITRD_SIZE, TOKEN_EXTRA_0, TOKEN_EXTRA_1, TOKEN_SPACE,
    TOKEN_VFS_VIEW_MGR,
};
use libcluu::fs::client::VfsClient;
use libcluu::ipc::{
    parse_message, reply_to_sender, reply_to_sender_with_payload, send_msg_with_payload,
    DRIVERMGR_DEVICE_STATE_LABEL, DRIVERMGR_PROBE_LABEL, DRIVERMGR_QUERY_DEVICE_LABEL, DRIVERMGR_QUERY_DEVICES_LABEL,
    DRIVERMGR_RESPAWN_DEVICE_LABEL, VFS_SET_VIEW_LABEL,
};
use libcluu::registry;
use libcluu::syscall::ipc_recv_any_with_sender;
use libcluu::tar::find_member;
use libcluu::types::{IpcFlags, Message};
use libcluu::{debug_print, yield_cpu, Result};

use bind_rules::{build_rule_from_manifest, BindRuleTable};
use device_tree::{query_all, query_device, DeviceState, DeviceTree};

const REPLY_OK: usize = 0;
const REPLY_NOT_FOUND: usize = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SpawnMode {
    Observe,
    Spawn,
    Hybrid,
}

impl SpawnMode {
    fn should_spawn(self) -> bool {
        matches!(self, SpawnMode::Spawn | SpawnMode::Hybrid)
    }
}

static DEVICE_TREE: spin::Mutex<DeviceTree> = spin::Mutex::new(DeviceTree::new());

static MATCHED_DEVICES: spin::Mutex<BTreeSet<String>> = spin::Mutex::new(BTreeSet::new());

static BIND_RULES: spin::Mutex<BindRuleTable> = spin::Mutex::new(BindRuleTable::new());

static PROCMGR_EP: spin::Mutex<usize> = spin::Mutex::new(0);
static DRIVERMON_EP: spin::Mutex<usize> = spin::Mutex::new(0);
static DRIVERMON_NOTIFY_EP: spin::Mutex<usize> = spin::Mutex::new(0);
static DRIVERMON_FAULT_EP: spin::Mutex<usize> = spin::Mutex::new(0);

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
    let pci_token = info.tokens[TOKEN_EXTRA_1];
    let space_token = info.tokens[TOKEN_SPACE];

    registry::init("drivermgr")?;
    registry::register_output("main", endpoint)?;
    let _ = debug_print("drivermgr: ready");

    if let Err(err) = pci_scan::scan(pci_token, &mut *DEVICE_TREE.lock()) {
        let _ = debug_print(&format!("drivermgr: PCI scan error {:?}", err));
    }
    if let Err(err) = acpi_scan::scan(space_token, &mut *DEVICE_TREE.lock()) {
        let _ = debug_print(&format!("drivermgr: ACPI scan error {:?}", err));
    }

    let view_mgr_token = info.tokens[TOKEN_VFS_VIEW_MGR];

    let initrd_size = info.params[PARAM_INITRD_SIZE] as usize;
    let initrd: Option<&[u8]> = if initrd_size > 0 {
        Some(unsafe { core::slice::from_raw_parts(INITRD_USER_BASE as *const u8, initrd_size) })
    } else {
        None
    };

    let spawn_mode = parse_spawn_mode_from_initrd(initrd);

    if spawn_mode.should_spawn() {
        let phase1 = load_initrd_bind_rules(initrd);
        let _ = debug_print(&format!(
            "drivermgr: phase 1 — {} initrd bind rules (mode={:?})",
            phase1.len(),
            spawn_mode
        ));
        {
            let mut cached = BIND_RULES.lock();
            for rule in phase1.iter() {
                cached.add(rule.clone());
            }
        }
        match_devices(&phase1, spawn_mode);

        // Phase 2: wait for VFS mounted, then load userdisk manifests.
        // Subscribe to vfs:mounted — blocks until VFS publishes mounted
        // (which happens after blkdev registers, which happens after
        // virtio-blk spawns and initializes in phase 1).
        if let Err(e) = registry::subscribe_output("vfs", "mounted") {
            let _ = debug_print(&format!("drivermgr: vfs mounted subscribe failed: {:?}", e));
            return Ok(());
        }
        let vfs_ep = match registry::subscribe_output("vfs", "main") {
            Ok(ep) => ep,
            Err(e) => {
                let _ = debug_print(&format!("drivermgr: vfs main subscribe failed: {:?}", e));
                return Ok(());
            }
        };
        if view_mgr_token != 0 {
            if let Err(e) = register_vfs_view(vfs_ep, view_mgr_token) {
                let _ = debug_print(&format!("drivermgr: vfs view registration failed: {:?}", e));
            }
        }

        let phase2 = load_userdisk_bind_rules(space_token, view_mgr_token);
        let _ = debug_print(&format!(
            "drivermgr: phase 2 — {} userdisk bind rules",
            phase2.len()
        ));
        {
            let mut cached = BIND_RULES.lock();
            for rule in phase2.iter() {
                cached.add(rule.clone());
            }
            cached.sort_by_priority();
        }
        match_devices(&phase2, spawn_mode);
    } else {
        let (table, mode) = load_bind_rules(space_token, view_mgr_token);
        let _ = debug_print(&format!(
            "drivermgr: loaded {} bind rules (mode={:?})",
            table.len(),
            mode
        ));
        match_devices(&table, mode);
    }

    let control_endpoint = registry::control_endpoint();
    let mut buf = [0u8; 512];

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
                let label = msg.tag.label;
                if label == DRIVERMGR_RESPAWN_DEVICE_LABEL {
                    handle_respawn_device(&buf[..len]);
                    continue;
                }
                if label == DRIVERMGR_DEVICE_STATE_LABEL {
                    handle_device_state(&buf[..len]);
                    continue;
                }
                if label == DRIVERMGR_PROBE_LABEL {
                    handle_probe(msg, &buf[..len], endpoint);
                    continue;
                }
                if !handle_query(label, msg, &buf[..len], endpoint) {
                    let _ = debug_print(&alloc::format!(
                        "drivermgr: recv label=0x{:x}",
                        label
                    ));
                    let reply_msg = Message::new(label, [REPLY_OK, 0, 0, 0, 0, 0], 1);
                    let _ = reply_to_sender(msg, &reply_msg, endpoint, IpcFlags::empty());
                }
            }
            Err(libcluu::Error::WouldBlock) => {
                let _ = yield_cpu();
            }
            Err(_) => {
                let _ = yield_cpu();
            }
        }
    }
}

/// Load bind rules from container manifests on the userdisk.
///
/// Blocks until VFS publishes "mounted" (userdisk is accessible), then
/// reads `/var/images/<name>/manifest.toml` for each container directory.
/// Manifests without a `[driver]` section are skipped silently. Manifests
/// that can't be read or parsed are logged as warnings and skipped.
fn load_bind_rules(space_token: usize, view_mgr_token: usize) -> (BindRuleTable, SpawnMode) {
    let mut table = BindRuleTable::new();
    let mut spawn_mode = SpawnMode::Observe;

    if let Err(e) = registry::subscribe_output("vfs", "mounted") {
        let _ = debug_print(&format!("drivermgr: vfs mounted subscribe failed: {:?}", e));
        return (table, spawn_mode);
    }

    let vfs_ep = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(e) => {
            let _ = debug_print(&format!("drivermgr: vfs main subscribe failed: {:?}", e));
            return (table, spawn_mode);
        }
    };

    if view_mgr_token != 0 {
        if let Err(e) = register_vfs_view(vfs_ep, view_mgr_token) {
            let _ = debug_print(&format!("drivermgr: vfs view registration failed: {:?}", e));
        }
    }

    let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());

    spawn_mode = read_spawn_mode(&vfs, space_token);

    let entries = match vfs.readdir("/var/images/") {
        Ok(e) => e,
        Err(e) => {
            let _ = debug_print(&format!("drivermgr: readdir /var/images/ failed: {:?}", e));
            return (table, spawn_mode);
        }
    };

    for entry in &entries {
        if !entry.is_dir {
            continue;
        }
        let name = &entry.name;
        if name == "." || name == ".." {
            continue;
        }
        let manifest_path = format!("/var/images/{}/manifest.toml", name);
        match read_manifest(&vfs, &manifest_path, space_token) {
            Ok(content) => {
                let doc = match libcluu::toml::parse(&content) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = debug_print(&format!(
                            "drivermgr: warning: parse {} failed at line {}: {}",
                            manifest_path, e.line, e.msg
                        ));
                        continue;
                    }
                };
                if doc.table("driver").is_none() {
                    continue;
                }
                match build_rule_from_manifest(name, &doc) {
                    Some(rule) => table.add(rule),
                    None => {
                        let _ = debug_print(&format!(
                            "drivermgr: warning: no [[driver.bind]] in {}",
                            manifest_path
                        ));
                    }
                }
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "drivermgr: warning: read {} failed: {:?}",
                    manifest_path, e
                ));
            }
        }
    }

    table.sort_by_priority();
    (table, spawn_mode)
}

fn read_spawn_mode(vfs: &VfsClient, space_token: usize) -> SpawnMode {
    match read_manifest(vfs, "/etc/drivermgr.toml", space_token) {
        Ok(content) => {
            if let Ok(doc) = libcluu::toml::parse(&content) {
                if let Some(t) = doc.tables.first() {
                    if let Some(s) = t.get_str("spawn_mode") {
                        return match s {
                            "spawn" => SpawnMode::Spawn,
                            "hybrid" => SpawnMode::Hybrid,
                            _ => SpawnMode::Observe,
                        };
                    }
                }
            }
            SpawnMode::Observe
        }
        Err(_) => SpawnMode::Observe,
    }
}

/// Read spawn_mode from the initrd directly (D3.6 two-phase boot).
///
/// Used in spawn mode before VFS is available — `etc/drivermgr.toml` is
/// pulled out of the initrd archive via `find_member` and parsed inline.
/// Defaults to `SpawnMode::Observe` on any error (missing initrd, missing
/// member, bad UTF-8, parse error, missing `[drivermgr]` table, unknown
/// mode value).
fn parse_spawn_mode_from_initrd(initrd: Option<&[u8]>) -> SpawnMode {
    let data = match initrd {
        Some(d) => d,
        None => return SpawnMode::Observe,
    };
    let config = match find_member(data, "etc/drivermgr.toml") {
        Some(c) => c,
        None => return SpawnMode::Observe,
    };
    let text = match core::str::from_utf8(config) {
        Ok(s) => s,
        Err(_) => return SpawnMode::Observe,
    };
    let doc = match libcluu::toml::parse(text) {
        Ok(d) => d,
        Err(_) => return SpawnMode::Observe,
    };
    if let Some(t) = doc.table("drivermgr") {
        if let Some(s) = t.get_str("spawn_mode") {
            return match s {
                "spawn" => SpawnMode::Spawn,
                "hybrid" => SpawnMode::Hybrid,
                _ => SpawnMode::Observe,
            };
        }
    }
    SpawnMode::Observe
}

/// Load bind rules from initrd manifests for phase 1 of two-phase boot.
///
/// Reads `sys/<name>.manifest.toml` from the initrd archive for each known
/// driver program. Only manifests with a `[driver]` section and an
/// `initrd_path` in `[driver.source]` are included — these are the drivers
/// that can be spawned from initrd before userdisk is mounted.
fn load_initrd_bind_rules(initrd: Option<&[u8]>) -> BindRuleTable {
    let mut table = BindRuleTable::new();
    let initrd = match initrd {
        Some(d) => d,
        None => return table,
    };
    let driver_names = [
        "virtio-blk",
        "virtio-net",
        "virtio-9p",
        "virtio-snd",
        "virtio-gpu",
        "usb-input",
        "kbd",
        "mouse",
    ];
    for name in &driver_names {
        let manifest_path = format!("sys/{}.manifest.toml", name);
        let manifest_data = match find_member(initrd, &manifest_path) {
            Some(d) => d,
            None => continue,
        };
        let text = match core::str::from_utf8(manifest_data) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let doc = match libcluu::toml::parse(text) {
            Ok(d) => d,
            Err(e) => {
                let _ = debug_print(&format!(
                    "drivermgr: warning: parse {} failed at line {}: {}",
                    manifest_path, e.line, e.msg
                ));
                continue;
            }
        };
        if doc.table("driver").is_none() {
            continue;
        }
        match build_rule_from_manifest(name, &doc) {
            Some(rule) => {
                if rule.source_initrd_path.is_some() {
                    table.add(rule);
                }
            }
            None => {
                let _ = debug_print(&format!(
                    "drivermgr: warning: no [[driver.bind]] in {}",
                    manifest_path
                ));
            }
        }
    }
    table.sort_by_priority();
    table
}

/// Load bind rules from userdisk manifests for phase 2 of two-phase boot.
///
/// Assumes the caller has already subscribed to `vfs:mounted` (blocking
/// until VFS publishes mounted) and registered the VFS view. This function
/// subscribes to `vfs:main` (idempotent if the caller already did) and
/// reads `/var/images/<name>/manifest.toml` for each container directory.
fn load_userdisk_bind_rules(space_token: usize, view_mgr_token: usize) -> BindRuleTable {
    let _ = view_mgr_token;
    let mut table = BindRuleTable::new();

    let vfs_ep = match registry::subscribe_output("vfs", "main") {
        Ok(ep) => ep,
        Err(e) => {
            let _ = debug_print(&format!("drivermgr: vfs main subscribe failed: {:?}", e));
            return table;
        }
    };

    let vfs = VfsClient::new(vfs_ep, registry::control_endpoint());

    let entries = match vfs.readdir("/var/images/") {
        Ok(e) => e,
        Err(e) => {
            let _ = debug_print(&format!("drivermgr: readdir /var/images/ failed: {:?}", e));
            return table;
        }
    };

    for entry in &entries {
        if !entry.is_dir {
            continue;
        }
        let name = &entry.name;
        if name == "." || name == ".." {
            continue;
        }
        let manifest_path = format!("/var/images/{}/manifest.toml", name);
        match read_manifest(&vfs, &manifest_path, space_token) {
            Ok(content) => {
                let doc = match libcluu::toml::parse(&content) {
                    Ok(d) => d,
                    Err(e) => {
                        let _ = debug_print(&format!(
                            "drivermgr: warning: parse {} failed at line {}: {}",
                            manifest_path, e.line, e.msg
                        ));
                        continue;
                    }
                };
                if doc.table("driver").is_none() {
                    continue;
                }
                match build_rule_from_manifest(name, &doc) {
                    Some(rule) => table.add(rule),
                    None => {
                        let _ = debug_print(&format!(
                            "drivermgr: warning: no [[driver.bind]] in {}",
                            manifest_path
                        ));
                    }
                }
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "drivermgr: warning: read {} failed: {:?}",
                    manifest_path, e
                ));
            }
        }
    }

    table.sort_by_priority();
    table
}

fn register_vfs_view(vfs_ep: usize, view_mgr_token: usize) -> Result<()> {
    let mut payload = Vec::new();
    let src = b"/";
    let dst = b"/";
    payload.extend_from_slice(&(src.len() as u16).to_le_bytes());
    payload.extend_from_slice(&(dst.len() as u16).to_le_bytes());
    payload.push(1u8);
    payload.extend_from_slice(&0u64.to_le_bytes());
    payload.extend_from_slice(src);
    payload.extend_from_slice(dst);

    let mut msg = Message::new(VFS_SET_VIEW_LABEL, [0; 6], 6);
    msg.words[0] = payload.len();
    msg.words[1] = 0;
    msg.words[2] = 1;
    msg.words[3] = 0;
    msg.words[4] = 0;
    msg.words[5] = view_mgr_token;
    send_msg_with_payload(vfs_ep, &msg, &payload)
}

/// Read a manifest file into a string. Allocates a scratch buffer via
/// VSPACE, reads via grant, copies into a Vec, frees the buffer.
fn read_manifest(vfs: &VfsClient, path: &str, space_token: usize) -> Result<String> {
    let file = vfs.open(path)?;
    let total = file.size;
    if total == 0 {
        let _ = vfs.close(file);
        return Ok(String::new());
    }

    let alloc_size = ((total.min(64 * 1024)) + 4095) & !4095;
    let scratch_base = libcluu::vspace::VSPACE
        .lock()
        .alloc(alloc_size)
        .map_err(|_| libcluu::Error::OutOfMemory)?;

    let mut raw: Vec<u8> = Vec::new();
    let mut offset = 0usize;
    let mut ok = true;

    while offset < total {
        let want = (total - offset).min(64 * 1024);
        match vfs.read_grant(file, offset, want, space_token, scratch_base) {
            Ok(grant) => {
                if grant.len == 0 {
                    break;
                }
                let slice = unsafe {
                    core::slice::from_raw_parts(scratch_base as *const u8, grant.len)
                };
                raw.extend_from_slice(slice);
                offset += grant.len;
            }
            Err(_) => {
                ok = false;
                break;
            }
        }
    }

    let _ = libcluu::vspace::VSPACE.lock().free(scratch_base, alloc_size);
    let _ = vfs.close(file);

    if !ok {
        return Err(libcluu::Error::InvalidOperation);
    }

    core::str::from_utf8(&raw)
        .map(String::from)
        .map_err(|_| libcluu::Error::InvalidArgument)
}

/// Walk the device tree and log each match or non-match against the
/// bind rule table. In observe mode (D2.6) only logs. In spawn/hybrid
/// mode (D3.2) calls `spawn::spawn_driver` for each matched device.
fn match_devices(table: &BindRuleTable, spawn_mode: SpawnMode) {
    let procmgr_ep = if spawn_mode.should_spawn() {
        match registry::subscribe_output("root-procmgr", "spawn") {
            Ok(ep) => Some(ep),
            Err(e) => {
                let _ = debug_print(&format!(
                    "drivermgr: procmgr:spawn subscribe failed {:?} — spawn disabled",
                    e
                ));
                None
            }
        }
    } else {
        None
    };
    let drivermon_ep = if spawn_mode.should_spawn() {
        match registry::subscribe_output("drivermon", "main") {
            Ok(ep) => Some(ep),
            Err(e) => {
                let _ = debug_print(&format!(
                    "drivermgr: drivermon:main subscribe failed {:?} — REGISTER disabled",
                    e
                ));
                None
            }
        }
    } else {
        None
    };
    let drivermon_notify_ep = if spawn_mode.should_spawn() {
        match registry::subscribe_output("drivermon", "notify") {
            Ok(ep) => Some(ep),
            Err(e) => {
                let _ = debug_print(&format!(
                    "drivermgr: drivermon:notify subscribe failed {:?} — exit-notify disabled",
                    e
                ));
                None
            }
        }
    } else {
        None
    };
    let drivermon_fault_ep = if spawn_mode.should_spawn() {
        match registry::subscribe_output("drivermon", "fault") {
            Ok(ep) => Some(ep),
            Err(e) => {
                let _ = debug_print(&format!(
                    "drivermgr: drivermon:fault subscribe failed {:?} — fault-IPC disabled",
                    e
                ));
                None
            }
        }
    } else {
        None
    };
    if let Some(pe) = procmgr_ep {
        *PROCMGR_EP.lock() = pe;
    }
    if let Some(de) = drivermon_ep {
        *DRIVERMON_EP.lock() = de;
    }
    if let Some(ne) = drivermon_notify_ep {
        *DRIVERMON_NOTIFY_EP.lock() = ne;
    }
    if let Some(fe) = drivermon_fault_ep {
        *DRIVERMON_FAULT_EP.lock() = fe;
    }

    let tree = DEVICE_TREE.lock();
    for node in tree.values() {
        match table.match_device(node) {
            Some(rule) => {
                if spawn_mode.should_spawn() {
                    let already_matched = MATCHED_DEVICES.lock().contains(&node.path);
                    if already_matched {
                        continue;
                    }
                }
                let _ = debug_print(&format!(
                    "drivermgr: device {} matched driver {} (priority {})",
                    node.path, rule.driver_name, rule.priority
                ));
                if spawn_mode.should_spawn() {
                    if let (Some(pe), Some(de)) = (procmgr_ep, drivermon_ep) {
                        let notify_ep = drivermon_notify_ep.unwrap_or(0);
                        let fault_ep = drivermon_fault_ep.unwrap_or(0);
                        if let Err(e) = spawn::spawn_driver(rule, node, pe, de, notify_ep, fault_ep) {
                            let _ = debug_print(&format!(
                                "drivermgr: spawn_driver error {:?} — device {} stays unbound",
                                e, node.path
                            ));
                        } else {
                            MATCHED_DEVICES.lock().insert(node.path.clone());
                        }
                    } else {
                        let _ = debug_print(&format!(
                            "drivermgr: spawn mode active but endpoints unavailable — {} stays unbound",
                            node.path
                        ));
                    }
                }
            }
            None => {
                let _ = debug_print(&format!(
                    "drivermgr: device {} no matching driver",
                    node.path
                ));
            }
        }
    }
}

/// Handle a `/proc/devices` query IPC.  Returns true if `label` was a
/// recognised drivermgr query (and a reply was sent), false otherwise so
/// the caller can fall through to its generic reply.
///
/// Reply convention: `reply_to_sender_with_payload` overwrites
/// `words[0]` with the payload length, so the status code lives in
/// `words[1]` (REPLY_OK on success, REPLY_NOT_FOUND when the requested
/// path does not exist).
fn handle_query(label: u32, msg: &Message, full_buf: &[u8], endpoint: usize) -> bool {
    if label == DRIVERMGR_QUERY_DEVICES_LABEL {
        let body = {
            let tree = DEVICE_TREE.lock();
            query_all(&*tree)
        };
        let reply = Message::new(
            DRIVERMGR_QUERY_DEVICES_LABEL,
            [0, REPLY_OK, 0, 0, 0, 0],
            1,
        );
        let _ = reply_to_sender_with_payload(msg, &reply, body.as_bytes(), endpoint);
        return true;
    }
    if label == DRIVERMGR_QUERY_DEVICE_LABEL {
        let payload_start = core::mem::size_of::<Message>();
        let path_bytes: &[u8] = if full_buf.len() > payload_start {
            &full_buf[payload_start..]
        } else {
            &[]
        };
        let path = match core::str::from_utf8(path_bytes) {
            Ok(s) => s.trim_start_matches('/'),
            Err(_) => {
                let reply = Message::new(
                    DRIVERMGR_QUERY_DEVICE_LABEL,
                    [0, REPLY_NOT_FOUND, 0, 0, 0, 0],
                    1,
                );
                let _ = reply_to_sender_with_payload(msg, &reply, &[], endpoint);
                return true;
            }
        };
        let canonical = if path.starts_with("pci/") || path.starts_with("acpi/") {
            format!("/{}", path)
        } else {
            String::from(path)
        };
        let body = {
            let tree = DEVICE_TREE.lock();
            match tree.get(&canonical) {
                Some(node) => query_device(node),
                None => String::new(),
            }
        };
        let (status, payload) = if body.is_empty() {
            (REPLY_NOT_FOUND, &[][..])
        } else {
            (REPLY_OK, body.as_bytes())
        };
        let reply = Message::new(
            DRIVERMGR_QUERY_DEVICE_LABEL,
            [0, status, 0, 0, 0, 0],
            1,
        );
        let _ = reply_to_sender_with_payload(msg, &reply, payload, endpoint);
        return true;
    }
    false
}

/// Handle DRIVERMGR_RESPAWN_DEVICE_LABEL: drivermon requests respawn of
/// a driver for a device. Looks up the BindRule by device_path, finds
/// the DeviceNode, and calls spawn_driver. Fire-and-forget — no reply.
fn handle_respawn_device(full_buf: &[u8]) {
    let payload_start = core::mem::size_of::<Message>();
    let payload: &[u8] = if full_buf.len() > payload_start {
        &full_buf[payload_start..]
    } else {
        &[]
    };
    let device_path = match parse_first_nul(payload) {
        Some(s) => s,
        None => {
            let _ = debug_print("drivermgr: RESPAWN_DEVICE missing device_path");
            return;
        }
    };
    let driver_image_override = parse_second_segment(payload);

    let procmgr_ep = *PROCMGR_EP.lock();
    let drivermon_ep = *DRIVERMON_EP.lock();
    let drivermon_notify_ep = *DRIVERMON_NOTIFY_EP.lock();
    let drivermon_fault_ep = *DRIVERMON_FAULT_EP.lock();
    if procmgr_ep == 0 || drivermon_ep == 0 {
        let _ = debug_print(&format!(
            "drivermgr: RESPAWN_DEVICE {} — endpoints not cached, cannot respawn",
            device_path
        ));
        return;
    }

    let node_clone = {
        let tree = DEVICE_TREE.lock();
        tree.get(&device_path).cloned()
    };
    let node = match node_clone {
        Some(n) => n,
        None => {
            let _ = debug_print(&format!(
                "drivermgr: RESPAWN_DEVICE {} — device not in tree",
                device_path
            ));
            return;
        }
    };

    let rule = {
        let rules = BIND_RULES.lock();
        rules.match_device(&node).cloned()
    };
    let mut rule = match rule {
        Some(r) => r,
        None => {
            let _ = debug_print(&format!(
                "drivermgr: RESPAWN_DEVICE {} — no matching bind rule",
                device_path
            ));
            return;
        }
    };
    if let Some(img) = driver_image_override {
        rule.driver_name = img;
        rule.source_initrd_path = None;
    }

    let _ = debug_print(&format!(
        "drivermgr: RESPAWN_DEVICE {} driver={}",
        device_path, rule.driver_name
    ));
    if let Err(e) = spawn::spawn_driver(&rule, &node, procmgr_ep, drivermon_ep, drivermon_notify_ep, drivermon_fault_ep) {
        let _ = debug_print(&format!(
            "drivermgr: RESPAWN_DEVICE spawn failed {:?} — device {} stays unbound",
            e, device_path
        ));
    }
}

fn parse_first_nul(payload: &[u8]) -> Option<String> {
    let end = payload.iter().position(|b| *b == 0).unwrap_or(payload.len());
    if end == 0 {
        return None;
    }
    core::str::from_utf8(&payload[..end]).ok().map(String::from)
}

fn parse_second_segment(payload: &[u8]) -> Option<String> {
    let first_end = payload.iter().position(|b| *b == 0)? + 1;
    if first_end >= payload.len() {
        return None;
    }
    let rest = &payload[first_end..];
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    core::str::from_utf8(&rest[..end]).ok().map(String::from)
}

/// State word values for DRIVERMGR_DEVICE_STATE_LABEL — mirror
/// drivermon's `DEVICE_STATE_*` constants in supervision.rs.
const STATE_WORD_BOUND: usize = 0;
const STATE_WORD_RESTARTING: usize = 1;
const STATE_WORD_FAILED: usize = 2;

fn state_from_word(word: usize) -> Option<DeviceState> {
    match word {
        STATE_WORD_BOUND => Some(DeviceState::Bound),
        STATE_WORD_RESTARTING => Some(DeviceState::Degraded),
        STATE_WORD_FAILED => Some(DeviceState::Failed),
        _ => None,
    }
}

/// Handle DRIVERMGR_DEVICE_STATE_LABEL: drivermon notifies a device
/// state transition. Parse device_path from payload, look up the
/// DeviceNode in DEVICE_TREE, update node.state. Fire-and-forget —
/// no reply.
fn handle_device_state(full_buf: &[u8]) {
    let payload_start = core::mem::size_of::<Message>();
    let payload: &[u8] = if full_buf.len() > payload_start {
        &full_buf[payload_start..]
    } else {
        &[]
    };
    let device_path = match core::str::from_utf8(payload) {
        Ok(s) if !s.is_empty() => s,
        _ => {
            let _ = debug_print("drivermgr: DEVICE_STATE missing device_path");
            return;
        }
    };
    let msg = unsafe { &*(full_buf.as_ptr() as *const Message) };
    let new_state = match state_from_word(msg.words[1]) {
        Some(s) => s,
        None => {
            let _ = debug_print(&format!(
                "drivermgr: DEVICE_STATE {} unknown state word={}",
                device_path, msg.words[1]
            ));
            return;
        }
    };
    let prev = {
        let mut tree = DEVICE_TREE.lock();
        match tree.get_mut(device_path) {
            Some(node) => {
                let prev = node.state;
                node.state = new_state;
                prev
            }
            None => {
                let _ = debug_print(&format!(
                    "drivermgr: DEVICE_STATE {} — device not in tree",
                    device_path
                ));
                return;
            }
        }
    };
    let _ = debug_print(&format!(
        "drivermgr: DEVICE_STATE {} {:?} -> {:?}",
        device_path, prev, new_state
    ));
}

fn handle_probe(msg: &Message, full_buf: &[u8], endpoint: usize) {
    let payload_start = core::mem::size_of::<Message>();
    let bus_bytes: &[u8] = if full_buf.len() > payload_start {
        &full_buf[payload_start..]
    } else {
        &[]
    };
    let bus = match core::str::from_utf8(bus_bytes) {
        Ok(s) => s.trim(),
        Err(_) => "",
    };
    let _ = debug_print(&format!("drivermgr: PROBE bus='{}'", bus));

    let info = process_info();
    let pci_token = info.tokens[TOKEN_EXTRA_1];
    let space_token = info.tokens[TOKEN_SPACE];

    let status: usize = match bus {
        "pci" => {
            match pci_scan::scan(pci_token, &mut *DEVICE_TREE.lock()) {
                Ok(_) => 0,
                Err(_) => 1,
            }
        }
        "acpi" => {
            match acpi_scan::scan(space_token, &mut *DEVICE_TREE.lock()) {
                Ok(_) => 0,
                Err(_) => 1,
            }
        }
        _ => 2,
    };

    let table = BIND_RULES.lock();
    let spawn_mode = parse_spawn_mode_from_initrd(None);
    if spawn_mode.should_spawn() {
        match_devices(&table, spawn_mode);
    }
    drop(table);

    let reply = Message::new(
        DRIVERMGR_PROBE_LABEL,
        [status, 0, 0, 0, 0, 0],
        1,
    );
    let _ = reply_to_sender_with_payload(msg, &reply, &[], endpoint);
}
