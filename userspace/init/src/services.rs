//! Service catalog.
//!
//! Service metadata is kept here so it can be reused by the launcher without
//! leaking policy into the boot and mapping layers.
//!
//! Multi-VT: Only the first VT (console:0, tty:0) is launched by init.
//! Additional VTs are spawned on demand by vtmgr when the user switches
//! to a new VT via Ctrl+Alt+Fn.

use libcluu::cap::CapProfile;
use libcluu::Rights;

pub struct ServiceSpec {
    pub name: &'static str,
    pub path: &'static str,
    pub priority: usize,
    pub rights: Option<Rights>,
    pub space_policy: SpacePolicy,
    pub kind: ServiceKind,
    pub instance_id: Option<u64>,
    pub profile: CapProfile,
}

#[derive(Copy, Clone)]
pub enum SpacePolicy {
    Standard,
    Grantable,
}

#[derive(Copy, Clone)]
pub enum ServiceKind {
    Registry,
    Timeserver,
    Console,
    Kbd,
    Tty,
    Procmgr,
    Vfs,
    Vtmgr,
    VirtioBlk,
    Tpmd,
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

// virtio-blk needs PCI access and space mapping for MMIO/DMA, plus IRQ
// rights so it can attach an IRQ handler for IRQ-driven completion.
const VIRTIOBLK_RIGHTS_BITS: u32 = Rights::PCI_ACCESS.bits()
    | Rights::SPACE_MAP.bits()
    | Rights::IPC_SEND.bits()
    | Rights::IPC_RECV.bits()
    | Rights::CREATE.bits()
    | Rights::GRANT.bits()
    | Rights::IRQ_HANDLE.bits()
    | Rights::IRQ_ACK.bits();

const VIRTIOBLK_RIGHTS: Rights = Rights::from_bits_truncate(VIRTIOBLK_RIGHTS_BITS);

// tpmd needs MMIO mapping for TIS interface + IPC for future service
const TPMD_RIGHTS_BITS: u32 = Rights::SPACE_MAP.bits()
    | Rights::IPC_SEND.bits()
    | Rights::IPC_RECV.bits()
    | Rights::CREATE.bits();

const TPMD_RIGHTS: Rights = Rights::from_bits_truncate(TPMD_RIGHTS_BITS);

/// Primordial services whose death causes init to panic.
/// If any of these exit, the system is in an unrecoverable state.
pub const PRIMORDIAL_SERVICES: &[&str] = &[
    "registry",
    "timeserver",
    "procmgr",
    "vfs",
    "virtio-blk",
];

// Boot-critical services in launch order.
//
// Only VT0 (console:0, tty:0) is launched at boot.  Additional VTs are
// spawned on demand by vtmgr when the user activates them.
pub const SERVICE_LIST: &[ServiceSpec] = &[
    ServiceSpec {
        name: "registry",
        path: "sys/registry",
        priority: 190,
        rights: None,
        space_policy: SpacePolicy::Standard,
        kind: ServiceKind::Registry,
        instance_id: None,
        profile: CapProfile::SERVICE,
    },
    ServiceSpec {
        name: "timeserver",
        path: "sys/timeserver",
        priority: 185,
        rights: None,
        space_policy: SpacePolicy::Standard,
        kind: ServiceKind::Timeserver,
        instance_id: None,
        profile: CapProfile::SERVICE,
    },
    ServiceSpec {
        name: "procmgr",
        path: "sys/procmgr",
        priority: 200,
        rights: Some(PROCMGR_RIGHTS),
        space_policy: SpacePolicy::Standard,
        kind: ServiceKind::Procmgr,
        instance_id: None,
        profile: CapProfile::SUPERVISOR,
    },
    ServiceSpec {
        name: "vfs",
        path: "sys/vfs",
        priority: 195,
        rights: None,
        space_policy: SpacePolicy::Grantable,
        kind: ServiceKind::Vfs,
        instance_id: None,
        profile: CapProfile::SERVICE,
    },
    // kbd, console, vtmgr, tty moved to /etc/autostart.toml (procmgr container autostart)
    ServiceSpec {
        name: "virtio-blk",
        path: "sys/virtio-blk",
        priority: 180,
        rights: Some(VIRTIOBLK_RIGHTS),
        space_policy: SpacePolicy::Standard,
        kind: ServiceKind::VirtioBlk,
        instance_id: None,
        profile: CapProfile::SERVICE,
    },
    ServiceSpec {
        name: "tpmd",
        path: "sys/tpmd",
        priority: 175,
        rights: Some(TPMD_RIGHTS),
        space_policy: SpacePolicy::Standard,
        kind: ServiceKind::Tpmd,
        instance_id: None,
        profile: CapProfile::SERVICE,
    },
];
