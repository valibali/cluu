//! Service wiring and launch logic.
//!
//! This module owns the orchestration of service startup. It applies the
//! ServiceKind-specific policy through a small trait (Single Responsibility),
//! keeping each service's wiring decisions isolated from boot/mapping code.

use alloc::format;
use libcluu::boot::{
    CONSOLE_FB_BASE, PARAM_CONSOLE_INSTANCE, PARAM_FB_BASE, PARAM_FB_HEIGHT, PARAM_FB_PITCH,
    PARAM_FB_SIZE, PARAM_FB_WIDTH, PARAM_INITRD_SIZE, PARAM_TTY_INSTANCE,
};
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

// ===== Service token layout =====
const SVC_TOKEN_LISTEN: usize = 7; // recv endpoint for service requests
const SVC_TOKEN_CAP: usize = 8; // capability token (procmgr)
const SVC_TOKEN_IRQ: usize = 9; // irq token (kbd)

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
        params: &mut [u64; 8],
    ) -> Result<()>;

    /// Map additional resources required by the service kind.
    ///
    /// This is where device mappings or initrd access are attached.
    fn map_resources(
        &self,
        ctx: &InitContext<'_>,
        space_token: usize,
        params: &[u64; 8],
    ) -> Result<()>;
}

impl ServiceWiring for ServiceKind {
    fn configure_tokens(
        &self,
        ctx: &InitContext<'_>,
        child_token: usize,
        instance_id: Option<u64>,
        tokens: &mut [usize; 16],
        params: &mut [u64; 8],
    ) -> Result<()> {
        match self {
            ServiceKind::Registry => {
                // Registry owns the shared listen endpoint so everyone can contact it.
                tokens[SVC_TOKEN_LISTEN] = ctx.registry_endpoint;
            }
            ServiceKind::Console => {
                tokens[SVC_TOKEN_LISTEN] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
                params[PARAM_FB_BASE] = CONSOLE_FB_BASE as u64;
                params[PARAM_FB_SIZE] = ctx.boot.fb_size;
                params[PARAM_FB_WIDTH] = ctx.boot.fb_width as u64;
                params[PARAM_FB_HEIGHT] = ctx.boot.fb_height as u64;
                params[PARAM_FB_PITCH] = ctx.boot.fb_pitch as u64;
                params[PARAM_CONSOLE_INSTANCE] = instance_id.unwrap_or(0);
            }
            ServiceKind::Kbd => {
                tokens[SVC_TOKEN_LISTEN] = create_listen_endpoint(ctx.boot.root_token)?;
                tokens[SVC_TOKEN_IRQ] = ctx.kbd_irq_token;
            }
            ServiceKind::Tty => {
                tokens[SVC_TOKEN_LISTEN] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
                params[PARAM_TTY_INSTANCE] = instance_id.unwrap_or(0);
            }
            ServiceKind::Procmgr => {
                tokens[SVC_TOKEN_LISTEN] = ctx.exit_endpoint;
                tokens[SVC_TOKEN_CAP] = child_token;
                params[PARAM_INITRD_SIZE] = ctx.boot.initrd_size as u64;
            }
            ServiceKind::Vfs => {
                tokens[SVC_TOKEN_LISTEN] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
                params[PARAM_INITRD_SIZE] = ctx.boot.initrd_size as u64;
            }
            ServiceKind::VirtioBlk => {
                tokens[SVC_TOKEN_LISTEN] = create_grantable_listen_endpoint(ctx.boot.root_token)?;
                tokens[SVC_TOKEN_CAP] = child_token; // Pass PCI-capable token
            }
        }
        Ok(())
    }

    fn map_resources(
        &self,
        ctx: &InitContext<'_>,
        space_token: usize,
        params: &[u64; 8],
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
            ServiceKind::Registry | ServiceKind::Kbd | ServiceKind::Tty | ServiceKind::VirtioBlk => Ok(()),
        }
    }
}

/// Launch a single service and wire it into the runtime registry model.
///
/// This loads the ELF, maps its segments, builds ProcessInfo, and finally
/// creates the first thread to start execution.
pub fn launch_service(ctx: &InitContext<'_>, service: &ServiceSpec, index: usize) -> Result<()> {
    // Derive an optional capability token for services that need elevated rights.
    let child_token = match service.rights {
        Some(rights) => token_derive(ctx.boot.root_token, rights.bits() as usize, u64::MAX)?,
        None => ctx.boot.root_token,
    };

    if service.name == "procmgr" {
        debug_print(&format!("init: procmgr token {}", child_token))?;
    }

    debug_print(&format!("init: launching {}", service.name))?;

    let service_bytes = load_service_image(ctx.initrd, service.path, service.name)?;
    let elf = ElfFile::parse(service_bytes)?;

    let stack_top = PROC_STACK_TOP - index * STACK_STEP;
    let space_token = space_create(ctx.boot.root_token)?;

    map_segments(space_token, &elf, service_bytes)?;
    map_stack(space_token, stack_top, PROC_STACK_SIZE, STACK_FLAGS)?;

    // Assemble process info payload (tokens + params) before mapping it into the child.
    let mut tokens = [0usize; 16];
    let mut params = [0u64; 8];

    tokens[TOKEN_REGISTRY] = ctx.registry_send;
    tokens[TOKEN_PROC_CAP] = derive_proc_cap(ctx.boot.root_token)?;
    tokens[TOKEN_SPACE] = 0;
    fill_default_endpoints(ctx.boot.root_token, &mut tokens)?;

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
    // init-spawned services are system services without PIDs (pid=0)
    // User processes spawned by procmgr get proper PIDs
    map_process_info(space_token, 0, 0, 0, &tokens, &params)?;

    service.kind.map_resources(ctx, space_token, &params)?;

    let thread_token = thread_create(
        space_token,
        elf.entry_point as usize,
        stack_top,
        service.priority,
    )?;
    let _ = thread_token;

    debug_print(&format!("init: {} ready", service.name))?;
    Ok(())
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

/// Derive a minimal process capability token for child services.
///
/// This limits privileges while still allowing IPC and endpoint creation.
fn derive_proc_cap(token: usize) -> Result<usize> {
    let rights =
        Rights::CREATE | Rights::IPC_SEND | Rights::IPC_RECV | Rights::IPC_CALL | Rights::GRANT;
    token_derive(token, rights.bits() as usize, u64::MAX)
}

/// Derive a space token for the child so services can accept grants.
fn derive_space_token(space_token: usize) -> Result<usize> {
    let rights = Rights::SPACE_MAP | Rights::SPACE_GRANT;
    token_derive(space_token, rights.bits() as usize, u64::MAX)
}

/// Derive a space token that also allows re-derivation (GRANT right).
///
/// Needed by services like VFS that must mint SPACE_MAP tokens to share
/// their address space window with other services.
fn derive_space_token_with_grant(space_token: usize) -> Result<usize> {
    let rights = Rights::SPACE_MAP | Rights::SPACE_GRANT | Rights::GRANT;
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
