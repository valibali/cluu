/*
 * Virtual File System (VFS) Layer
 *
 * This module implements the kernel VFS stub for the microkernel architecture.
 * File operations are handled by a userspace VFS server via IPC.
 *
 * Architecture:
 * - Kernel provides syscalls (open, read, write, close) that forward to VFS server
 * - VFS server runs as userspace daemon (PID 2) and handles all filesystem operations
 * - Communication uses IPC port-based messaging (256-byte messages)
 * - VFS server registers itself with well-known port name "vfs"
 *
 * Mount Points (Phase 1):
 * - /bin/     - Executables from initrd (read-only)
 * - /sys/     - System files from initrd (read-only)
 * - /dev/null - Null device (discard writes, EOF on reads)
 * - /dev/initrd - Raw access to initrd TAR archive
 * - All other paths return -ENOENT
 */

pub mod protocol;

use crate::scheduler::ProcessId;
use crate::scheduler::ipc::{self, IpcError, PortId};
use alloc::collections::BTreeMap;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use protocol::*;
use spin::Mutex;

/// Well-known port name for VFS server
pub const VFS_PORT_NAME: &str = "vfs";

/// VFS server port ID (resolved at runtime when server registers)
static VFS_SERVER_PORT: Mutex<Option<PortId>> = Mutex::new(None);

/// VFS initialization flag
static VFS_INIT: AtomicBool = AtomicBool::new(false);

/// Global request ID counter (for matching requests/responses)
static NEXT_REQUEST_ID: AtomicU64 = AtomicU64::new(1);

/// Port name registry (maps string names to PortIds)
///
/// This allows services to register well-known port names that clients
/// can look up by name instead of needing to know the port ID.
static PORT_NAME_REGISTRY: Mutex<Option<BTreeMap<&'static str, PortId>>> = Mutex::new(None);

/// Initialize VFS subsystem
///
/// Must be called after IPC is initialized but before any VFS operations.
pub fn init() {
    *PORT_NAME_REGISTRY.lock() = Some(BTreeMap::new());
    VFS_INIT.store(true, Ordering::SeqCst);
    log::info!("VFS subsystem initialized (waiting for VFS server to register)");
}

/// Spawn the VFS server userspace process
///
/// This function:
/// 1. Gets initrd location from bootloader
/// 2. Spawns VFS server ELF binary from initrd
/// 3. Creates shared memory region for initrd
/// 4. Maps initrd into VFS server's address space
///
/// The VFS server will then:
/// - Parse the TAR archive in the mapped initrd
/// - Register its IPC port with name "vfs"
/// - Wait for file operation requests
///
/// Must be called after scheduler::init() and before scheduler::enable()
pub fn spawn_server() -> Result<(ProcessId, crate::scheduler::ThreadId), &'static str> {
    use crate::initrd;
    use crate::loaders;
    use crate::scheduler;
    use alloc::format;

    log::info!("Spawning VFS server...");

    // Get initrd info for creating shared memory region
    let (initrd_phys, initrd_size) = initrd::get_info();
    log::info!(
        "Initrd: phys=0x{:x}, size={} bytes ({} MB)",
        initrd_phys.as_u64(),
        initrd_size,
        initrd_size / 1024 / 1024
    );

    // Create shared memory region from physical initrd
    // Kernel provides the resource, VFS server decides where to map it
    let shmem_id = scheduler::shmem::shmem_create_from_phys(
        initrd_phys,
        initrd_size,
        scheduler::ProcessId(0), // Kernel owns the physical memory
        scheduler::shmem::ShmemPermissions {
            read: true,
            write: false, // Read-only
        },
    )
    .map_err(|_| "Failed to create shmem for initrd")?;

    log::info!("Created shmem region {} for initrd", shmem_id.0);

    // Format arguments: shmem_id and size
    // VFS server will decide where to map this in its own address space
    let shmem_id_str = format!("{}", shmem_id.0);
    let initrd_size_str = format!("{}", initrd_size);

    log::info!(
        "VFS server args: shmem_id={}, size={}",
        shmem_id_str,
        initrd_size_str
    );

    // Spawn VFS server process
    // Pass shmem_id (not physical address!) - VFS will map it itself
    let vfs_binary = vfs_read_file("sys/vfs_server").map_err(|_| "VFS server binary not found")?;

    let (pid, tid) = loaders::elf::spawn_elf_process(
        &vfs_binary,
        "vfs_server",
        &[shmem_id_str.as_str(), initrd_size_str.as_str()],
    )
    .map_err(|_| "Failed to spawn VFS server")?;

    log::info!("VFS server spawned: PID={:?}, TID={:?}", pid, tid);
    log::info!("VFS server will map initrd shmem region into its own address space");

    Ok((pid, tid))
}

/// Register a port with a well-known name
///
/// This allows servers to register their ports so clients can find them
/// by name instead of needing to know the port ID.
///
/// # Arguments
/// * `name` - The well-known name for this port (e.g., "vfs")
/// * `port_id` - The port ID to register
///
/// # Returns
/// Ok if registration successful, Err if name already registered
pub fn register_port_name(name: &'static str, port_id: PortId) -> Result<(), &'static str> {
    let mut registry = PORT_NAME_REGISTRY.lock();
    if let Some(ref mut map) = *registry {
        if map.contains_key(name) {
            return Err("Port name already registered");
        }
        map.insert(name, port_id);
        log::info!("Registered port name '{}' -> Port({})", name, port_id.0);
        Ok(())
    } else {
        Err("Port name registry not initialized")
    }
}

/// Look up a port ID by well-known name
///
/// # Arguments
/// * `name` - The port name to look up (e.g., "vfs")
///
/// # Returns
/// Some(PortId) if name is registered, None otherwise
pub fn lookup_port_name(name: &str) -> Option<PortId> {
    let registry = PORT_NAME_REGISTRY.lock();
    if let Some(ref map) = *registry {
        map.get(name).copied()
    } else {
        None
    }
}

/// Register VFS server port
///
/// Called by VFS server during initialization to register its IPC port.
/// After registration, kernel VFS stub can forward file operations to the server.
///
/// # Arguments
/// * `port_id` - The IPC port where VFS server is listening
pub fn register_vfs_server(port_id: PortId) -> Result<(), &'static str> {
    // Register in port name registry
    register_port_name(VFS_PORT_NAME, port_id)?;

    // Store in VFS-specific variable for quick access
    *VFS_SERVER_PORT.lock() = Some(port_id);

    log::info!("VFS server registered on port {}", port_id.0);
    Ok(())
}

/// Get VFS server port ID
///
/// Returns the port ID of the VFS server if it has registered.
fn get_vfs_port() -> Result<PortId, IpcError> {
    let port = VFS_SERVER_PORT.lock();
    port.ok_or(IpcError::PortNotFound)
}

/// Check if VFS server is ready
///
/// Returns true if VFS server has registered and is ready to handle requests.
pub fn is_vfs_ready() -> bool {
    VFS_SERVER_PORT.lock().is_some()
}

/// Allocate a new request ID
fn next_request_id() -> u64 {
    NEXT_REQUEST_ID.fetch_add(1, Ordering::SeqCst)
}

/// Send VFS request and wait for response
///
/// This is a synchronous request-response pattern:
/// 1. Create a reply port for this request
/// 2. Include reply port ID in the request
/// 3. Send request message to VFS server port
/// 4. Block waiting for response on reply port
/// 5. Destroy reply port
/// 6. Return response
///
/// # Arguments
/// * `request` - The VFS request to send
///
/// # Returns
/// The VFS response message, or IpcError if communication fails
fn vfs_request_sync(mut request: VfsRequest) -> Result<VfsRequest, IpcError> {
    // Get VFS server port
    let vfs_port = get_vfs_port()?;

    // Create a reply port for this request
    let reply_port = ipc::port_create()?;

    // Allocate request ID
    let req_id = next_request_id();
    request.set_request_id(req_id);
    request.set_reply_port_id(reply_port.0 as u64);

    // Send request to VFS server
    let msg = request.into_message();
    ipc::port_send(vfs_port, msg)?;

    // Block waiting for response on reply port
    let response_msg = ipc::port_recv(reply_port)?;

    // Destroy the reply port (we're done with it)
    let _ = ipc::port_destroy(reply_port);

    // Convert response message to VfsRequest
    let response = VfsRequest::from_message(response_msg);

    // Verify request ID matches (for debugging)
    if response.request_id() != req_id {
        log::warn!(
            "VFS response request_id mismatch: expected {}, got {}",
            req_id,
            response.request_id()
        );
    }

    Ok(response)
}

/// VFS open() operation
///
/// Send VFS_OPEN request to userspace VFS server and return file descriptor.
///
/// # Arguments
/// * `path` - File path (e.g., "/bin/hello")
/// * `flags` - Open flags (O_RDONLY, O_WRONLY, O_RDWR, etc.)
///
/// # Returns
/// File descriptor on success, negative error code on failure
pub fn vfs_open(path: &str, flags: i32) -> isize {
    if !VFS_INIT.load(Ordering::SeqCst) {
        return VFS_ERR_NOSYS as isize;
    }

    // Check if VFS server is ready
    if !is_vfs_ready() {
        log::warn!("vfs_open: VFS server not ready");
        return VFS_ERR_NOSYS as isize;
    }

    // Create request
    let request = create_open_request(0, path, flags);

    // Send request and wait for response
    match vfs_request_sync(request) {
        Ok(response) => {
            let result = response.result();
            if result < 0 {
                return result as isize;
            }

            let fd = response.fd();
            let shmem_id = response.shmem_id();

            // If VFS server provided shmem_id, map it into client address space
            if shmem_id >= 0 {
                use crate::scheduler::shmem::{ShmemId, ShmemPermissions};

                if let Some(current_pid) = crate::scheduler::current_process_id() {
                    // Map fsitem into client address space (read-only)
                    let perms = ShmemPermissions::from_flags(ShmemPermissions::READ);

                    match crate::scheduler::shmem::shmem_map(
                        ShmemId(shmem_id as usize),
                        current_pid,
                        0x0,  // Let kernel choose address
                        perms
                    ) {
                        Ok(virt_addr) => {
                            log::info!("vfs_open: Mapped fsitem for FD {} at {:?}", fd, virt_addr);
                            // TODO: Store mapping in per-process FD table
                            // For now, client will need to call syscall to get fsitem address
                        }
                        Err(e) => {
                            log::warn!("vfs_open: Failed to map fsitem: {:?}, falling back to IPC reads", e);
                            // Continue without mapping - client will use IPC-based reads
                        }
                    }
                } else {
                    log::warn!("vfs_open: No current process, cannot map fsitem");
                }
            }

            fd as isize
        }
        Err(e) => {
            log::error!("vfs_open: IPC error: {:?}", e);
            VFS_ERR_IO as isize
        }
    }
}

/// VFS read() operation
///
/// # Arguments
/// * `fd` - File descriptor
/// * `buffer` - Buffer to read into
/// * `count` - Number of bytes to read
///
/// # Returns
/// Number of bytes read on success, negative error code on failure
pub fn vfs_read(fd: i32, buffer: &mut [u8], count: usize) -> isize {
    if !VFS_INIT.load(Ordering::SeqCst) {
        return VFS_ERR_NOSYS as isize;
    }

    if !is_vfs_ready() {
        return VFS_ERR_NOSYS as isize;
    }

    // Limit read to MAX_PATH_LEN (216 bytes) per request
    let count = count.min(MAX_PATH_LEN);

    let request = create_read_request(0, fd, count as u64);

    match vfs_request_sync(request) {
        Ok(response) => {
            let result = response.result();
            if result < 0 {
                result as isize
            } else {
                // Copy data from response to buffer
                let bytes_read = result as usize;
                let data = response.buffer();
                buffer[..bytes_read].copy_from_slice(&data[..bytes_read]);
                result as isize
            }
        }
        Err(e) => {
            log::error!("vfs_read: IPC error: {:?}", e);
            VFS_ERR_IO as isize
        }
    }
}

/// VFS write() operation
///
/// # Arguments
/// * `fd` - File descriptor
/// * `buffer` - Buffer to write from
/// * `count` - Number of bytes to write
///
/// # Returns
/// Number of bytes written on success, negative error code on failure
pub fn vfs_write(fd: i32, buffer: &[u8], count: usize) -> isize {
    if !VFS_INIT.load(Ordering::SeqCst) {
        return VFS_ERR_NOSYS as isize;
    }

    if !is_vfs_ready() {
        return VFS_ERR_NOSYS as isize;
    }

    // Limit write to MAX_PATH_LEN (216 bytes) per request
    let count = count.min(MAX_PATH_LEN);

    let request = create_write_request(0, fd, &buffer[..count]);

    match vfs_request_sync(request) {
        Ok(response) => {
            let result = response.result();
            result as isize
        }
        Err(e) => {
            log::error!("vfs_write: IPC error: {:?}", e);
            VFS_ERR_IO as isize
        }
    }
}

/// VFS close() operation
///
/// # Arguments
/// * `fd` - File descriptor to close
///
/// # Returns
/// 0 on success, negative error code on failure
pub fn vfs_close(fd: i32) -> isize {
    if !VFS_INIT.load(Ordering::SeqCst) {
        return VFS_ERR_NOSYS as isize;
    }

    if !is_vfs_ready() {
        return VFS_ERR_NOSYS as isize;
    }

    let request = create_close_request(0, fd);

    match vfs_request_sync(request) {
        Ok(response) => response.result() as isize,
        Err(e) => {
            log::error!("vfs_close: IPC error: {:?}", e);
            VFS_ERR_IO as isize
        }
    }
}

/// VFS lseek() operation
///
/// # Arguments
/// * `fd` - File descriptor
/// * `offset` - Offset to seek to
/// * `whence` - Seek mode (SEEK_SET, SEEK_CUR, SEEK_END)
///
/// # Returns
/// New file offset on success, negative error code on failure
pub fn vfs_lseek(fd: i32, offset: i64, whence: i32) -> isize {
    if !VFS_INIT.load(Ordering::SeqCst) {
        return VFS_ERR_NOSYS as isize;
    }

    if !is_vfs_ready() {
        return VFS_ERR_NOSYS as isize;
    }

    let request = create_lseek_request(0, fd, offset, whence);

    match vfs_request_sync(request) {
        Ok(response) => {
            let result = response.result();
            if result < 0 {
                result as isize
            } else {
                response.offset() as isize
            }
        }
        Err(e) => {
            log::error!("vfs_lseek: IPC error: {:?}", e);
            VFS_ERR_IO as isize
        }
    }
}

/// Read entire file from VFS (convenience function for kernel use)
///
/// This is a helper for kernel code that needs to load files (like ELF binaries).
/// It opens the file, reads all data, and closes it.
///
/// # Arguments
/// * `path` - File path (e.g., "/bin/hello")
///
/// # Returns
/// File data on success, or error message
pub fn vfs_read_file(path: &str) -> Result<alloc::vec::Vec<u8>, &'static str> {
    // If VFS server is ready, use it
    if is_vfs_ready() {
        // Open file via VFS
        let fd = vfs_open(path, O_RDONLY);
        if fd < 0 {
            return Err("Failed to open file via VFS");
        }

        // Get file size via lseek
        let file_size = vfs_lseek(fd as i32, 0, SEEK_END);
        if file_size < 0 {
            vfs_close(fd as i32);
            return Err("Failed to get file size");
        }

        // Seek back to start
        vfs_lseek(fd as i32, 0, SEEK_SET);

        // Read entire file
        let mut data = alloc::vec::Vec::with_capacity(file_size as usize);
        data.resize(file_size as usize, 0);

        let mut total_read = 0;
        while total_read < file_size as usize {
            let to_read = (file_size as usize - total_read).min(MAX_PATH_LEN);
            let bytes_read = vfs_read(fd as i32, &mut data[total_read..], to_read);
            if bytes_read <= 0 {
                vfs_close(fd as i32);
                return Err("Failed to read file via VFS");
            }
            total_read += bytes_read as usize;
        }

        vfs_close(fd as i32);
        return Ok(data);
    }

    // Fall back to direct initrd access
    log::warn!("vfs_read_file: VFS not ready, falling back to initrd");

    // Try multiple path variations since initrd doesn't understand mount points
    // Common mount prefixes to try stripping
    let mount_prefixes = ["/dev/initrd/", "/mnt/", "/"];

    // First, try the path as-is (in case it's already simple like "sys/vfs_server")
    if let Ok(data) = crate::initrd::read_file(path) {
        return Ok(data.to_vec());
    }

    // Try stripping common mount prefixes
    for prefix in &mount_prefixes {
        if path.starts_with(prefix) {
            let stripped = &path[prefix.len()..];
            if let Ok(data) = crate::initrd::read_file(stripped) {
                return Ok(data.to_vec());
            }
        }
    }

    // Nothing worked
    Err("Failed to read file from initrd")
}
