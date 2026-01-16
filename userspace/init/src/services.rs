//! Service catalog.
//!
//! Service metadata is kept here so it can be reused by the launcher without
//! leaking policy into the boot and mapping layers.

use libcluu::Rights;

pub struct ServiceSpec {
    pub name: &'static str,
    pub path: &'static str,
    pub priority: usize,
    pub rights: Option<Rights>,
    pub kind: ServiceKind,
    pub instance_id: Option<u64>,
}

#[derive(Copy, Clone)]
pub enum ServiceKind {
    Registry,
    Console,
    Kbd,
    Tty,
    Procmgr,
}

// Capability grant for procmgr (it needs to create, map, and manage children).
const PROCMGR_RIGHTS_BITS: u32 = Rights::READ.bits()
    | Rights::WRITE.bits()
    | Rights::CREATE.bits()
    | Rights::THREAD_CONTROL.bits()
    | Rights::THREAD_SUSPEND.bits()
    | Rights::DESTROY.bits()
    | Rights::SPACE_MAP.bits()
    | Rights::SPACE_UNMAP.bits()
    | Rights::SPACE_GRANT.bits()
    | Rights::IPC_SEND.bits()
    | Rights::IPC_RECV.bits()
    | Rights::IPC_CALL.bits()
    | Rights::IRQ_HANDLE.bits()
    | Rights::IRQ_ACK.bits()
    | Rights::GRANT.bits();

const PROCMGR_RIGHTS: Rights = Rights::from_bits_truncate(PROCMGR_RIGHTS_BITS);

// Boot-critical services in launch order.
pub const SERVICE_LIST: &[ServiceSpec] = &[
    ServiceSpec {
        name: "registry",
        path: "sys/registry",
        priority: 190,
        rights: None,
        kind: ServiceKind::Registry,
        instance_id: None,
    },
    ServiceSpec {
        name: "procmgr",
        path: "sys/procmgr",
        priority: 200,
        rights: Some(PROCMGR_RIGHTS),
        kind: ServiceKind::Procmgr,
        instance_id: None,
    },
    ServiceSpec {
        name: "kbd",
        path: "sys/kbd",
        priority: 230,
        rights: None,
        kind: ServiceKind::Kbd,
        instance_id: None,
    },
    ServiceSpec {
        name: "tty",
        path: "sys/tty",
        priority: 205,
        rights: None,
        kind: ServiceKind::Tty,
        instance_id: Some(0),
    },
    ServiceSpec {
        name: "console",
        path: "sys/console",
        priority: 210,
        rights: None,
        kind: ServiceKind::Console,
        instance_id: Some(0),
    },
];
