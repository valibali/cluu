//! Frame token allocate/map/free helpers for compositor SHM regions.
//!
//! Per-window SHM is a `WindowShm` header + cells laid out contiguously in
//! a frame token allocated via `InvokeOp::FrameAllocate`. The frame token
//! is shared with the client by handing the value back in the
//! `WIN_REGISTER_REPLY` message. Both compositor and client invoke
//! `space_map_range` with `MAP_FRAME_TOKEN` to map the same physical
//! frames into their own address spaces.

use libcluu::boot::space_token;
use libcluu::syscall::{self, InvokeOp, MAP_FRAME_TOKEN};
use libcluu::{Error, Result};

/// READ + WRITE + USER bits used for SHM mappings.
const FLAGS_USER_RW: usize = 0x07;
const PAGE_SIZE: usize = 4096;

/// Allocate a frame token covering at least `bytes` (rounded up to 4 KiB).
/// Returns `(token, allocated_bytes)`.
///
/// Uses the space token (which has CREATE right via the `space_grant`
/// capability) rather than the root token (which is 0 for container processes).
pub fn alloc_frame(bytes: usize) -> Result<(u64, usize)> {
    let rounded = (bytes + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let sp = space_token();
    if sp == 0 {
        return Err(Error::InvalidArgument);
    }
    let token =
        unsafe { syscall::invoke(sp, InvokeOp::FrameAllocate, rounded, 0, 0, 0)? };
    Ok((token as u64, rounded))
}

/// Map a previously allocated frame token at the given virtual address with
/// READ + WRITE rights. The address must be page-aligned and not collide
/// with existing mappings.
pub fn map_frame_rw(va: usize, token: u64, size: usize) -> Result<()> {
    let space = libcluu::boot::space_token();
    if space == 0 {
        return Err(Error::InvalidArgument);
    }
    let rounded = (size + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    let num_pages = rounded / PAGE_SIZE;
    syscall::space_map_range(
        space,
        va,
        token as usize,
        FLAGS_USER_RW | MAP_FRAME_TOKEN,
        num_pages,
        0,
    )?;
    Ok(())
}

/// Free a frame token allocated via `alloc_frame`.
///
/// The `token` parameter is the frame token handle returned by `alloc_frame`.
/// FrameFree is invoked on the frame token itself (not the space token).
pub fn free_frame(token: u64) -> Result<()> {
    if token == 0 {
        return Err(Error::InvalidArgument);
    }
    unsafe { syscall::invoke(token as usize, InvokeOp::FrameFree, 0, 0, 0, 0)? };
    Ok(())
}
