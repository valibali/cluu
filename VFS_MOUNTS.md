# VFS Mount System

## Overview

The CLUU VFS server implements a Unix-like mount table for filesystem abstraction. This allows different filesystems to be mounted at different paths in a unified namespace.

## Architecture

### Mount Table Structure

```c
struct mount_point {
    int in_use;
    char path[256];              /* Mount path, e.g., "/dev/initrd/" */
    int fs_type;                 /* Filesystem type (TAR, TMPFS, etc.) */
    const char *data;            /* FS-specific data pointer */
    size_t data_size;            /* FS-specific data size */
};
```

### Supported Filesystem Types

- **FS_TYPE_TAR** (1): Read-only TAR archive filesystem
- **FS_TYPE_TMPFS** (2): In-memory temporary filesystem (future)
- **FS_TYPE_PROC** (3): Process information filesystem (future)
- **FS_TYPE_DEV** (4): Device filesystem (future)

## Path Resolution

### Algorithm

1. Find the **longest** matching mount point prefix
2. Strip the mount prefix from the path
3. Pass the relative path to the filesystem handler

### Example

```
Request: vfs_open("/dev/initrd/bin/shell")
         ↓
Mount Resolution: "/dev/initrd/" → TAR filesystem @ initrd_base
         ↓
Relative Path: "bin/shell"
         ↓
TAR Handler: tar_find_file(initrd_base, "bin/shell")
         ↓
Result: File data pointer + size
```

## Current Mounts

### At Boot

```
/dev/initrd/  →  Initrd TAR archive (read-only)
```

**Files accessible:**
- `/dev/initrd/bin/shell` - Userspace shell
- `/dev/initrd/bin/vfs_server` - VFS server binary
- `/dev/initrd/sys/vfs_server` - VFS server binary (legacy path)

## Future Mounts

### Root Filesystem (when hard drive support is added)

```
/  →  ext2/ext4 filesystem on /dev/sda1
/dev/initrd/  →  Initrd TAR archive
```

The mount table supports overlaying, so `/dev/initrd/` can coexist with a root mount at `/`.

### Other Potential Mounts

```
/proc/    →  Process information (PID, memory, etc.)
/dev/     →  Device nodes
/tmp/     →  Temporary filesystem (tmpfs)
/mnt/usb/ →  USB drive
```

## Implementation Details

### Mount Priority

Mounts are resolved by **longest prefix match**:
- `/dev/initrd/bin/shell` matches `/dev/initrd/` (14 chars)
- If both `/` and `/dev/initrd/` exist, `/dev/initrd/` wins (more specific)

### Special Files

Special files like `/dev/null` are handled **before** mount resolution as a fast path.

### Kernel Integration

The kernel uses the full mount paths when calling VFS:
```rust
vfs::vfs_read_file("/dev/initrd/bin/shell")  // NEW
vfs::vfs_read_file("bin/shell")              // OLD (won't work)
```

## Adding New Filesystems

### Step 1: Define FS Type

```c
#define FS_TYPE_EXT2  5
```

### Step 2: Implement Handler

```c
const char* ext2_find_file(const char *disk_base, const char *path, size_t *out_size) {
    // Parse ext2 superblock, inodes, directory entries
    // Return pointer to file data
}
```

### Step 3: Add to VFS_OPEN Handler

```c
if (mount->fs_type == FS_TYPE_EXT2) {
    file_data = ext2_find_file(mount->data, relative_path, &file_size);
}
```

### Step 4: Mount at Boot

```c
/* In main(): mount hard drive as root */
vfs_mount("/", FS_TYPE_EXT2, disk_base, disk_size);
```

## Benefits

1. **Clean Abstraction**: Each filesystem implements a simple interface
2. **Flexible Hierarchy**: Multiple filesystems in one namespace
3. **Future-Proof**: Easy to add new filesystem types
4. **Unix-like**: Familiar mount semantics
5. **Zero-Copy**: Pointers into mapped memory, no data copying

## Notes

- Mount table holds up to 16 mount points (MAX_MOUNTS)
- Mounts are permanent until VFS server restart
- Currently no unmount support (future enhancement)
- Thread-safe: Mount table accessed only in VFS server's single thread
