# fsitem Implementation Status

## Overview

The fsitem (filesystem item) system combines BSD-style vnodes with microkernel shared memory to provide zero-copy file access. Files are represented as metadata structures in shared memory that clients can map and read directly without IPC overhead.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│ Client Process (Shell, etc.)                                │
│                                                              │
│  syscall_open("/dev/initrd/bin/shell")                     │
│       ↓                                                      │
│  [Kernel VFS Stub] ──IPC──> [VFS Server]                   │
│       ↓                            ↓                         │
│  Returns: FD=3, shmem_id=42   Creates fsitem in shmem      │
│       ↓                                                      │
│  syscall_shmem_map(42) ────> Maps fsitem into address space│
│       ↓                                                      │
│  ┌────────────────────────────────┐                         │
│  │ Shared Memory Region           │                         │
│  │ [0..512)    fsitem metadata    │                         │
│  │ [512..4096) reserved/padding   │                         │
│  │ [4096..EOF) file data          │                         │
│  └────────────────────────────────┘                         │
│       ↓                                                      │
│  read() directly from shmem (zero-copy!)                    │
└─────────────────────────────────────────────────────────────┘
```

## What's Implemented ✅

### 1. fsitem Header Definition
**File:** `userspace/lib/fsitem.h`

Complete 512-byte structure with:
- Magic number (0x46534954 = "FSIT")
- Version field
- Type (FILE, DIR, DEVICE, SYMLINK)
- Flags (open mode)
- File metadata (size, fs_type, mode)
- Data offset (4096)
- Current offset (for read/write position)
- Reference count and lock for synchronization
- Original file path (256 bytes)
- Helper functions: `fsitem_is_valid()`, `fsitem_get_data()`, `fsitem_remaining()`

### 2. VFS Protocol Extension
**File:** `kernel/src/vfs/protocol.rs`

Added shmem_id field to VFS protocol:
- Offset 48-55: shmem_id (i64) - Shared memory ID for fsitem
- Offset 56-255: data (200 bytes) - Path string or buffer data
- Accessors: `shmem_id()`, `set_shmem_id()`

### 3. VFS Server fsitem Creation
**File:** `userspace/vfs_server/vfs_server.c`

VFS_OPEN handler now:
1. Allocates shared memory: 4096 bytes + file_size
2. Maps shmem into VFS server address space
3. Initializes fsitem structure at offset 0:
   - Sets magic number, version, type, flags
   - Stores file size, fs_type, mode
   - Sets data_offset = 4096
   - Copies file path
4. Copies file data from TAR to shmem at offset 4096
5. Unmaps shmem from VFS server
6. Returns FD and shmem_id to client

### 4. Kernel VFS Stub fsitem Mapping ✅ NEW!
**File:** `kernel/src/vfs/mod.rs`

`vfs_open()` now:
1. Sends VFS_OPEN request to VFS server
2. Extracts shmem_id from response
3. Maps fsitem into client process address space (read-only)
4. Logs successful mapping: "Mapped fsitem for FD X at 0x..."
5. Returns FD to caller

### 5. vfs_read_file() Improvements ✅ NEW!
**File:** `kernel/src/vfs/mod.rs`

Fixed to properly use VFS server:
1. If VFS server ready: uses vfs_open/vfs_read/vfs_close via IPC
2. If VFS not ready: falls back to direct initrd with intelligent path resolution:
   - First tries path as-is (e.g., "sys/vfs_server")
   - Then tries stripping common mount prefixes ("/dev/initrd/", "/mnt/", "/")
   - Works for both simple and mount-prefixed paths

### 6. VFS Server Synchronization ✅ NEW!
**File:** `kernel/src/main.rs`

Boot sequence now waits for VFS server:
1. Spawns VFS server thread
2. Yields cooperatively until VFS registers its port
3. Timeout after 100 attempts (prevents infinite wait)
4. Only proceeds with shell spawn after VFS is ready

## What's NOT Yet Implemented ❌

### 1. Per-Process File Descriptor Table
**Location:** TBD (likely `kernel/src/scheduler/process.rs`)

Need to track:
- FD → shmem_id mapping
- FD → virtual address mapping (where fsitem is mapped)
- FD → flags, offset, etc.

**Proposed structure:**
```rust
struct ProcessFd {
    in_use: bool,
    shmem_id: Option<ShmemId>,
    fsitem_addr: Option<VirtAddr>,
    flags: i32,
}

// Add to Process struct:
struct Process {
    // ... existing fields ...
    fd_table: [ProcessFd; 256],
}
```

### 3. Userspace Zero-Copy Read
**Location:** TBD (userspace library or shell)

Userspace needs API to:
1. Get fsitem address from FD
2. Validate fsitem magic
3. Read directly from shmem
4. Update offset atomically

**Example usage:**
```c
// Open file
int fd = syscall_open("/dev/initrd/bin/shell", O_RDONLY);

// Get fsitem address (via new syscall or ioctl?)
struct fsitem *item = (struct fsitem *)syscall_get_fsitem(fd);

if (fsitem_is_valid(item)) {
    // Zero-copy read: direct memory access!
    const char *data = fsitem_get_data(item);
    size_t size = item->size;

    // Read ELF header
    Elf64_Ehdr *ehdr = (Elf64_Ehdr *)data;

    // No syscalls needed for reads!
}

// Close file (unmaps fsitem)
syscall_close(fd);
```

### 4. Fallback to IPC Reads
**File:** `kernel/src/vfs/mod.rs`

For files that don't support fsitem (e.g., pipes, devices), `vfs_read()` should:
1. Check if FD has fsitem mapping
2. If yes, return error (client should read from shmem)
3. If no, fall back to VFS_READ IPC request

## Current Behavior

**What works now:**
- VFS server creates fsitems in shared memory ✅
- fsitem contains complete file metadata and data ✅
- shmem_id is returned to kernel in VFS_OPEN response ✅
- Kernel maps fsitem into client process address space ✅
- vfs_read_file() properly uses VFS server with mount paths ✅

**What doesn't work yet:**
- No per-process FD table to track fsitem mappings
- Userspace has no API to get fsitem address from FD
- Userspace still uses IPC for reads (via VFS_READ syscall)
- Zero-copy benefit not yet realized (need userspace API)

## Testing Plan

Once kernel-side mapping is implemented:

1. **Test fsitem creation:**
   ```
   kernel log should show: "Mapped fsitem for FD 3 at 0x..."
   ```

2. **Test direct read:**
   ```c
   // In shell or test program
   int fd = syscall_open("/dev/initrd/bin/hello", O_RDONLY);
   void *fsitem_addr = syscall_get_fsitem(fd);  // New syscall

   struct fsitem *item = (struct fsitem *)fsitem_addr;
   assert(item->magic == FSITEM_MAGIC);
   assert(item->size > 0);

   const char *data = fsitem_get_data(item);
   assert(data[0] == 0x7f);  // ELF magic
   ```

3. **Test performance:**
   - Benchmark IPC-based read vs direct memory read
   - Should see ~10-100x speedup for large file reads

## Benefits of fsitem Design

1. **Zero-Copy:** File data never copied, stays in original location
2. **Low Latency:** No IPC overhead for reads
3. **Microkernel Isolation:** VFS still in userspace
4. **BSD Compatibility:** Similar to vnode concept
5. **Future-Proof:** Can add more metadata fields without breaking protocol

## Next Steps

1. Implement kernel-side fsitem mapping in `vfs_open()`
2. Add per-process FD table to track mappings
3. Create userspace API for direct fsitem access
4. Update shell to use zero-copy reads
5. Add tests to verify correctness and performance

## Related Files

- `userspace/lib/fsitem.h` - fsitem structure definition
- `kernel/src/vfs/protocol.rs` - VFS protocol with shmem_id
- `userspace/vfs_server/vfs_server.c` - VFS server implementation
- `kernel/src/vfs/mod.rs` - Kernel VFS stub (needs update)
- `VFS_MOUNTS.md` - Mount table documentation
