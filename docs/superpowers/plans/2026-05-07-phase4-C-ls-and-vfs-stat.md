# Phase 4 Plan C — ls Deep Redesign + Extended VfsStat

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bump VFS protocol so `VfsStat` carries mtime/nlink/uid/gid/blocks; have `readdir` return `(name, stat)` pairs in one round trip; rebuild `ls` from the bare 53-LOC version into a ~450-LOC GNU-close `ls` with long mode, columns, colors, and sort flags.

**Architecture:** (1) Extend the VFS wire protocol to include extended stat fields, with the ext2 backend reading them from inode and the procfs/devfs/memfs backends supplying defaults. (2) Bump `VfsClient::readdir` to return entries with stats batched. (3) Rewrite `userspace/ls/src/main.rs` on top of the new client API plus `libcluu::cli`. Components: `ls/format.rs` (mode bits, time, size, color), `ls/columns.rs` (column layout), `ls/sort.rs` (comparators).

**Tech Stack:** Rust `no_std`, `libcluu`, ext2 inode reader (already in `userspace/ext2/`).

**Spec reference:** `docs/superpowers/specs/2026-05-07-userspace-polish-design.md` §5.4

**Prereq:** Plan A merged. Plan B's cli.rs (Stage 1) is a soft dependency — ls uses it. Either run Plan B Stage 1 first, or write ls's arg loop ad-hoc and migrate later.

---

## File structure

### Created
- `userspace/ls/src/format.rs` (mode bits, size formatter, time formatter, color)
- `userspace/ls/src/columns.rs` (terminal-width column layout)
- `userspace/ls/src/sort.rs` (comparators)
- `userspace/libcluu/tests/format_test.rs` (host-side tests for the formatters)

### Modified
- `userspace/libcluu/src/fs/protocol.rs` (VfsStat extended fields, wire format bump)
- `userspace/libcluu/src/fs/client.rs` (`VfsStat` struct, `VfsDirEntry` carries `stat`, `readdir` parses new wire format)
- `userspace/vfs/src/main.rs` (server-side: emit extended stat in readdir + stat replies)
- `userspace/vfs/src/backends/ext2.rs` (inode → extended stat)
- `userspace/vfs/src/backends/{ramfs,memfs,procfs,devfs}.rs` (fill defaults)
- `userspace/ls/Cargo.toml` (depend on libcluu cli feature if needed)
- `userspace/ls/src/main.rs` (full rewrite ~250 LOC)
- `scripts/harness_cases.conf` (add l2_ls_long, l2_ls_color, l2_ls_recursive)

---

## Stage 1 — VFS protocol bump

### Task 1.1: Find the wire format

**Files:**
- Read-only audit

- [ ] **Step 1: Locate VFS readdir reply format**

```bash
grep -n 'VFS_READDIR\|readdir\|fn handle_readdir\|ReaddirReply' userspace/vfs/src/main.rs userspace/libcluu/src/fs/*.rs | head -20
```

Capture: which message label, what payload format (currently a list of `(name_len, name_bytes, is_dir_byte)` tuples or similar).

- [ ] **Step 2: Locate the stat reply format**

```bash
grep -n 'VFS_STAT\|fn handle_stat\|StatReply' userspace/vfs/src/main.rs userspace/libcluu/src/fs/*.rs
```

Capture current reply format. Today: `words[1]=size`, `words[2]=mode`.

### Task 1.2: Define extended VfsStat

**Files:**
- Modify: `userspace/libcluu/src/fs/client.rs`

- [ ] **Step 1: Replace the existing VfsStat with the extended version**

```rust
/// File metadata returned by stat/fstat. Wire format v2.
#[derive(Debug, Clone, Copy, Default)]
pub struct VfsStat {
    pub size:   u64,
    pub mode:   u32,    // S_IFMT | perms
    pub mtime:  u64,    // unix seconds
    pub nlink:  u32,
    pub uid:    u32,
    pub gid:    u32,
    pub blocks: u64,    // 512-byte units
}
```

- [ ] **Step 2: Update VfsDirEntry to carry stat**

```rust
/// Directory entry returned by readdir. Wire format v2.
#[derive(Debug, Clone, Default)]
pub struct VfsDirEntry {
    pub name: String,
    pub stat: VfsStat,
    pub is_dir: bool,   // duplicates stat.mode S_IFDIR for backward call sites
}
```

`is_dir` retained so existing callers (current bare ls) keep compiling during the migration.

### Task 1.3: Bump server-side stat reply

**Files:**
- Modify: `userspace/vfs/src/main.rs`

- [ ] **Step 1: Find the current stat reply**

Existing reply has `words[1]=size, words[2]=mode`. Switch to a payload-based reply with the full stat struct serialized at known offsets:

```
reply.words[0] = status
reply.words[1] = payload_len
payload bytes:
  [0..8]   size (u64 LE)
  [8..12]  mode (u32 LE)
  [12..20] mtime (u64 LE)
  [20..24] nlink (u32 LE)
  [24..28] uid (u32 LE)
  [28..32] gid (u32 LE)
  [32..40] blocks (u64 LE)
```

40 bytes total per stat. Serialize with `to_le_bytes()` per field.

- [ ] **Step 2: Bump VFS protocol version constant**

```bash
grep -n 'VFS_PROTOCOL_VERSION\|VFS_PROTO_VERSION' userspace/libcluu/src/fs/*.rs userspace/vfs/src/main.rs
```

If a version constant exists, bump it. Otherwise add:

```rust
pub const VFS_PROTO_VERSION: u32 = 2;
```

in `userspace/libcluu/src/fs/protocol.rs`. Document the version 1 → 2 change in a comment listing the new payload layout.

- [ ] **Step 3: Update server stat handler to emit the 40-byte payload**

In whichever fn handles `VFS_STAT`:

```rust
let mut buf = [0u8; 40];
buf[0..8].copy_from_slice(&stat.size.to_le_bytes());
buf[8..12].copy_from_slice(&stat.mode.to_le_bytes());
buf[12..20].copy_from_slice(&stat.mtime.to_le_bytes());
buf[20..24].copy_from_slice(&stat.nlink.to_le_bytes());
buf[24..28].copy_from_slice(&stat.uid.to_le_bytes());
buf[28..32].copy_from_slice(&stat.gid.to_le_bytes());
buf[32..40].copy_from_slice(&stat.blocks.to_le_bytes());
reply_with_payload(reply, &buf, 0 /* status ok */);
```

### Task 1.4: Bump server-side readdir reply

**Files:**
- Modify: `userspace/vfs/src/main.rs`

- [ ] **Step 1: New per-entry wire format**

Each entry: `[name_len: u32 LE][stat 40 bytes][name: name_len bytes]`. Total per entry = 44 + name_len.

- [ ] **Step 2: Update readdir handler**

For each directory entry:
```rust
let mut entry_buf = Vec::with_capacity(44 + name.len());
entry_buf.extend_from_slice(&(name.len() as u32).to_le_bytes());
entry_buf.extend_from_slice(&serialize_stat(&entry_stat));
entry_buf.extend_from_slice(name.as_bytes());
payload.extend_from_slice(&entry_buf);
```

Where `serialize_stat` is the 40-byte serializer from Task 1.3 step 3.

- [ ] **Step 3: Update reply word count**

`reply.words[0] = status`, `reply.words[1] = entry_count`, `reply.words[2] = payload_len`.

### Task 1.5: Backend changes

**Files:**
- Modify: `userspace/vfs/src/backends/ext2.rs`
- Modify: `userspace/vfs/src/backends/ramfs.rs`
- Modify: `userspace/vfs/src/backends/memfs.rs` (if exists)
- Modify: `userspace/vfs/src/backends/procfs.rs`
- Modify: `userspace/vfs/src/backends/devfs.rs` (if exists)

- [ ] **Step 1: ext2 backend — read full inode**

In `ext2.rs::stat`, the inode struct already has `size`, `mode`, `mtime`, `links_count`, `uid`, `gid`, `blocks`. Map directly:

```rust
VfsStat {
    size:   inode.size_low as u64 | ((inode.size_high as u64) << 32),
    mode:   inode.mode as u32,
    mtime:  inode.mtime as u64,
    nlink:  inode.links_count as u32,
    uid:    inode.uid as u32,
    gid:    inode.gid as u32,
    blocks: inode.blocks as u64,
}
```

In `ext2.rs::readdir`, fetch each entry's inode and emit a populated stat. Cost = N inode reads; ext2 has block cache so this is acceptable.

- [ ] **Step 2: ramfs / memfs backend — supply defaults**

```rust
VfsStat {
    size:   node.data.len() as u64,
    mode:   if node.is_dir { 0o040755 } else { 0o100644 },
    mtime:  0,    // memfs has no time; placeholder until timeserver wiring
    nlink:  1,
    uid:    0,
    gid:    0,
    blocks: (node.data.len() as u64 + 511) / 512,
}
```

- [ ] **Step 3: procfs backend**

ProcfsBackend already calls procmgr for status info. Add stat reply with synthetic mode (0o040555 for directories like /proc/<pid>/, 0o100444 for files like /proc/<pid>/stat), nlink=1, uid/gid=0, mtime=0, size=actual content length.

- [ ] **Step 4: devfs / device backend**

`/dev/null /dev/zero /dev/urandom`: mode 0o020666 (S_IFCHR | rw-rw-rw-), size 0, others 0.

### Task 1.6: Client-side parsing of new wire format

**Files:**
- Modify: `userspace/libcluu/src/fs/client.rs`

- [ ] **Step 1: New stat parser**

```rust
pub fn stat(&self, path: &str) -> Result<VfsStat> {
    let payload = path.as_bytes();
    let msg = make_payload_message(VFS_STAT, payload.len(), &[self.client_id]);
    let mut reply = Message::new(0, [0; 6], 0);
    let reply_payload = ipc::call_with_payload_recv_payload(self.endpoint, &msg, payload, &mut reply, 40)?;
    parse_status(reply.words[0])?;
    Ok(deserialize_stat(&reply_payload))
}

fn deserialize_stat(buf: &[u8]) -> VfsStat {
    let mut a = [0u8; 8];
    a.copy_from_slice(&buf[0..8]);
    let size = u64::from_le_bytes(a);
    let mut b = [0u8; 4];
    b.copy_from_slice(&buf[8..12]);
    let mode = u32::from_le_bytes(b);
    a.copy_from_slice(&buf[12..20]);
    let mtime = u64::from_le_bytes(a);
    b.copy_from_slice(&buf[20..24]);
    let nlink = u32::from_le_bytes(b);
    b.copy_from_slice(&buf[24..28]);
    let uid = u32::from_le_bytes(b);
    b.copy_from_slice(&buf[28..32]);
    let gid = u32::from_le_bytes(b);
    a.copy_from_slice(&buf[32..40]);
    let blocks = u64::from_le_bytes(a);
    VfsStat { size, mode, mtime, nlink, uid, gid, blocks }
}
```

(`call_with_payload_recv_payload` is the existing IPC primitive that returns the reply payload bytes. If the current API is different, adapt; the libcluu IPC layer already supports payload+reply-payload.)

- [ ] **Step 2: New readdir parser**

```rust
pub fn readdir(&self, path: &str) -> Result<Vec<VfsDirEntry>> {
    let payload = path.as_bytes();
    let msg = make_payload_message(VFS_READDIR, payload.len(), &[self.client_id]);
    let mut reply = Message::new(0, [0; 6], 0);
    let reply_payload = ipc::call_with_payload_recv_payload(self.endpoint, &msg, payload, &mut reply, 64 * 1024)?;
    parse_status(reply.words[0])?;
    let count = reply.words[1];
    let mut out = Vec::with_capacity(count);
    let mut cursor = 0usize;
    for _ in 0..count {
        let mut len_buf = [0u8; 4];
        len_buf.copy_from_slice(&reply_payload[cursor..cursor+4]);
        let name_len = u32::from_le_bytes(len_buf) as usize;
        cursor += 4;
        let stat = deserialize_stat(&reply_payload[cursor..cursor+40]);
        cursor += 40;
        let name = core::str::from_utf8(&reply_payload[cursor..cursor+name_len])
            .map_err(|_| Error::Bad)?
            .to_string();
        cursor += name_len;
        let is_dir = (stat.mode & 0o170000) == 0o040000;
        out.push(VfsDirEntry { name, stat, is_dir });
    }
    Ok(out)
}
```

### Task 1.7: Verify backward compat for non-ls callers

**Files:**
- Audit only

- [ ] **Step 1: Find callers of readdir**

```bash
grep -rn '\.readdir(' userspace/ 2>/dev/null
```

- [ ] **Step 2: Each caller still compiles**

Most callers only use `entry.name` and `entry.is_dir`. New `entry.stat` is additive; the `VfsDirEntry` struct still has both. Build to confirm:

```bash
cargo xtask build 2>&1 | tail -20
```

If a caller broke, fix the field access (e.g. `entry.stat.size` instead of `entry.size`).

### Task 1.8: Run protocol smoke + commit

- [ ] **Step 1: Run l2_ls and l2_mp_etc (touches stat heavily)**

```bash
bash scripts/harness_run.sh l2_ls 2>&1 | tail -3
bash scripts/harness_run.sh l2_mp_etc 2>&1 | tail -3
```

PASS both.

- [ ] **Step 2: Commit Stage 1**

```bash
git add userspace/libcluu/src/fs/ userspace/vfs/
git commit -m "$(cat <<'EOF'
feat(vfs): bump protocol to v2 — extended VfsStat + batched readdir

VfsStat gains mtime/nlink/uid/gid/blocks. readdir now returns
(name, stat) pairs in a single round trip, eliminating the N+1
stat pattern for ls -l, du, find. ext2 backend reads from inode;
ramfs/procfs/devfs supply sensible defaults.

Phase 4 Plan C Stage 1.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 2 — ls rewrite

### Task 2.1: Format helpers — failing test

**Files:**
- Create: `userspace/ls/src/format.rs`
- Create: `userspace/ls/tests/format_test.rs`

ls needs `cargo test` for the formatters. Add a host-test feature mirror of what libcluu does.

- [ ] **Step 1: Update Cargo.toml**

```toml
[features]
default = []
host-test = []

[[test]]
name = "format_test"
path = "tests/format_test.rs"
required-features = ["host-test"]
```

- [ ] **Step 2: Failing test for mode rendering**

```rust
// tests/format_test.rs
#![cfg(feature = "host-test")]

use cluu_ls::format::render_mode;

#[test]
fn mode_dir_rwxr_xr_x() {
    assert_eq!(render_mode(0o040755), "drwxr-xr-x");
}

#[test]
fn mode_file_rw_r__r__() {
    assert_eq!(render_mode(0o100644), "-rw-r--r--");
}

#[test]
fn mode_exec_rwxr_x_r__() {
    assert_eq!(render_mode(0o100754), "-rwxr-xr--");
}

#[test]
fn mode_symlink() {
    assert_eq!(render_mode(0o120777), "lrwxrwxrwx");
}
```

- [ ] **Step 3: Run, verify FAIL**

```bash
cargo test -p cluu-ls --features host-test --test format_test
```

Compile error — module/function does not exist. Fail loud.

### Task 2.2: Implement render_mode

```rust
// userspace/ls/src/format.rs

extern crate alloc;
use alloc::string::String;

const S_IFMT:  u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
const S_IFCHR: u32 = 0o020000;
const S_IFBLK: u32 = 0o060000;
const S_IFIFO: u32 = 0o010000;
const S_IFSOCK:u32 = 0o140000;

pub fn render_mode(mode: u32) -> String {
    let mut s = String::with_capacity(10);
    s.push(match mode & S_IFMT {
        S_IFDIR  => 'd',
        S_IFREG  => '-',
        S_IFLNK  => 'l',
        S_IFCHR  => 'c',
        S_IFBLK  => 'b',
        S_IFIFO  => 'p',
        S_IFSOCK => 's',
        _        => '?',
    });
    let bits = [
        (0o400, 'r'), (0o200, 'w'), (0o100, 'x'),
        (0o040, 'r'), (0o020, 'w'), (0o010, 'x'),
        (0o004, 'r'), (0o002, 'w'), (0o001, 'x'),
    ];
    for (mask, ch) in bits {
        s.push(if mode & mask != 0 { ch } else { '-' });
    }
    s
}
```

Add a `lib.rs` so format/columns/sort can be unit-tested:

```rust
// userspace/ls/src/lib.rs
#![cfg_attr(not(feature = "host-test"), no_std)]
extern crate alloc;
pub mod format;
pub mod columns;
pub mod sort;
```

Adjust `Cargo.toml`:

```toml
[lib]
name = "cluu_ls"
path = "src/lib.rs"

[[bin]]
name = "ls"
path = "src/main.rs"
```

- [ ] **Step 1: Run tests, verify all 4 mode tests PASS**

```bash
cargo test -p cluu-ls --features host-test --test format_test
```

PASS.

### Task 2.3: Size formatter

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn size_human_kib() {
    assert_eq!(cluu_ls::format::render_size(1024, true), "1.0K");
}
#[test]
fn size_human_mib() {
    assert_eq!(cluu_ls::format::render_size(1024 * 1024 * 3 + 100_000, true), "3.1M");
}
#[test]
fn size_raw() {
    assert_eq!(cluu_ls::format::render_size(1024, false), "1024");
}
```

- [ ] **Step 2: Implement**

```rust
pub fn render_size(bytes: u64, human: bool) -> String {
    if !human {
        return format!("{}", bytes);
    }
    let units = ["", "K", "M", "G", "T"];
    let mut v = bytes as f64;
    let mut idx = 0;
    while v >= 1024.0 && idx < units.len() - 1 {
        v /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{}", bytes)
    } else {
        format!("{:.1}{}", v, units[idx])
    }
}
```

- [ ] **Step 3: Tests PASS.**

### Task 2.4: Time formatter

- [ ] **Step 1: Failing tests**

```rust
#[test]
fn time_recent_uses_hhmm() {
    // Within 6 months: "Mmm DD HH:MM"
    let now: u64 = 1_700_000_000;       // 2023-11-14 22:13:20 UTC
    let t:   u64 = 1_699_500_000;       // 2023-11-09 04:40:00 UTC
    let s = cluu_ls::format::render_time(t, now);
    assert_eq!(s.len(), 12);            // "Nov 09 04:40"
    assert!(s.starts_with("Nov 09"));
}
```

- [ ] **Step 2: Implement (no-std friendly date math, no chrono)**

```rust
pub fn render_time(t: u64, now: u64) -> String {
    let half_year = 60 * 60 * 24 * 30 * 6;  // approx
    let recent = now.saturating_sub(t) < half_year && t <= now;
    let (y, m, d, hh, mm) = unix_to_components(t);
    let months = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"];
    if recent {
        format!("{} {:02} {:02}:{:02}", months[(m-1) as usize], d, hh, mm)
    } else {
        format!("{} {:02}  {}", months[(m-1) as usize], d, y)
    }
}

fn unix_to_components(t: u64) -> (u32, u32, u32, u32, u32) {
    // Days since 1970-01-01.
    let days = t / 86_400;
    let mut secs = t % 86_400;
    let hh = (secs / 3600) as u32; secs %= 3600;
    let mm = (secs / 60) as u32;
    // Year/month/day via civil_from_days (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe/1460 + doe/36524 - doe/146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365*yoe + yoe/4 - yoe/100);
    let mp = (5*doy + 2)/153;
    let d = (doy - (153*mp + 2)/5 + 1) as u32;
    let m = if mp < 10 { (mp + 3) as u32 } else { (mp - 9) as u32 };
    let y = if m <= 2 { y + 1 } else { y } as u32;
    (y, m, d, hh, mm)
}
```

- [ ] **Step 3: Add second test for old-time format**

```rust
#[test]
fn time_old_uses_year() {
    let now: u64 = 1_700_000_000;
    let t:   u64 = 1_500_000_000;       // ~2.5 years older
    let s = cluu_ls::format::render_time(t, now);
    assert!(s.contains("2017") || s.contains("2018"));
}
```

PASS.

### Task 2.5: Color renderer

- [ ] **Step 1: Failing test**

```rust
#[test]
fn color_dir_blue() {
    let s = cluu_ls::format::colorize("d", "etc", true);
    assert!(s.contains("\x1b[1;34m"));
    assert!(s.ends_with("\x1b[0m"));
}
#[test]
fn color_disabled_when_not_tty() {
    let s = cluu_ls::format::colorize("d", "etc", false);
    assert_eq!(s, "etc");
}
```

- [ ] **Step 2: Implement**

```rust
pub fn colorize(kind: &str, name: &str, enable: bool) -> String {
    if !enable { return name.into(); }
    let prefix = match kind {
        "d" => "\x1b[1;34m",
        "x" => "\x1b[1;32m",
        "l" => "\x1b[1;36m",
        _   => "",
    };
    if prefix.is_empty() { name.into() } else { format!("{}{}\x1b[0m", prefix, name) }
}

pub fn classify(stat: &cluu_libcluu::fs::client::VfsStat) -> &'static str {
    let m = stat.mode;
    if (m & 0o170000) == 0o040000 { return "d"; }
    if (m & 0o170000) == 0o120000 { return "l"; }
    if m & 0o111 != 0             { return "x"; }
    "f"
}
```

- [ ] **Step 3: Tests PASS.**

### Task 2.6: Column layout

**Files:**
- Create: `userspace/ls/src/columns.rs`

- [ ] **Step 1: Test**

```rust
#[test]
fn columns_three_per_row() {
    let names = vec!["aa".into(), "bb".into(), "cc".into(), "dd".into(), "ee".into()];
    let out = cluu_ls::columns::layout(&names, 12);
    // Width 12, gap 2 → max 4 cols of width 2; expect 4 then 1.
    assert!(out.lines().count() == 2);
}
```

- [ ] **Step 2: Implement**

```rust
extern crate alloc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

pub fn layout(names: &[String], width: usize) -> String {
    if names.is_empty() { return String::new(); }
    let max = names.iter().map(|n| n.len()).max().unwrap_or(0) + 2;
    let cols = (width / max).max(1);
    let rows = (names.len() + cols - 1) / cols;
    let mut out = String::new();
    for r in 0..rows {
        for c in 0..cols {
            let idx = c * rows + r;
            if idx >= names.len() { break; }
            let n = &names[idx];
            out.push_str(n);
            if c < cols - 1 && idx + rows < names.len() {
                let pad = max - n.len();
                for _ in 0..pad { out.push(' '); }
            }
        }
        out.push('\n');
    }
    out
}
```

- [ ] **Step 3: PASS.**

### Task 2.7: Sort comparators

**Files:**
- Create: `userspace/ls/src/sort.rs`

```rust
extern crate alloc;
use alloc::vec::Vec;
use cluu_libcluu::fs::client::VfsDirEntry;

pub enum SortKey { Name, Size, Mtime }

pub fn sort_entries(entries: &mut Vec<VfsDirEntry>, key: SortKey, reverse: bool) {
    match key {
        SortKey::Name  => entries.sort_by(|a, b| a.name.cmp(&b.name)),
        SortKey::Size  => entries.sort_by(|a, b| b.stat.size.cmp(&a.stat.size)),
        SortKey::Mtime => entries.sort_by(|a, b| b.stat.mtime.cmp(&a.stat.mtime)),
    }
    if reverse { entries.reverse(); }
}
```

- [ ] **Step 1: Tests for each key + reverse, PASS.**

### Task 2.8: Wire into main.rs

**Files:**
- Modify: `userspace/ls/src/main.rs`

- [ ] **Step 1: Replace existing main.rs (53 LOC) with full version**

```rust
#![no_std]
#![no_main]

extern crate alloc;
#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use libcluu::cli::{Spec, parse, render_help, CliError};
use libcluu::fs::client::{VfsClient, VfsDirEntry};
use libcluu::posix::{_write, isatty, current_dir_string, getenv};
use libcluu::registry;
use libcluu::time::current_time;
use cluu_ls::{format as fmt, columns, sort as srt};

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let argv: Vec<String> = libcluu::args::args();
    let spec = Spec::new()
        .program("ls")
        .usage("[OPTION]... [FILE]...")
        .flag('1', "one-per-line", "single column")
        .flag('l', "long", "long listing format")
        .flag('a', "all", "include hidden files")
        .flag('h', "human-readable", "1.2K, 3.4M etc")
        .flag('R', "recursive", "")
        .flag('S', "sort-size", "sort by size")
        .flag('t', "sort-time", "sort by mtime")
        .flag('r', "reverse", "reverse sort order")
        .optional(' ', "color", "always|never|auto");

    let parsed = match parse(&spec, &argv) {
        Ok(p) => p,
        Err(CliError::HelpRequested)    => { let h = render_help(&spec); let _ = _write(1, h.as_ptr() as *const _, h.len()); return 0; }
        Err(CliError::VersionRequested) => { let v = "ls 0.1.0\n".to_string(); let _ = _write(1, v.as_ptr() as *const _, v.len()); return 0; }
        Err(e) => { let m = format!("ls: {}\n", e); let _ = _write(2, m.as_ptr() as *const _, m.len()); return 2; }
    };

    let opts = LsOpts {
        one_col:   parsed.is_set("one-per-line"),
        long:      parsed.is_set("long"),
        all:       parsed.is_set("all"),
        human:     parsed.is_set("human-readable"),
        recursive: parsed.is_set("recursive"),
        reverse:   parsed.is_set("reverse"),
        sort:      if parsed.is_set("sort-size") { srt::SortKey::Size }
                   else if parsed.is_set("sort-time") { srt::SortKey::Mtime }
                   else { srt::SortKey::Name },
        color:     resolve_color(parsed.value("color")),
    };

    let paths = if parsed.positional.is_empty() { vec![current_dir_string()] } else { parsed.positional.clone() };

    let Ok(vfs_endpoint) = registry::subscribe_output("vfs", "main") else {
        let m = b"ls: vfs not available\n";
        let _ = _write(2, m.as_ptr() as *const _, m.len());
        return 1;
    };
    let vfs = match VfsClient::new_from_registry(vfs_endpoint) {
        Ok(c) => c,
        Err(_) => { let m = b"ls: failed to create vfs client\n"; let _ = _write(2, m.as_ptr() as *const _, m.len()); return 1; }
    };

    let mut rc = 0;
    for path in &paths {
        if list_path(&vfs, path, &opts) != 0 { rc = 1; }
    }
    rc
}

struct LsOpts {
    one_col: bool, long: bool, all: bool, human: bool, recursive: bool, reverse: bool,
    sort: srt::SortKey, color: bool,
}

fn resolve_color(value: Option<&str>) -> bool {
    if getenv("NO_COLOR").is_some() { return false; }
    match value {
        Some("never")  => false,
        Some("always") => true,
        _              => isatty(1),
    }
}

fn list_path(vfs: &VfsClient, path: &str, opts: &LsOpts) -> i32 {
    let resolved = libcluu::posix::resolve_path(path);
    let mut entries = match vfs.readdir(&resolved) {
        Ok(e) => e,
        Err(e) => { let m = format!("ls: {}: {:?}\n", path, e); let _ = _write(2, m.as_ptr() as *const _, m.len()); return 1; }
    };
    if !opts.all { entries.retain(|e| !e.name.starts_with('.')); }
    srt::sort_entries(&mut entries, match opts.sort { srt::SortKey::Name => srt::SortKey::Name, srt::SortKey::Size => srt::SortKey::Size, srt::SortKey::Mtime => srt::SortKey::Mtime }, opts.reverse);

    if opts.long {
        emit_long(&entries, opts);
    } else if opts.one_col {
        for e in &entries {
            let name = fmt::colorize(fmt::classify(&e.stat), &e.name, opts.color);
            let _ = _write(1, name.as_ptr() as *const _, name.len());
            let _ = _write(1, b"\n".as_ptr() as *const _, 1);
        }
    } else {
        let names: Vec<String> = entries.iter().map(|e| fmt::colorize(fmt::classify(&e.stat), &e.name, opts.color)).collect();
        let out = columns::layout(&names, term_width());
        let _ = _write(1, out.as_ptr() as *const _, out.len());
    }

    if opts.recursive {
        for e in entries.iter().filter(|e| (e.stat.mode & 0o170000) == 0o040000 && e.name != "." && e.name != "..") {
            let sub = format!("{}/{}", resolved.trim_end_matches('/'), e.name);
            let header = format!("\n{}:\n", sub);
            let _ = _write(1, header.as_ptr() as *const _, header.len());
            list_path(vfs, &sub, opts);
        }
    }
    0
}

fn emit_long(entries: &[VfsDirEntry], opts: &LsOpts) {
    let now = current_time();
    for e in entries {
        let mode = fmt::render_mode(e.stat.mode);
        let size = fmt::render_size(e.stat.size, opts.human);
        let when = fmt::render_time(e.stat.mtime, now);
        let name = fmt::colorize(fmt::classify(&e.stat), &e.name, opts.color);
        let line = format!("{} {:>3} {:>5} {:>5} {:>8} {} {}\n",
            mode, e.stat.nlink, e.stat.uid, e.stat.gid, size, when, name);
        let _ = _write(1, line.as_ptr() as *const _, line.len());
    }
}

fn term_width() -> usize {
    if let Some(c) = getenv("COLUMNS").and_then(|s| s.parse::<usize>().ok()) {
        return c;
    }
    80
}
```

- [ ] **Step 2: Build**

```bash
cargo xtask build 2>&1 | tail -10
```

If any libcluu primitive is missing (`isatty`, `current_dir_string`, `current_time`, `getenv`, `resolve_path`), add a thin wrapper to libcluu (5-30 LOC each). Do NOT remove the call sites.

### Task 2.9: New harness cases

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`

- [ ] **Step 1: Add l2_ls_long**

```
l2_ls_long|full|MARKER_MODE=l2_ls_long TEST_COMMAND_REPEAT=1 RUN_WAIT=18
```

```sh
        l2_ls_long)
            SHELL_AUTOSTART_CMD_DEFAULT="echo hello > /tmp/lf; ls -l /tmp/lf"
            EXPECTED_CONTAINS=("-rw" "lf")
            ;;
```

- [ ] **Step 2: Add l2_ls_color**

```
l2_ls_color|full|MARKER_MODE=l2_ls_color TEST_COMMAND_REPEAT=1 RUN_WAIT=18
```

```sh
        l2_ls_color)
            SHELL_AUTOSTART_CMD_DEFAULT="mkdir -p /tmp/cd; ls --color=always /tmp"
            EXPECTED_CONTAINS=("\x1b[1;34mcd")
            ;;
```

- [ ] **Step 3: Add l2_ls_recursive**

```
l2_ls_recursive|full|MARKER_MODE=l2_ls_recursive TEST_COMMAND_REPEAT=1 RUN_WAIT=18
```

```sh
        l2_ls_recursive)
            SHELL_AUTOSTART_CMD_DEFAULT="mkdir -p /tmp/r/sub; touch /tmp/r/a /tmp/r/sub/b; ls -R /tmp/r"
            EXPECTED_CONTAINS=("a" "/tmp/r/sub:" "b")
            ;;
```

- [ ] **Step 4: Run all three**

```bash
for c in l2_ls_long l2_ls_color l2_ls_recursive; do
    bash scripts/harness_run.sh $c 2>&1 | tail -5
done
```

PASS all three.

### Task 2.10: Run full matrix; commit

```bash
bash scripts/harness_matrix.sh 2>&1 | tee /tmp/matrix-after-C.log
```

Green.

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(ls): GNU-close ls — long, color, recursive, sort

Rewrite of userspace/ls (53 → ~450 LOC). Flags -1 -l -a -h -R -S -t -r
plus --color=always|never|auto. Long format prints
'mode nlink uid gid size mtime name'. Color via S_IFMT classification,
disabled when stdout is not a TTY or NO_COLOR is set. Built on
libcluu::cli (Plan B Stage 1) and the extended VfsStat (Plan C Stage 1).

Phase 4 Plan C Stage 2.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Self-review

- **Spec coverage**: §5.4.1 → Stage 1. §5.4.2 → Tasks 2.1–2.8. §5.4.3 (color) → Task 2.5. §5.4.4 (mode/time render) → Tasks 2.2–2.4. §3.4 (stat-batched readdir = perf win) → Task 1.4 readdir wire format.
- **Placeholders**: none. Every helper has full code.
- **Type consistency**: `VfsStat`/`VfsDirEntry` consistent across libcluu, server, and ls. `SortKey`/`LsOpts` consistent within ls.
- **Risk**: protocol bump touches every backend; if a backend mishandles missing fields, only `ls -l` exposes the bug. Smoke `l2_mp_etc` (Task 1.8) catches this for ext2.

---

## Acceptance

Plan C done when:
- `VfsStat` carries size/mode/mtime/nlink/uid/gid/blocks
- `readdir` returns `(name, stat)` pairs in one round trip
- Every backend (ext2, ramfs/memfs, procfs, devfs) supplies a populated stat
- `ls -l -a -h -R -1 -S -t -r --color=auto` works
- `l2_ls_long`, `l2_ls_color`, `l2_ls_recursive` PASS
- `harness_matrix.sh` green
