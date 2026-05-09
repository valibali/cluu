//! Service wiring and launch logic.
//!
//! This module owns the orchestration of service startup. It applies the
//! ServiceKind-specific policy through a small trait (Single Responsibility),
//! keeping each service's wiring decisions isolated from boot/mapping code.

use alloc::format;
use libcluu::boot::{
    CONSOLE_FB_BASE,
    PARAM_CAP_PROFILE,
    PARAM_CONSOLE_ACTIVE,
    PARAM_CONSOLE_INSTANCE,
    PARAM_FB_BASE,
    PARAM_FB_HEIGHT,
    PARAM_FB_PHYS,
    PARAM_FB_PITCH,
    PARAM_FB_SIZE,
    PARAM_FB_WIDTH,
    PARAM_INITRD_SIZE,
    PARAM_TTY_INSTANCE,
    PARAM_VFS_FB_PHYS,
    PARAM_VFS_FB_SIZE,
    PARAM_VFS_FB_WIDTH,
    PARAM_VFS_FB_HEIGHT,
    PARAM_VFS_FB_PITCH,
    // New token slot constants
    TOKEN_CLOCK,
    TOKEN_EXTRA_0,
    TOKEN_EXTRA_1,
    TOKEN_EXTRA_2,
    TOKEN_IPC,
    TOKEN_REGISTRY,
    TOKEN_SELF,
    TOKEN_SPACE,
};
use libcluu::boot_manifest::BootManifest;
use libcluu::elf::ElfFile;
use libcluu::tar::find_member;
use libcluu::*;

use crate::context::InitContext;
use crate::mappings::{map_framebuffer, map_initrd, map_process_info};
use crate::services::{ServiceKind, ServiceSpec, SpacePolicy};

// ===== Process layout =====
const PROC_STACK_SIZE: usize = 64 * 1024;
const PROC_STACK_BASE: usize = 0x6f000000;
const PROC_STACK_TOP: usize = PROC_STACK_BASE + PROC_STACK_SIZE;
const STACK_FLAGS: usize = 0x03; // read + write
const STACK_STEP: usize = PROC_STACK_SIZE + 0x1000;

/// Wiring policy interface. Each service kind implements its own behavior.
///
/// This trait keeps per-service decisions isolated from the launch sequence,
/// so init can evolve without cross-cutting changes.
trait ServiceWiring {
    /// Configure tokens and params for the service kind.
    ///
    /// This is where each service declares the endpoints it owns and any
    /// boot parameters it needs.
    fn configure_tokens(
        &self,
        ctx: &InitContext<'_>,
        child_token: usize,
        instance_id: Option<u64>,
        tokens: &mut [usize; 16],
        params: &mut [u64; 14],
    ) -> Result<()>;

    /// Map additional resources required by the service kind.
    ///
    /// This is where device mappings or initrd access are attached.
    fn map_resources(
        &self,
        ctx: &InitContext<'_>,
        space_token: usize,
        params: &[u64; 14],
    ) -> Result<()>;
}

impl ServiceWiring for ServiceKind {
    fn configure_tokens(
        &self,
        ctx: &InitContext<'_>,
        child_token: usize,
        instance_id: Option<u64>,
        tokens: &mut [usize; 16],
        params: &mut [u64; 14],
    ) -> Result<()> {
        // New token layout:
        // - Slots 0-8: Universal (set in launch_service)
        // - Slots 9-15 (TOKEN_EXTRA_*): Contextual, set here per service kind
        //
        // In the new model, services create their own listen endpoints using TOKEN_IPC.
        // We only pass device-specific capabilities (IRQ, PCI) in TOKEN_EXTRA_* slots.
        // TODO: Phase 3 will update services to create own endpoints; for now we still
        // pass pre-created endpoints in TOKEN_EXTRA_0 for backward compatibility.

        match self {
            ServiceKind::Registry => {
                // Registry owns the shared listen endpoint so everyone can contact it.
                // This is a special case - registry endpoint is set by init.
                tokens[TOKEN_EXTRA_0] = ctx.registry_endpoint;
            }
            ServiceKind::Timeserver => {
                // Grantable so registry can derive send-only tokens for
                // subscribers (clock_gettime / time / gettimeofday).
                tokens[TOKEN_EXTRA_0] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
            }
            ServiceKind::Console => {
                // Console will create its own endpoint in Phase 3
                tokens[TOKEN_EXTRA_0] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
                params[PARAM_FB_BASE] = CONSOLE_FB_BASE as u64;
                params[PARAM_FB_SIZE] = ctx.boot.fb_size;
                params[PARAM_FB_WIDTH] = ctx.boot.fb_width as u64;
                params[PARAM_FB_HEIGHT] = ctx.boot.fb_height as u64;
                params[PARAM_FB_PITCH] = ctx.boot.fb_pitch as u64;
                params[PARAM_FB_PHYS] = ctx.boot.fb_phys;
                params[PARAM_CONSOLE_INSTANCE] = instance_id.unwrap_or(0);
                // Only console:0 starts as the active (visible) VT.
                params[PARAM_CONSOLE_ACTIVE] = if instance_id.unwrap_or(0) == 0 { 1 } else { 0 };
            }
            ServiceKind::Kbd => {
                // Kbd will create its own endpoint in Phase 3
                tokens[TOKEN_EXTRA_0] = create_listen_endpoint(ctx.boot.root_token)?;
                // IRQ token for keyboard interrupt handling
                tokens[TOKEN_EXTRA_1] = ctx.kbd_irq_token;
            }
            ServiceKind::Tty => {
                // Tty will create its own endpoint in Phase 3
                tokens[TOKEN_EXTRA_0] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
                params[PARAM_TTY_INSTANCE] = instance_id.unwrap_or(0);
            }
            ServiceKind::Procmgr => {
                // Procmgr exit notification endpoint
                tokens[TOKEN_EXTRA_0] = ctx.exit_endpoint;
                // Elevated capability token for process management
                tokens[TOKEN_EXTRA_1] = child_token;
                params[PARAM_INITRD_SIZE] = ctx.boot.initrd_size as u64;
            }
            ServiceKind::Vfs => {
                // VFS will create its own endpoint in Phase 3
                tokens[TOKEN_EXTRA_0] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
                params[PARAM_INITRD_SIZE] = ctx.boot.initrd_size as u64;
                // Framebuffer info for /proc/fb
                params[PARAM_VFS_FB_PHYS] = ctx.boot.fb_phys;
                params[PARAM_VFS_FB_SIZE] = ctx.boot.fb_size;
                params[PARAM_VFS_FB_WIDTH] = ctx.boot.fb_width as u64;
                params[PARAM_VFS_FB_HEIGHT] = ctx.boot.fb_height as u64;
                params[PARAM_VFS_FB_PITCH] = ctx.boot.fb_pitch as u64;
            }
            ServiceKind::Vtmgr => {
                // Vtmgr needs a listen endpoint for kbd switch requests.
                tokens[TOKEN_EXTRA_0] = create_listen_endpoint(ctx.boot.root_token)?;
            }
            ServiceKind::VirtioBlk => {
                // VirtioBlk will create its own endpoint in Phase 3
                tokens[TOKEN_EXTRA_0] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
                // PCI-capable token for device access
                tokens[TOKEN_EXTRA_1] = child_token;
                // IRQ token for IRQ 11 (virtio-blk on QEMU PIC).
                tokens[TOKEN_EXTRA_2] = ctx.virtio_blk_irq_token;
            }
            ServiceKind::Tpmd => {
                // Tpmd uses its elevated token for MMIO mapping (via TOKEN_SPACE rights)
            }
        }
        Ok(())
    }

    fn map_resources(
        &self,
        ctx: &InitContext<'_>,
        space_token: usize,
        params: &[u64; 14],
    ) -> Result<()> {
        match self {
            ServiceKind::Console => {
                map_framebuffer(space_token, ctx.boot.fb_phys, params[PARAM_FB_SIZE])
            }
            ServiceKind::Procmgr => {
                map_initrd(space_token, ctx.initrd, params[PARAM_INITRD_SIZE] as usize)
            }
            ServiceKind::Vfs => {
                map_initrd(space_token, ctx.initrd, params[PARAM_INITRD_SIZE] as usize)
            }
            ServiceKind::Registry
            | ServiceKind::Kbd
            | ServiceKind::Tty
            | ServiceKind::Vtmgr
            | ServiceKind::VirtioBlk
            | ServiceKind::Tpmd => Ok(()),
            ServiceKind::Timeserver => Ok(()),
        }
    }
}

/// Launch a single service and wire it into the runtime registry model.
///
/// This loads the ELF, maps its segments, builds ProcessInfo, and finally
/// creates the first thread to start execution.
pub fn launch_service(
    ctx: &InitContext<'_>,
    service: &ServiceSpec,
    index: usize,
    manifest: Option<&BootManifest>,
    exit_cookie: usize,
) -> Result<[u8; 32]> {
    // Per-stage timing harness. Disabled at compile time when not chasing
    // boot-time regressions; turn on by flipping BOOT_PROFILE to true. The
    // overhead is one clock_now() per stage (a single InvokeOp::ClockNow
    // syscall, ~1k cycles), which is negligible compared to the stages
    // themselves.
    const BOOT_PROFILE: bool = true;
    let mut t = if BOOT_PROFILE {
        StageTimer::new(ctx.boot.clock_token, service.name)
    } else {
        StageTimer::disabled()
    };

    // Derive an optional capability token for services that need elevated rights.
    let child_token = match service.rights {
        Some(rights) => token_derive(ctx.boot.root_token, rights.bits() as usize, u64::MAX)?,
        None => ctx.boot.root_token,
    };
    t.mark("token_derive");

    if service.name == "procmgr" {
        debug_print(&format!("init: procmgr token {}", child_token))?;
    }

    debug_print(&format!("init: launching {}", service.name))?;

    let service_bytes = load_service_image(ctx.initrd, service.path, service.name)?;
    t.mark("load_image");
    let hash = enforce_manifest_policy(manifest, service, service_bytes)?;
    t.mark("manifest_check");
    let elf = ElfFile::parse(service_bytes)?;
    t.mark("elf_parse");

    let stack_top = PROC_STACK_TOP - index * STACK_STEP;
    let space_token = space_create(ctx.boot.root_token)?;
    t.mark("space_create");

    map_segments(space_token, &elf, service_bytes)?;
    t.mark("map_segments");
    map_stack(space_token, stack_top, PROC_STACK_SIZE, STACK_FLAGS)?;
    t.mark("map_stack");

    // Assemble process info payload (tokens + params) before mapping it into the child.
    let mut tokens = [0usize; 16];
    let mut params = [0u64; 14];

    // Write profile before configure_tokens — console's configure_tokens
    // overwrites slot 5 with PARAM_CONSOLE_INSTANCE (documented overlap).
    params[PARAM_CAP_PROFILE] = service.profile.bits() as u64;

    // Universal token slots (0-8) - every process gets these
    // Slots 0-3: Standard I/O (filled by fill_default_endpoints)
    fill_default_endpoints(ctx.boot.root_token, &mut tokens)?;

    // Slots 4-7: Core capabilities
    tokens[TOKEN_SELF] = derive_self_cap(ctx.boot.root_token)?;
    tokens[TOKEN_SPACE] = 0; // Set later by derive_space_token_for_policy
    tokens[TOKEN_IPC] = derive_ipc_cap(ctx.boot.root_token)?;
    tokens[TOKEN_CLOCK] = ctx.boot.clock_token;

    // Slot 8: System service
    tokens[TOKEN_REGISTRY] = ctx.registry_send;

    service.kind.configure_tokens(
        ctx,
        child_token,
        service.instance_id,
        &mut tokens,
        &mut params,
    )?;
    if tokens[TOKEN_SPACE] == 0 {
        tokens[TOKEN_SPACE] = derive_space_token_for_policy(space_token, service.space_policy)?;
    }
    t.mark("configure_tokens");
    // init-spawned services are system services without PIDs (pid=0)
    // exit_cookie=0 means non-primordial (no exit notification to init).
    let exit_token = if exit_cookie != 0 { ctx.primordial_exit_send } else { 0 };
    map_process_info(space_token, exit_token, exit_cookie, 0, &tokens, &params)?;
    t.mark("map_process_info");

    service.kind.map_resources(ctx, space_token, &params)?;
    t.mark("map_resources");

    let thread_token = thread_create(
        space_token,
        elf.entry_point as usize,
        stack_top,
        service.priority,
        0,
    )?;
    let _ = thread_token;
    t.mark("thread_create");

    // "<name> ready" line is emitted by the StageTimer report below
    // (with TOTAL us).  Duplicate removed.
    t.report();
    Ok(hash)
}

// ─────────────────────────────────────────────────────────────────────────────
// Boot-time stage profiler
// ─────────────────────────────────────────────────────────────────────────────
//
// Records per-stage TSC deltas during launch_service and emits a one-line
// summary at end-of-service. Each line shows: service name, then each stage
// label and its elapsed milliseconds. Read alongside the kernel timestamps
// in the boot log to see total spawn cost vs per-stage breakdown.
//
// Designed to be cheap: clock_now() is one ClockNow invoke (~1k cycles), and
// the recorded entries live on the stack (no heap), capped at 12 stages.

const MAX_STAGES: usize = 12;

struct StageTimer {
    enabled: bool,
    clock_token: usize,
    service: &'static str,
    last_tsc: u64,
    entries: [(&'static str, u64); MAX_STAGES],
    count: usize,
}

impl StageTimer {
    fn new(clock_token: usize, service: &'static str) -> Self {
        let now = libcluu::syscall::clock_now(clock_token).unwrap_or(0);
        Self {
            enabled: true,
            clock_token,
            service,
            last_tsc: now,
            entries: [("", 0); MAX_STAGES],
            count: 0,
        }
    }

    fn disabled() -> Self {
        Self {
            enabled: false,
            clock_token: 0,
            service: "",
            last_tsc: 0,
            entries: [("", 0); MAX_STAGES],
            count: 0,
        }
    }

    fn mark(&mut self, label: &'static str) {
        if !self.enabled || self.count >= MAX_STAGES {
            return;
        }
        let now = libcluu::syscall::clock_now(self.clock_token).unwrap_or(self.last_tsc);
        let delta = now.saturating_sub(self.last_tsc);
        self.entries[self.count] = (label, delta);
        self.count += 1;
        self.last_tsc = now;
    }

    fn report(&self) {
        if !self.enabled || self.count == 0 {
            return;
        }
        let freq = libcluu::syscall::clock_frequency(self.clock_token).unwrap_or(0);
        if freq == 0 {
            return;
        }
        // Per-stage breakdown was useful for boot-perf debugging but produces
        // 12 lines per service.  Keep just the TOTAL summary at INFO; full
        // breakdown is reachable by un-commenting the loop below.
        let mut total_us: u64 = 0;
        for i in 0..self.count {
            let (_label, ticks) = self.entries[i];
            let us = ticks.saturating_mul(1_000_000) / freq;
            total_us = total_us.saturating_add(us);
        }
        let _ = debug_print(&format!(
            "init: {} ready ({}us)",
            self.service, total_us
        ));
    }
}

fn enforce_manifest_policy(
    manifest: Option<&BootManifest>,
    service: &ServiceSpec,
    service_bytes: &[u8],
) -> Result<[u8; 32]> {
    let actual_hash = klibcluu::crypto::hash_sha256(service_bytes);

    let Some(manifest) = manifest else {
        return Ok(actual_hash);
    };

    let entry = manifest
        .services
        .iter()
        .find(|entry| entry.path == service.path)
        .ok_or(Error::PermissionDenied)?;

    let actual_hash_hex = to_lower_hex(&actual_hash);
    if actual_hash_hex != entry.sha256_hex {
        debug_print(&format!(
            "init: manifest hash mismatch for {}",
            service.name
        ))?;
        return Err(Error::PermissionDenied);
    }

    // In current model, services without explicit derived rights inherit root authority.
    let expected_rights_mask = service
        .rights
        .map(|r| r.bits())
        .unwrap_or(Rights::all().bits());
    if entry.rights_mask != expected_rights_mask {
        debug_print(&format!(
            "init: manifest rights mismatch for {}",
            service.name
        ))?;
        return Err(Error::PermissionDenied);
    }

    Ok(actual_hash)
}

fn to_lower_hex(bytes: &[u8; 32]) -> alloc::string::String {
    use alloc::string::String;

    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(64);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Load a service binary from initrd and log the parse step.
///
/// Keeping tar lookups here prevents the caller from depending on archive layout.
fn load_service_image<'a>(initrd: &'a [u8], path: &str, name: &str) -> Result<&'a [u8]> {
    let service_bytes = find_member(initrd, path).ok_or(Error::NotFound)?;
    debug_print(&format!("init: parsed {} entry", name))?;
    Ok(service_bytes)
}

// ===== Endpoint/token helpers =====

/// Ensure stdin/stdout/stderr/stdlog endpoints exist for a service.
///
/// These defaults keep logging and basic IO consistent across services.
fn fill_default_endpoints(token: usize, tokens: &mut [usize; 16]) -> Result<()> {
    if tokens[TOKEN_STDIN] == 0 {
        tokens[TOKEN_STDIN] = endpoint_create(token)?;
    }
    if tokens[TOKEN_STDOUT] == 0 {
        tokens[TOKEN_STDOUT] = endpoint_create(token)?;
    }
    if tokens[TOKEN_STDERR] == 0 {
        tokens[TOKEN_STDERR] = endpoint_create(token)?;
    }
    if tokens[TOKEN_STDLOG] == 0 {
        tokens[TOKEN_STDLOG] = endpoint_create(token)?;
    }
    Ok(())
}

/// Derive a self/thread capability token for child services.
///
/// TOKEN_SELF provides authority for thread operations within the process.
/// For now, this provides basic thread control; elevated rights (THREAD_SUSPEND,
/// DESTROY) are given to procmgr via its elevated token in TOKEN_EXTRA_1.
fn derive_self_cap(token: usize) -> Result<usize> {
    // Basic thread control + read for all processes. READ is required for
    // global stat invokes that authenticate against any valid token bearing
    // it (clock_now, clock_frequency, sched_get_overflow, pmm_get_stats).
    // Without READ on TOKEN_SELF, /proc/{cpuinfo,meminfo,sched_overflow}
    // and any time-based syscall via TOKEN_SELF return PermissionDenied.
    // TODO: Add THREAD_CONTROL right once kernel supports it for self-operations
    let rights = Rights::READ | Rights::CREATE | Rights::GRANT;
    token_derive(token, rights.bits() as usize, u64::MAX)
}

/// Derive an IPC capability token for child services.
///
/// TOKEN_IPC provides authority for IPC operations: creating endpoints,
/// sending/receiving messages, and granting capabilities to others.
fn derive_ipc_cap(token: usize) -> Result<usize> {
    let rights =
        Rights::CREATE | Rights::IPC_SEND | Rights::IPC_RECV | Rights::IPC_CALL | Rights::GRANT;
    token_derive(token, rights.bits() as usize, u64::MAX)
}

/// Derive a space token for the child so services can accept grants.
fn derive_space_token(space_token: usize) -> Result<usize> {
    let rights = Rights::SPACE_MAP | Rights::SPACE_GRANT | Rights::THREAD_CONTROL;
    token_derive(space_token, rights.bits() as usize, u64::MAX)
}

/// Derive a space token that also allows re-derivation (GRANT right).
///
/// Needed by services like VFS that must mint SPACE_MAP tokens to share
/// their address space window with other services.
fn derive_space_token_with_grant(space_token: usize) -> Result<usize> {
    let rights = Rights::SPACE_MAP | Rights::SPACE_GRANT | Rights::GRANT | Rights::THREAD_CONTROL;
    token_derive(space_token, rights.bits() as usize, u64::MAX)
}

/// Select the appropriate space token policy per service kind.
fn derive_space_token_for_policy(space_token: usize, policy: SpacePolicy) -> Result<usize> {
    match policy {
        SpacePolicy::Grantable => derive_space_token_with_grant(space_token),
        SpacePolicy::Standard => derive_space_token(space_token),
    }
}

/// Create a recv-only endpoint for passive listeners.
///
/// Used when the service only needs to wait on incoming messages.
fn create_listen_endpoint(token: usize) -> Result<usize> {
    let endpoint = endpoint_create(token)?;
    token_derive(endpoint, Rights::IPC_RECV.bits() as usize, u64::MAX)
}

/// Create a recv/send/grant endpoint for subscription-style outputs.
///
/// This allows the service to hand out derived tokens to subscribers.
fn create_grantable_listen_endpoint(token: usize) -> Result<usize> {
    let endpoint = endpoint_create(token)?;
    let rights = Rights::IPC_RECV | Rights::IPC_SEND | Rights::IPC_CALL | Rights::GRANT;
    token_derive(endpoint, rights.bits() as usize, u64::MAX)
}
