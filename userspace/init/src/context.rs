//! Shared initialization context.
//!
//! Init constructs long-lived resources (registry endpoints, exit endpoint,
//! IRQ handle) once and passes them to each service launcher. This keeps
//! ownership and lifetime explicit and avoids global singletons.

use crate::boot::BootSnapshot;
use libcluu::{endpoint_create, token_derive, Result, Rights};

/// Shared resources available to every service during startup.
pub struct InitContext<'a> {
    pub boot: BootSnapshot,
    pub initrd: &'a [u8],
    pub exit_endpoint: usize,
    pub primordial_exit_recv: usize,
    pub primordial_exit_send: usize,
    pub registry_endpoint: usize,
    pub registry_send: usize,
    pub kbd_irq_token: usize,
    pub virtio_blk_irq_token: usize,
    pub virtio_9p_irq_token: usize,
    pub virtio_net_irq_token: usize,
    pub virtio_snd_irq_token: usize,
    pub pci_token: usize,
    pub irq_handle_root_token: usize,
}

impl<'a> InitContext<'a> {
    /// Build the shared init context from boot snapshot + initrd mapping.
    ///
    /// This aggregates endpoints/tokens that are reused by many services so
    /// we can construct them once and pass references explicitly.
    pub fn new(boot: BootSnapshot, initrd: &'a [u8]) -> Result<Self> {
        // Exit endpoint used by procmgr to receive child exit notifications.
        let exit_endpoint = create_exit_endpoint(boot.root_token)?;

        // Primordial exit endpoint: init monitors this for primordial service deaths.
        let primordial_exit_full = endpoint_create(boot.root_token)?;
        let primordial_exit_recv =
            token_derive(primordial_exit_full, Rights::IPC_RECV.bits() as usize, u64::MAX)?;
        let primordial_exit_send =
            token_derive(primordial_exit_full, Rights::IPC_SEND.bits() as usize, u64::MAX)?;

        // Registry uses a single shared listen endpoint (recv) and a send token.
        // IPC_CALL is included so callers can use synchronous call-style IPC
        // (required by register_output, unregister_output which now block until
        // the registry has committed the entry and replied).
        // GRANT is included so spawners (root-procmgr, session-procmgr) can
        // re-derive narrower handles for their children via profile_to_rights.
        let registry_full = endpoint_create(boot.root_token)?;
        let registry_endpoint =
            token_derive(registry_full, Rights::IPC_RECV.bits() as usize, u64::MAX)?;
        let registry_send = token_derive(
            registry_full,
            (Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT).bits() as usize,
            u64::MAX,
        )?;

        // Keyboard service needs an IRQ handle token.
        let kbd_irq_token = token_derive(
            boot.root_token,
            Rights::IRQ_HANDLE.bits() as usize,
            u64::MAX,
        )?;

        // virtio-blk IRQ handle for IRQ 11 (virtio-blk on QEMU PIC).
        let virtio_blk_irq_token = token_derive(
            boot.root_token,
            Rights::IRQ_HANDLE.bits() as usize,
            u64::MAX,
        )?;

        let virtio_9p_irq_token = token_derive(
            boot.root_token,
            Rights::IRQ_HANDLE.bits() as usize,
            u64::MAX,
        )?;

        let virtio_net_irq_token = token_derive(
            boot.root_token,
            Rights::IRQ_HANDLE.bits() as usize,
            u64::MAX,
        )?;

        let virtio_snd_irq_token = token_derive(
            boot.root_token,
            Rights::IRQ_HANDLE.bits() as usize,
            u64::MAX,
        )?;

        // PCI_ACCESS token for ACPI shutdown/reset port I/O.
        let pci_token = token_derive(
            boot.root_token,
            Rights::PCI_ACCESS.bits() as usize,
            u64::MAX,
        )?;

        // IRQ handle root token for devmgr (D3.1).  Carries GRANT so devmgr
        // can sub-derive per-driver IRQ_HANDLE | IRQ_ACK tokens via
        // MINT_IRQ_CAP.  Wired to devmgr's TOKEN_EXTRA_2.
        let irq_handle_root_token = token_derive(
            boot.root_token,
            (Rights::IRQ_HANDLE | Rights::IRQ_ACK | Rights::GRANT).bits() as usize,
            u64::MAX,
        )?;

        Ok(Self {
            boot,
            initrd,
            exit_endpoint,
            primordial_exit_recv,
            primordial_exit_send,
            registry_endpoint,
            registry_send,
            kbd_irq_token,
            virtio_blk_irq_token,
            virtio_9p_irq_token,
            virtio_net_irq_token,
            virtio_snd_irq_token,
            pci_token,
            irq_handle_root_token,
        })
    }
}

/// Create a grantable endpoint for exit notifications.
///
/// Procmgr needs to receive exit notifications and forward them, so this
/// endpoint is created with both recv and grant capabilities.
fn create_exit_endpoint(token: usize) -> Result<usize> {
    let endpoint = endpoint_create(token)?;
    let rights = Rights::IPC_RECV | Rights::IPC_SEND | Rights::GRANT;
    token_derive(endpoint, rights.bits() as usize, u64::MAX)
}
