# Symlink-Following Path Resolution Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the four hard-coded `/bin/` substring strips in the shell with proper ext2 symlink resolution, so that `/bin/<name>` (and any other path that happens to symlink under `/var/images/<name>/`) resolves to the correct container image name through the same code path PATH-based dispatch already uses.

**Architecture:** Add fast-symlink-aware path resolution in the ext2 library and expose it through a new `FS_REALPATH` IPC op on the remote-FS server (virtio-blk). The VFS service forwards an equivalent `VFS_REALPATH` op to its backends; non-ext2 backends (initrd/devfs/procfs/memfs) return the path unchanged. The shell calls `VfsClient::realpath()` to convert any path with a slash to its canonical form, then extracts `<name>` if the canonical form matches `/var/images/<name>/...`. Procmgr is hardened to reject image names that still contain a slash.

**Tech Stack:** Rust no_std, libcluu IPC, ext2 plugin, VFS daemon, virtio-blk fs server, shell pipeline executor. Harness: `scripts/harness_run.sh` with a marker case in `scripts/harness_case_defaults.sh`.

---

## Code Map

Files touched per task, each with one focused responsibility:

- `userspace/ext2/src/inode.rs` — expose fast-symlink raw inline bytes (60-byte i_block window).
- `userspace/ext2/src/lib.rs` — `read_symlink_target`, `realpath_canonical`, override `resolve_path` to follow symlinks.
- `userspace/virtio-blk/src/main.rs` — handle new `FS_REALPATH = 0x30D` op.
- `userspace/libcluu/src/fs/protocol.rs` — add `VFS_REALPATH = 0x210` constant and `VfsOp::Realpath` arm.
- `userspace/libcluu/src/fs/client.rs` — `VfsClient::realpath(path) -> String`.
- `userspace/vfs/src/mount.rs` — extend `MountBackend` trait with `realpath`; default returns input; `RemoteBackend` forwards `FS_REALPATH`.
- `userspace/vfs/src/main.rs` — dispatch `VfsOp::Realpath` to `handle_realpath`, forward through view check + backend.
- `userspace/shell/src/path_lookup.rs` — new helper `image_name_from_canonical(path) -> Option<String>` and `resolve_or_passthrough(name, vfs) -> String` that returns the bare image name shell should send to procmgr.
- `userspace/shell/src/pipeline.rs` — drop the four `strip_prefix("/bin/")` sites and call the new helper.
- `userspace/procmgr/src/main.rs` — defensive validation: reject `image_name` containing `/`.
- `scripts/harness_case_defaults.sh` — new `l2_path_symlink_resolve` marker case.

---

## Task 1: ext2 — expose raw 60-byte i_block window for fast symlinks

**Files:**
- Modify: `userspace/ext2/src/inode.rs`
- Modify: `userspace/ext2/src/lib.rs`

Fast symlinks store their target inline in the 60 bytes that would otherwise hold direct/indirect block pointers. The current `Inode::parse` decodes those bytes as `[u32; 12] + 3 * u32`, throwing away the raw view. Add a method that re-serialises them so we can read targets ≤60 bytes without a data-block fetch.

- [ ] **Step 1: Add `inline_block_bytes()` method to `Inode`**

Open `userspace/ext2/src/inode.rs`. After the existing `set_size` method (around line 99), insert:

```rust
    /// Re-serialise the 60-byte i_block area (12 direct + indirect + double +
    /// triple) as raw little-endian bytes. Used by fast-symlink reads where
    /// the symlink target is stored inline instead of referenced by block
    /// pointers.
    pub fn inline_block_bytes(&self) -> [u8; 60] {
        let mut buf = [0u8; 60];
        for i in 0..12 {
            buf[i * 4..i * 4 + 4].copy_from_slice(&self.direct_blocks[i].to_le_bytes());
        }
        buf[48..52].copy_from_slice(&self.indirect_block.to_le_bytes());
        buf[52..56].copy_from_slice(&self.double_indirect.to_le_bytes());
        buf[56..60].copy_from_slice(&self.triple_indirect.to_le_bytes());
        buf
    }
```

- [ ] **Step 2: Verify ext2 still builds**

Run: `cargo check -p cluu-ext2 --target x86_64-cluu-elf`
Expected: clean, no warnings about unused code (the method is `pub`, so no warning).

- [ ] **Step 3: Commit**

```bash
git add userspace/ext2/src/inode.rs
git commit -m "feat(ext2): expose 60-byte i_block window for fast symlinks

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 2: ext2 — `read_symlink_target` and symlink-following resolve_path

**Files:**
- Modify: `userspace/ext2/src/lib.rs`

Two new public methods on `Ext2Fs`. `read_symlink_target` chooses the inline path when `inode.size() <= 60 && inode.blocks == 0`, otherwise reads the first data block. `resolve_path_following` walks components using `lookup_in_dir`, and on each hit checks whether the inode is a symlink — if so, splices the target into the remaining component list (relative target keeps current dir; absolute target restarts from root). Maximum 32 redirects to bound recursion.

- [ ] **Step 1: Add the two methods to `Ext2Fs<'a>`**

In `userspace/ext2/src/lib.rs`, after the existing `resolve_path_to_inode` method (around line 189), insert:

```rust
    /// Maximum symlink hops permitted before returning `Error::TooManyLinks`.
    /// Linux uses 40; 32 is plenty for our flat layouts.
    pub const MAX_SYMLINK_HOPS: usize = 32;

    /// Read the target of a symbolic-link inode. Handles both the fast
    /// (inline, ≤60 bytes) and indirect (data block) forms.
    pub fn read_symlink_target(&self, inode: &Inode) -> Result<Vec<u8>> {
        if !inode.is_symlink() {
            return Err(Error::InvalidArgument);
        }
        let size = inode.size() as usize;
        if size == 0 || size > 4096 {
            // 4096 cap: a single-block read is enough for any sane symlink.
            return Err(Error::InvalidArgument);
        }
        // Fast symlinks: inode.blocks reports 0 sectors and target ≤ 60 bytes.
        if inode.blocks == 0 && size <= 60 {
            let raw = inode.inline_block_bytes();
            return Ok(raw[..size].to_vec());
        }
        // Indirect symlinks: target lives in direct_blocks[0].
        let phys = inode.direct_blocks[0];
        if phys == 0 {
            return Err(Error::InvalidArgument);
        }
        let mut buf = alloc::vec![0u8; size];
        let phys_byte_offset = (phys as usize) * self.block_size;
        self.block.read_bytes(phys_byte_offset as u64, &mut buf)?;
        Ok(buf)
    }

    /// Walk an absolute path following symlinks at every directory hop,
    /// returning the canonical absolute path AND the inode it resolves to.
    /// `path` must start with `/`. Components `.` and `..` are normalised.
    pub fn realpath_canonical(&self, path: &str) -> Result<(String, u32)> {
        if !path.starts_with('/') {
            return Err(Error::InvalidArgument);
        }
        // Components left to consume, in reverse order (so `pop` walks forward).
        let mut remaining: Vec<String> = path
            .trim_start_matches('/')
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .rev()
            .map(String::from)
            .collect();
        let mut canon: Vec<String> = Vec::new();
        let mut current_inode: u32 = 2; // root
        let mut hops: usize = 0;

        while let Some(component) = remaining.pop() {
            if component == ".." {
                canon.pop();
                // Re-resolve current_inode from root following canon.
                current_inode = 2;
                for part in &canon {
                    let dir = self.read_inode(current_inode)?;
                    current_inode = self.lookup_in_dir(&dir, part)?;
                }
                continue;
            }
            let dir_inode = self.read_inode(current_inode)?;
            let child_inode_num = self.lookup_in_dir(&dir_inode, &component)?;
            let child = self.read_inode(child_inode_num)?;
            if child.is_symlink() {
                hops += 1;
                if hops > Self::MAX_SYMLINK_HOPS {
                    return Err(Error::TooManyLinks);
                }
                let target_bytes = self.read_symlink_target(&child)?;
                let target_str = core::str::from_utf8(&target_bytes)
                    .map_err(|_| Error::InvalidArgument)?;
                if target_str.starts_with('/') {
                    canon.clear();
                    current_inode = 2;
                }
                // Push target components in reverse so they pop in forward order.
                let parts: Vec<String> = target_str
                    .trim_start_matches('/')
                    .split('/')
                    .filter(|c| !c.is_empty() && *c != ".")
                    .map(String::from)
                    .collect();
                for part in parts.into_iter().rev() {
                    remaining.push(part);
                }
                continue;
            }
            canon.push(component);
            current_inode = child_inode_num;
        }

        let mut out = String::from("/");
        out.push_str(&canon.join("/"));
        Ok((out, current_inode))
    }
```

- [ ] **Step 2: Confirm `Error::TooManyLinks` exists in libcluu**

Run: `grep -n "TooManyLinks" /home/vlb2bp/git/cluu/userspace/libcluu/src/error.rs`
Expected: at least one match.

If it does NOT exist, add it. Open `userspace/libcluu/src/error.rs`, find the `Error` enum, and add a `TooManyLinks` variant. Also extend `to_errno` and `from_errno` to map it to/from `-40` (POSIX `ELOOP`).

- [ ] **Step 3: Override `Filesystem::resolve_path` so backends following the trait also follow symlinks**

Still in `userspace/ext2/src/lib.rs`, in the `impl<'a> Filesystem for Ext2Fs<'a>` block (around line 1065), add:

```rust
    fn resolve_path(&self, path: &str) -> Result<u64> {
        let p = if path.starts_with('/') {
            String::from(path)
        } else {
            let mut s = String::from("/");
            s.push_str(path);
            s
        };
        let (_canon, inode) = self.realpath_canonical(&p)?;
        Ok(inode as u64)
    }
```

- [ ] **Step 4: Build verify**

Run: `cargo check -p cluu-ext2 --target x86_64-cluu-elf`
Expected: clean compile. If `Vec`/`String` imports missing, ensure `use alloc::string::String; use alloc::vec::Vec;` are at top of file (they already are — line 19/20).

- [ ] **Step 5: Commit**

```bash
git add userspace/ext2/src/lib.rs userspace/libcluu/src/error.rs
git commit -m "feat(ext2): symlink-following realpath_canonical

Adds read_symlink_target + realpath_canonical and overrides
resolve_path so all callers transparently follow symlinks.
TooManyLinks (ELOOP=-40) bounds recursion at 32 hops.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 3: virtio-blk — handle FS_REALPATH

**Files:**
- Modify: `userspace/virtio-blk/src/main.rs`

The remote-fs server already exposes FS_OPEN/FS_READ/FS_STAT/FS_READDIR. Add FS_REALPATH=0x30D: takes a path payload, returns the canonical path as the reply payload (status word 0 on success, payload = canonical bytes).

- [ ] **Step 1: Add the FS_REALPATH constant**

In `userspace/virtio-blk/src/main.rs` near the existing `FS_OPEN` declaration (line 39):

```rust
const FS_REALPATH: u32 = 0x30D;
```

- [ ] **Step 2: Add the handler arm in `handle_fs_request`**

After the existing `FS_STAT => { … }` arm (around line 467), add:

```rust
        FS_REALPATH => {
            let path = core::str::from_utf8(payload).unwrap_or("");
            match fs.realpath_canonical(path) {
                Ok((canon, _inode)) => {
                    let bytes = canon.into_bytes();
                    let reply_msg = Message::new(FS_REALPATH, [0, bytes.len(), 0, 0, 0, 0], 2);
                    if let Some(token) = reply_token {
                        let _ = reply_with_payload(token, &reply_msg, &bytes);
                    }
                }
                Err(_) => send_error_reply(reply_token, -3), // NotFound
            }
        }
```

- [ ] **Step 3: Build verify**

Run: `cargo check -p virtio-blk --target x86_64-cluu-elf`
Expected: clean compile.

- [ ] **Step 4: Commit**

```bash
git add userspace/virtio-blk/src/main.rs
git commit -m "feat(virtio-blk): FS_REALPATH op for symlink-following resolve

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 4: libcluu — VFS_REALPATH protocol constant

**Files:**
- Modify: `userspace/libcluu/src/fs/protocol.rs`

- [ ] **Step 1: Add `VFS_REALPATH` constant and `VfsOp::Realpath` arm**

After `VFS_FLUSH` at line 63:

```rust
/// Resolve a path to its canonical form, following symlinks.
pub const VFS_REALPATH: u32 = 0x210;
```

In the `VfsOp` enum (line 67), add `Realpath` after `Link`:

```rust
    Link,
    Realpath,
```

In the `from_label` match (line 87), add:

```rust
            VFS_REALPATH => Some(Self::Realpath),
```

- [ ] **Step 2: Build verify**

Run: `cargo check -p libcluu --target x86_64-cluu-elf`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add userspace/libcluu/src/fs/protocol.rs
git commit -m "feat(libcluu/vfs): VFS_REALPATH protocol op

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 5: libcluu — VfsClient::realpath

**Files:**
- Modify: `userspace/libcluu/src/fs/client.rs`

- [ ] **Step 1: Update protocol import**

At the top of `userspace/libcluu/src/fs/client.rs`, line 7 imports list, add `VFS_REALPATH`:

```rust
use crate::fs::protocol::{
    VFS_CLOSE, VFS_FSTAT, VFS_LINK, VFS_MAP_ELF, VFS_MKDIR, VFS_OPEN, VFS_READDIR,
    VFS_READ_GRANT, VFS_READ_RING, VFS_REALPATH, VFS_RENAME, VFS_RING_SETUP, VFS_RMDIR,
    VFS_STAT, VFS_UNLINK, VFS_WRITE,
};
```

- [ ] **Step 2: Add `realpath` method on `VfsClient`**

In the `impl VfsClient` block, after the existing `stat` method (around line 360), add:

```rust
    /// Resolve `path` to its canonical absolute form, following symlinks.
    /// Backends without symlinks (memfs, procfs, devfs, initrd) return the
    /// input unchanged.
    pub fn realpath(&self, path: &str) -> Result<String> {
        let payload = path.as_bytes();
        let msg = make_payload_message(VFS_REALPATH, payload.len(), &[self.client_id]);
        let mut reply_buf = [0u8; 4096];
        let (reply, payload_len) =
            ipc::call_with_reply_buf(self.endpoint, &msg, payload, &mut reply_buf)?;
        parse_status(reply.words[0])?;
        let data_start = core::mem::size_of::<Message>();
        let bytes = &reply_buf[data_start..data_start + payload_len];
        let s = core::str::from_utf8(bytes).map_err(|_| Error::InvalidArgument)?;
        Ok(String::from(s))
    }
```

- [ ] **Step 3: Build verify**

Run: `cargo check -p libcluu --target x86_64-cluu-elf`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/libcluu/src/fs/client.rs
git commit -m "feat(libcluu/vfs): VfsClient::realpath wrapper

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 6: VFS — MountBackend::realpath + RemoteBackend forward

**Files:**
- Modify: `userspace/vfs/src/mount.rs`

- [ ] **Step 1: Add FS_REALPATH constant and trait method**

Open `userspace/vfs/src/mount.rs`. Near the other `FS_*` constants (lines 23-32), add:

```rust
const FS_REALPATH: u32 = 0x30D;
```

In the `MountBackend` trait (line 71), after the `link` method around line 109, add the new method with a default implementation that returns `rel_path` unchanged:

```rust
    fn realpath(&self, rel_path: &str) -> Result<String> {
        Ok(String::from(rel_path))
    }
```

- [ ] **Step 2: Override `realpath` for `RemoteBackend`**

In `impl MountBackend for RemoteBackend` (line 200), after the existing `readdir` impl (around line 230) but inside the same impl block, add:

```rust
    fn realpath(&self, rel_path: &str) -> Result<String> {
        let req = Message::new(FS_REALPATH, [rel_path.len(), 0, 0, 0, 0, 0], 1);
        let mut reply_buf = [0u8; 4096];
        let (reply, payload_len) =
            call_with_reply_buf(self.endpoint, &req, rel_path.as_bytes(), &mut reply_buf)?;
        let status = reply.words[0] as isize;
        if status < 0 {
            return Err(Error::NotFound);
        }
        let data_start = core::mem::size_of::<Message>();
        let bytes = &reply_buf[data_start..data_start + payload_len];
        core::str::from_utf8(bytes)
            .map(String::from)
            .map_err(|_| Error::InvalidArgument)
    }
```

- [ ] **Step 3: Build verify**

Run: `cargo check -p vfs --target x86_64-cluu-elf`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add userspace/vfs/src/mount.rs
git commit -m "feat(vfs): MountBackend::realpath, RemoteBackend forwards FS_REALPATH

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 7: VFS — dispatch VfsOp::Realpath

**Files:**
- Modify: `userspace/vfs/src/main.rs`

- [ ] **Step 1: Update protocol import**

In `userspace/vfs/src/main.rs` line 17:

```rust
use libcluu::fs::protocol::{
    VfsOp, VFS_CLOSE, VFS_FSTAT, VFS_LINK, VFS_MAP_ELF, VFS_MKDIR, VFS_OPEN, VFS_READDIR,
    VFS_READ_GRANT, VFS_READ_RING, VFS_REALPATH, VFS_RENAME, VFS_RING_SETUP, VFS_RMDIR,
    VFS_STAT, VFS_UNLINK, VFS_WRITE,
};
```

- [ ] **Step 2: Wire `VfsOp::Realpath` into the dispatch match**

Around line 658, the dispatch match `let result = match op { … }` already has `VfsOp::ReadRing => self.handle_read_ring(...)`. Add directly after `VfsOp::Link => self.handle_link(...)` (line 654) the new arm:

```rust
            VfsOp::Realpath => self.handle_realpath(msg, payload, reply_token, authenticated_client),
```

- [ ] **Step 3: Implement `handle_realpath`**

Append this method inside the `impl VfsService` block. A clean place is right after `handle_stat` (search for `fn handle_stat(` around line 1356 and place the new handler immediately after the closing brace of that method body, before the next `fn`):

```rust
    fn handle_realpath(
        &mut self,
        msg: &Message,
        payload: &[u8],
        reply_token: usize,
        caller_client: Option<usize>,
    ) -> Result<()> {
        let mut reply_msg = Message::new(VFS_REALPATH, [0; 6], 2);
        let client_id = match self.resolve_client_id("realpath", caller_client, msg.words[1]) {
            Ok(id) => id,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let path = match core::str::from_utf8(payload) {
            Ok(p) => p,
            Err(_) => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        let real_path = match self.view_check_path(client_id, path) {
            Ok(rp) => rp,
            Err(err) => {
                reply_msg.words[0] = err.to_errno() as usize;
                return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
            }
        };
        // Forward to backend; mounts without symlinks return rel as-is.
        let canon = match self.mount_table.get_backend(&real_path) {
            Some(backend) => {
                // Compute prefix + rel that backend understands.
                let (prefix, rel) = self.mount_table.split_path(&real_path);
                match backend.realpath(rel) {
                    Ok(rel_canon) => {
                        if rel_canon.starts_with('/') {
                            // Backend returned absolute path inside its own
                            // mount; re-prefix.
                            let mut out = String::from(prefix);
                            if out.ends_with('/') {
                                out.pop();
                            }
                            out.push_str(&rel_canon);
                            out
                        } else {
                            real_path.clone()
                        }
                    }
                    Err(err) => {
                        reply_msg.words[0] = err.to_errno() as usize;
                        return ipc::reply(reply_token, &reply_msg, IpcFlags::empty());
                    }
                }
            }
            None => real_path.clone(),
        };
        let bytes = canon.into_bytes();
        reply_msg.words[0] = 0;
        reply_msg.words[1] = bytes.len();
        ipc::reply_with_payload(reply_token, &reply_msg, &bytes)
    }
```

> Note: `mount_table.split_path` may not exist yet — if not, add a tiny helper in `mount.rs` that returns `(prefix_str, rel_str)` for a given absolute path against the mount prefix list. Cross-check before assuming.

- [ ] **Step 4: Add `MountTable::split_path` helper if missing**

Run: `grep -n "fn split_path\|fn longest_prefix" /home/vlb2bp/git/cluu/userspace/vfs/src/mount.rs`
If no match, in `userspace/vfs/src/mount.rs`, in the `impl MountTable` block (search `pub fn get_backend`), add directly above `get_backend`:

```rust
    /// Split `path` into (mount-prefix, rel-within-mount). Returns `("/", path)`
    /// when no mount matches.
    pub fn split_path<'b>(&self, path: &'b str) -> (&'static str, &'b str) {
        let mut best: (&'static str, &'b str) = ("/", path);
        for (prefix, _backend) in self.mounts() {
            if path.starts_with(prefix) && prefix.len() > best.0.len() {
                let rel = &path[prefix.len()..];
                let rel = rel.trim_start_matches('/');
                best = (prefix, rel);
            }
        }
        best
    }
```

If `mounts()` accessor does not exist, add it: `pub fn mounts(&self) -> impl Iterator<Item = (&'static str, &dyn MountBackend)> { self.mounts.iter().map(|m| (m.prefix, &*m.backend)) }`. Field names must match — verify against the existing struct (`pub struct MountTable { mounts: Vec<Mount>, ... }` around line 600).

- [ ] **Step 5: Build verify**

Run: `cargo check -p vfs --target x86_64-cluu-elf`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add userspace/vfs/src/main.rs userspace/vfs/src/mount.rs
git commit -m "feat(vfs): VFS_REALPATH dispatch through view+backend

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 8: shell — image_name_from_canonical helper

**Files:**
- Modify: `userspace/shell/src/path_lookup.rs`

The new helper does the canonical-path inspection: input is the result of `vfs.realpath`. If it matches `^/var/images/([^/]+)/.+$`, return `Some(name)`. Otherwise `None`.

A second helper, `resolve_to_image_name(name, vfs)`, encapsulates the new shell-side flow used by both `pipeline.rs` and `commands/exec.rs`:

1. If `name` does not contain `/`, return `name` as-is (existing PATH-lookup path handles this).
2. Else call `vfs.realpath(name)`; on success, run `image_name_from_canonical` and return that. On failure or no match, return `name` unchanged so the caller emits a "not a CLUU image" error downstream when procmgr rejects it.

- [ ] **Step 1: Add helper `image_name_from_canonical`**

Append to `userspace/shell/src/path_lookup.rs`:

```rust
/// Pull the container image name out of a canonical absolute path, when the
/// path lives inside `/var/images/<name>/...`. Returns `None` for any other
/// shape, including the `/var/images` root itself.
pub fn image_name_from_canonical(canonical: &str) -> Option<String> {
    let rest = canonical.strip_prefix("/var/images/")?;
    let (name, tail) = rest.split_once('/')?;
    if name.is_empty() || tail.is_empty() {
        return None;
    }
    Some(String::from(name))
}

/// Convert a user-typed command word into the bare image name procmgr
/// expects. Bare names pass through; paths-with-slashes are resolved via
/// `vfs.realpath` and then matched against `/var/images/<name>/...`.
/// Returns the original input unchanged when realpath fails or the
/// canonical path does not look like a CLUU image binary; the caller is
/// responsible for downstream error reporting.
pub fn resolve_to_image_name(name: &str, vfs: &VfsClient) -> String {
    if !name.contains('/') {
        return String::from(name);
    }
    match vfs.realpath(name) {
        Ok(canon) => image_name_from_canonical(&canon).unwrap_or_else(|| String::from(name)),
        Err(_) => String::from(name),
    }
}
```

- [ ] **Step 2: Add a small `cargo test` style assertion (no_std, so use `#[cfg(test)]` only at host)**

`userspace/shell` is `no_std`, so Rust unit tests don't run here. Skip; coverage comes from the harness in Task 11.

- [ ] **Step 3: Build verify**

Run: `cargo check -p shell --target x86_64-cluu-elf`
Expected: clean. The new function is `pub`; the unused-import warning for `String` is already silenced because `String` is imported at the top.

- [ ] **Step 4: Commit**

```bash
git add userspace/shell/src/path_lookup.rs
git commit -m "feat(shell): image_name_from_canonical + resolve_to_image_name

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 9: shell — drop the four /bin/ strip sites

**Files:**
- Modify: `userspace/shell/src/pipeline.rs`

All four sites currently look like:

```rust
let name = argv[0].as_str();
let image_name = name.strip_prefix("/bin/").unwrap_or(name);
```

Replace each with the new `resolve_to_image_name` helper, which needs a `VfsClient`. We borrow the same one already obtained for shell ops; if a stage runs before any VFS handle is set up, fall back to passing the raw name through (procmgr will reject and the user sees a clear error per Task 10).

- [ ] **Step 1: Add a single VfsClient lookup at the top of `run_single_with_redirs` and `run_multi`**

Both methods need to build a `VfsClient` once and pass it to the new helper. Add at the start of each (right after the early-return for empty argv, before the first `image_name` computation):

```rust
let vfs_client = libcluu::registry::subscribe_output("vfs", "main")
    .ok()
    .and_then(|ep| libcluu::fs::client::VfsClient::new_from_registry(ep).ok());
```

- [ ] **Step 2: Replace all four `strip_prefix("/bin/")` lines**

Lines 88, 351, 368, 474 in the current pipeline.rs.

For each occurrence, replace:

```rust
let image_name = name.strip_prefix("/bin/").unwrap_or(name);
```

With:

```rust
let image_name_owned = match vfs_client.as_ref() {
    Some(vfs) => crate::path_lookup::resolve_to_image_name(name, vfs),
    None => alloc::string::String::from(name),
};
let image_name = image_name_owned.as_str();
```

> Note: `image_name` is currently used as `&str` — keeping that type avoids cascading lifetime changes. The owned `image_name_owned` lives until the end of the loop iteration / function so all reborrows are sound.

- [ ] **Step 3: Build verify**

Run: `cargo check -p shell --target x86_64-cluu-elf`
Expected: clean. If borrow-checker complains, ensure `image_name_owned` is declared in the same scope as `image_name` and lives at least as long.

- [ ] **Step 4: Commit**

```bash
git add userspace/shell/src/pipeline.rs
git commit -m "refactor(shell): VFS realpath replaces /bin/ substring strip

Drops the four hard-coded strip_prefix(\"/bin/\") sites in pipeline.rs.
All path-with-slash inputs now flow through VfsClient::realpath and
match against /var/images/<name>/..., per item #1 of the open-work
queue.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 10: procmgr — defensive image_name validation

**Files:**
- Modify: `userspace/procmgr/src/main.rs`

`handle_container_run` already extracts `image_name` from the payload (line ~4736). Add a defensive check: if it contains `/`, reject with InvalidArgument and a clear log line. This is defence-in-depth — the shell now always sends bare names — but catches future regressions or non-shell callers.

- [ ] **Step 1: Add the slash check**

In `userspace/procmgr/src/main.rs`, immediately after the `image_name.is_empty()` check (around line 4763), insert:

```rust
        if image_name.contains('/') {
            let _ = debug_print(&format!(
                "procmgr: container_run rejected: image name '{}' contains '/' (use bare name; resolve symlinks shell-side)",
                image_name
            ));
            reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }
```

- [ ] **Step 2: Build verify**

Run: `cargo check -p procmgr --target x86_64-cluu-elf`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "feat(procmgr): reject container image names containing '/'

Defence in depth — shell now always resolves /bin/<name> via VFS
realpath and sends bare image names. Reject paths with a clear
log so future regressions surface immediately.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 11: harness — l2_path_symlink_resolve marker case

**Files:**
- Modify: `scripts/harness_case_defaults.sh`

Add a marker case that types `/bin/ls /` at the shell prompt and asserts that the listing contains `bin` (so the symlink resolved through to the real `ls` binary). Existing `l2_bare_cmd` covers the no-slash flow; this one covers the new with-slash path.

- [ ] **Step 1: Add the marker case**

In `scripts/harness_case_defaults.sh`, after the `l2_bare_cmd) … ;;` block (line 67), insert:

```bash
            l2_path_symlink_resolve)
                TEST_COMMAND=""
                # Item #1 of open-work queue: /bin/ls is now a real ext2
                # symlink that resolves through VFS realpath instead of the
                # legacy strip_prefix("/bin/") hack. Listing root must show
                # at least the bin entry the symlink itself lives in.
                SHELL_AUTOSTART_CMD_DEFAULT="/bin/ls /"
                EXPECTED_CONTAINS=("bin")
                ;;
```

- [ ] **Step 2: Run the harness**

Run from repo root:

```bash
MARKER_MODE=l2_path_symlink_resolve TEST_COMMAND=__AUTO__ ./scripts/harness_run.sh
```

Expected: harness reports PASS and the serial log shows the listing of `/` including `bin`.

- [ ] **Step 3: Confirm `l2_bare_cmd` still passes (regression guard)**

Run:

```bash
MARKER_MODE=l2_bare_cmd TEST_COMMAND=__AUTO__ ./scripts/harness_run.sh
```

Expected: PASS, with the procmgr container start marker for `cat`.

- [ ] **Step 4: Commit**

```bash
git add scripts/harness_case_defaults.sh
git commit -m "test(harness): l2_path_symlink_resolve marker

Covers the new VFS realpath flow that replaced the four
strip_prefix(\"/bin/\") sites in shell pipeline.rs.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Task 12: full clean build + harness sweep

- [ ] **Step 1: Clean build**

Run from repo root:

```bash
rm -rf target/newlib-build target/sysroot/x86_64-cluu-elf
make clean
cargo xtask build-newlib
cargo xtask build-syscalls
cargo xtask build-crt0
cargo xtask build
```

Expected: build completes, no errors.

- [ ] **Step 2: Smoke test the new case**

Run: `MARKER_MODE=l2_path_symlink_resolve TEST_COMMAND=__AUTO__ ./scripts/harness_run.sh`
Expected: PASS.

- [ ] **Step 3: Run a representative slice of the harness suite**

Cases to run for regression coverage (each as MARKER_MODE):

- `l2_bare_cmd` — PATH dispatch unchanged
- `l2_which_basic` — `which ls` still prints `/bin/ls`
- `l2_owner_deny` — pre-existing flaky case; record current state, do NOT block on regression here
- `l2_ext2write`, `l2_ext2append`, `l2_ext2mutate`, `l2_ext2unlink` — ext2 path resolve unchanged
- `l2_mp_etc` — MicroPython end-to-end still works
- `l2_edit_smoke` — editor dispatch still works

Run sequentially:

```bash
for m in l2_bare_cmd l2_which_basic l2_ext2write l2_ext2append l2_ext2mutate l2_ext2unlink l2_mp_etc l2_edit_smoke; do
    echo "=== $m ==="
    MARKER_MODE=$m TEST_COMMAND=__AUTO__ ./scripts/harness_run.sh || echo "FAIL: $m"
done
```

Expected: each prints its own PASS marker. Any FAIL line points at a regression that must be fixed before declaring the plan done.

- [ ] **Step 4: Update memory**

Open `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_open_work_2026_05_09.md` and remove item 1 (it's done). Move item 2 (re-enable PRELOAD activation) to the top.

Open `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/MEMORY.md` index — no entry change required; the open-work memory is already pointed to.

- [ ] **Step 5: Final commit (if memory updates landed)**

Memory files live outside the repo; no `git add` needed. Done.

---

## Self-review notes

- All four `strip_prefix("/bin/")` sites identified at lines 88, 351, 368, 474 in `userspace/shell/src/pipeline.rs` are touched in Task 9.
- `Filesystem::resolve_path` default trait impl in `userspace/libcluu/src/fs/traits.rs:93` is a non-following walker; Task 2 overrides it on `Ext2Fs` so all callers see symlink-following behaviour.
- `MountTarget::MemFs` paths in VFS bypass the backend lookup at `handle_open` line ~1083; `handle_realpath` (Task 7) deliberately follows the same `view_check_path` indirection so memfs paths emerge unchanged through the default `MountBackend::realpath` impl.
- The `image_name` extraction in procmgr at line ~4736 reads up to either `fdac_offset`, `param_offset`, or `effective_payload.len()`. The slash-check (Task 10) runs after that boundary is applied, so it sees the cleaned name string.
- Harness case `l2_path_symlink_resolve` (Task 11) uses `EXPECTED_CONTAINS=("bin")` rather than a procmgr debug marker because `ls /` writes its output to the TTY, which the harness scrapes from COM2 as well — same anchor strategy as `l2_dirname_basic`.
