/*
 * File System Support
 *
 * This module provides basic file system functionality for CLUU kernel.
 * Currently supports:
 * - TAR archive reading (for initrd)
 *
 * Future:
 * - Virtual File System (VFS) layer
 * - Disk-based filesystems (ext2, FAT)
 * - File handles and buffering
 */

pub mod tar;

pub use tar::TarReader;
