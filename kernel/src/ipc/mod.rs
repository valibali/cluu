/*
 * Inter-Process Communication (IPC) Module
 *
 * Provides port-based message passing for microkernel IPC.
 * Used by userspace servers (VFS, device drivers, etc.) to communicate.
 *
 * Architecture:
 * - Port-based: Each process can create ports to receive messages
 * - Asynchronous: Send is non-blocking, receive can block
 * - Fixed message size: 256 bytes per message
 * - Port registry: Well-known names for service discovery
 */

pub mod port;

// Re-export public types and functions
pub use port::{
    PortId,
    Message,
    IpcError,
    port_create,
    port_destroy,
    port_send,
    port_recv,
    port_try_recv,
};
