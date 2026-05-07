# Phase 4 Plan D — Full POSIX Job Control (All Userspace)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement real Ctrl-Z + fg + bg + SIGSTOP/SIGCONT/SIGINT + job table + `kill %N` entirely in userspace. Zero kernel commits required — kernel already exposes `InvokeOp::ThreadSuspend`/`ThreadResume`.

**Architecture:** Three-component split.
1. **procmgr** owns `pgid → [pid]` lifetime, per-pid state machine (Running / Stopped / Continued / Zombie), suspend/resume via existing kernel invoke ops.
2. **TTY service** tracks `fg_pgid_per_session`, decodes Ctrl-C / Ctrl-Z, sends `PROCMGR_PG_SIGNAL` to the foreground pgid.
3. **Shell** carries `JobTable`, replaces pre-jobs primitives (`SpawnBgBuiltin` etc) with `&` syntax + `jobs`/`fg`/`bg`/`wait`/`kill %N` builtins.

**Tech Stack:** Rust, libcluu IPC, kernel `ThreadSuspend`/`Resume` invoke ops (already shipped).

**Spec reference:** `docs/superpowers/specs/2026-05-07-userspace-polish-design.md` §6

**Prereq:** Plan A merged (commands.rs split — jobs builtins live in `commands/builtins/jobs.rs`). Plans B/C optional but recommended (Ctrl-C handler tests benefit from `kill` builtin from B Task 3.6).

---

## File structure

### Created
- `userspace/libcluu/src/posix/jobs.rs` (procmgr IPC wrapper for new labels)
- `userspace/procmgr/src/pg_table.rs` (`PgTable` struct, suspend/resume, signal delivery)

### Modified
- `userspace/libcluu/src/ipc.rs` (PROCMGR_PG_* label constants)
- `userspace/procmgr/src/main.rs` (handle new labels, state-machine update)
- `userspace/procmgr/src/process.rs` (per-pid State enum gains Stopped/Continued)
- `userspace/tty/src/main.rs` (Ctrl-C/Ctrl-Z routing, fg pgid table)
- `userspace/shell/src/commands/builtins/jobs.rs` (rewrite — JobTable, `cmd &`, fg/bg/wait/kill %N)
- `userspace/shell/src/commands/exec.rs` (set pgid on spawned children, set fg pgid for pipelines)
- `userspace/shell/src/main.rs` (install SIGTSTP/SIGINT handlers; signal-aware REPL)
- `Cargo.toml` (no change — files added in-place)
- `scripts/harness_cases.conf` (5 new cases: l2_jobs_basic, l2_jobs_ctrlz, l2_jobs_pipeline, l2_jobs_bg_to_fg, l2_jobs_sigint_fg)

---

## Stage 1 — procmgr pgid table + suspend/resume

### Task 1.1: Define new IPC labels

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs`

- [ ] **Step 1: Add constants**

```rust
pub const PROCMGR_PG_CREATE_LABEL: u32   = 80;
pub const PROCMGR_PG_ATTACH_LABEL: u32   = 81;
pub const PROCMGR_PG_SIGNAL_LABEL: u32   = 82;
pub const PROCMGR_PG_SUSPEND_LABEL: u32  = 83;
pub const PROCMGR_PG_RESUME_LABEL: u32   = 84;
pub const PROCMGR_TTY_SET_FG_LABEL: u32  = 85;
pub const PROCMGR_JOB_NOTIFY_LABEL: u32  = 86; // procmgr → shell async
```

(Pick numbers from an unused range. Verify no collision with existing labels.)

```bash
grep -n 'PROCMGR_.*_LABEL: u32 =' userspace/libcluu/src/ipc.rs | sort -k4 -n
```

- [ ] **Step 2: Add a `JobNotify` struct used in PROCMGR_JOB_NOTIFY_LABEL payload**

```rust
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct JobNotify {
    pub pgid: usize,
    pub pid:  usize,
    pub state: u32, // 0=Running, 1=Stopped, 2=Continued, 3=Exited
    pub exit_code: i32,
}
```

### Task 1.2: PgTable in procmgr

**Files:**
- Create: `userspace/procmgr/src/pg_table.rs`
- Modify: `userspace/procmgr/src/main.rs`

- [ ] **Step 1: Failing test for PgTable basics**

If procmgr lacks host-test infra, add a `host-test` feature mirror like libcluu in Plan B Task 1.1 step 1.

```rust
// userspace/procmgr/tests/pg_table_test.rs
#![cfg(feature = "host-test")]

use cluu_procmgr::pg_table::PgTable;

#[test]
fn create_yields_unique_pgid() {
    let mut t = PgTable::new();
    let a = t.create();
    let b = t.create();
    assert_ne!(a, b);
}

#[test]
fn attach_then_membership() {
    let mut t = PgTable::new();
    let pgid = t.create();
    t.attach(pgid, 42);
    t.attach(pgid, 43);
    let m = t.members(pgid);
    assert_eq!(m.len(), 2);
    assert!(m.contains(&42));
    assert!(m.contains(&43));
}

#[test]
fn detach_removes_pid() {
    let mut t = PgTable::new();
    let pgid = t.create();
    t.attach(pgid, 1);
    t.attach(pgid, 2);
    t.detach(pgid, 1);
    assert_eq!(t.members(pgid), vec![2]);
}

#[test]
fn empty_pgid_garbage_collected() {
    let mut t = PgTable::new();
    let pgid = t.create();
    t.attach(pgid, 1);
    t.detach(pgid, 1);
    assert!(t.members(pgid).is_empty());
    assert!(!t.exists(pgid));
}
```

- [ ] **Step 2: Run, verify FAIL**

```bash
cargo test -p procmgr --features host-test --test pg_table_test
```

Compile error — module doesn't exist.

- [ ] **Step 3: Implement PgTable**

```rust
// userspace/procmgr/src/pg_table.rs

extern crate alloc;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

#[derive(Default)]
pub struct PgTable {
    next_pgid: usize,
    members:  BTreeMap<usize, Vec<usize>>,
}

impl PgTable {
    pub const fn new() -> Self {
        Self { next_pgid: 1, members: BTreeMap::new() }
    }
    pub fn create(&mut self) -> usize {
        let id = self.next_pgid; self.next_pgid += 1;
        self.members.insert(id, Vec::new());
        id
    }
    pub fn attach(&mut self, pgid: usize, pid: usize) {
        if let Some(v) = self.members.get_mut(&pgid) {
            if !v.contains(&pid) { v.push(pid); }
        }
    }
    pub fn detach(&mut self, pgid: usize, pid: usize) {
        if let Some(v) = self.members.get_mut(&pgid) {
            v.retain(|&p| p != pid);
            if v.is_empty() { self.members.remove(&pgid); }
        }
    }
    pub fn members(&self, pgid: usize) -> Vec<usize> {
        self.members.get(&pgid).cloned().unwrap_or_default()
    }
    pub fn exists(&self, pgid: usize) -> bool {
        self.members.contains_key(&pgid)
    }
    pub fn pgid_of(&self, pid: usize) -> Option<usize> {
        for (pgid, members) in &self.members {
            if members.contains(&pid) { return Some(*pgid); }
        }
        None
    }
}
```

- [ ] **Step 4: Tests PASS**

```bash
cargo test -p procmgr --features host-test --test pg_table_test
```

All 4 PASS.

### Task 1.3: Per-pid state machine extension

**Files:**
- Modify: `userspace/procmgr/src/process.rs`

- [ ] **Step 1: Find the existing state enum**

```bash
grep -n 'enum.*State\|enum ProcessState\|Running\|Zombie' userspace/procmgr/src/process.rs | head
```

- [ ] **Step 2: Extend it**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Running,
    Stopped,
    Continued,    // transient, transitions to Running after notification
    Zombie,
}
```

Update every `match` over the old enum to handle the new variants explicitly.

### Task 1.4: PROCMGR_PG_CREATE / ATTACH handlers

**Files:**
- Modify: `userspace/procmgr/src/main.rs`

- [ ] **Step 1: Find the central message dispatch loop**

```bash
grep -n 'PROCMGR_.*_LABEL\|fn handle_message\|match label' userspace/procmgr/src/main.rs | head
```

- [ ] **Step 2: Add handlers**

```rust
PROCMGR_PG_CREATE_LABEL => {
    let pgid = pg_table.create();
    reply.words[0] = 0;        // ok
    reply.words[1] = pgid;
}
PROCMGR_PG_ATTACH_LABEL => {
    let pgid = msg.words[0];
    let pid  = msg.words[1];
    pg_table.attach(pgid, pid);
    reply.words[0] = 0;
}
```

Add a `pg_table: PgTable` to the procmgr's main state struct alongside the existing process table.

### Task 1.5: PROCMGR_PG_SUSPEND / RESUME handlers

- [ ] **Step 1: Implement**

```rust
PROCMGR_PG_SUSPEND_LABEL => {
    let pgid = msg.words[0];
    for pid in pg_table.members(pgid) {
        if let Some(p) = process_table.get_mut(&pid) {
            for tid in &p.threads {
                let _ = libcluu::syscall::invoke(*tid, InvokeOp::ThreadSuspend, &[]);
            }
            p.state = ProcessState::Stopped;
            // Notify shell asynchronously.
            send_job_notify(&p.parent_exit_endpoint, JobNotify {
                pgid, pid, state: 1 /* Stopped */, exit_code: 0,
            });
        }
    }
    reply.words[0] = 0;
}
PROCMGR_PG_RESUME_LABEL => {
    let pgid = msg.words[0];
    for pid in pg_table.members(pgid) {
        if let Some(p) = process_table.get_mut(&pid) {
            for tid in &p.threads {
                let _ = libcluu::syscall::invoke(*tid, InvokeOp::ThreadResume, &[]);
            }
            p.state = ProcessState::Running;
            send_job_notify(&p.parent_exit_endpoint, JobNotify {
                pgid, pid, state: 2 /* Continued */, exit_code: 0,
            });
        }
    }
    reply.words[0] = 0;
}
```

`send_job_notify` is a helper that fires a one-way IPC with `PROCMGR_JOB_NOTIFY_LABEL` to the parent's existing exit endpoint (already used for `PROC_EXIT_LABEL`).

```rust
fn send_job_notify(endpoint: &Option<usize>, n: JobNotify) {
    if let Some(ep) = endpoint {
        let mut msg = libcluu::ipc::Message::new(PROCMGR_JOB_NOTIFY_LABEL, [0;6], 4);
        msg.words[0] = n.pgid;
        msg.words[1] = n.pid;
        msg.words[2] = n.state as usize;
        msg.words[3] = n.exit_code as usize;
        let _ = libcluu::ipc::send_oneway(*ep, &mut msg);
    }
}
```

### Task 1.6: PROCMGR_PG_SIGNAL handler

- [ ] **Step 1: Implement**

```rust
PROCMGR_PG_SIGNAL_LABEL => {
    let pgid    = msg.words[0];
    let signum  = msg.words[1] as i32;
    for pid in pg_table.members(pgid) {
        if let Some(p) = process_table.get_mut(&pid) {
            // Default action for SIGSTOP / SIGTSTP: suspend (uncatchable for SIGSTOP).
            // For other signals: deliver via libcluu signal infra (existing).
            match signum {
                SIGSTOP | SIGTSTP => {
                    if signum == SIGTSTP && p.has_signal_handler(SIGTSTP) {
                        deliver_signal_to(p, SIGTSTP);
                    } else {
                        for tid in &p.threads {
                            let _ = libcluu::syscall::invoke(*tid, InvokeOp::ThreadSuspend, &[]);
                        }
                        p.state = ProcessState::Stopped;
                        send_job_notify(&p.parent_exit_endpoint, JobNotify { pgid, pid, state: 1, exit_code: 0 });
                    }
                }
                SIGCONT => {
                    for tid in &p.threads {
                        let _ = libcluu::syscall::invoke(*tid, InvokeOp::ThreadResume, &[]);
                    }
                    p.state = ProcessState::Running;
                    send_job_notify(&p.parent_exit_endpoint, JobNotify { pgid, pid, state: 2, exit_code: 0 });
                }
                _ => {
                    deliver_signal_to(p, signum);
                }
            }
        }
    }
    reply.words[0] = 0;
}
```

`deliver_signal_to` reuses the existing signal-delivery path (libcluu signal infra). If procmgr currently doesn't deliver signals (only the kernel does, indirectly), wire it via the existing `signal::raise_in` primitive.

### Task 1.7: TTY_SET_FG handler

```rust
PROCMGR_TTY_SET_FG_LABEL => {
    let session_id = msg.words[0];
    let pgid       = msg.words[1];
    fg_pgid_by_session.insert(session_id, pgid);
    reply.words[0] = 0;
}
```

`fg_pgid_by_session: BTreeMap<usize, usize>` is a new field on procmgr's state. (Or live in TTY — see Task 2 below.)

**Decision**: per spec §6.1, **TTY owns** `fg_pgid_per_session`, not procmgr. Move this to Task 2.1. Procmgr exposes a query so other services can ask "is pgid X foreground?" if needed.

Actually re-read spec §6.3: `PROCMGR_TTY_SET_FG` is shell→procmgr, where procmgr forwards (or echoes) to TTY. Cleaner: shell sets fg directly on TTY via TTY's own labels. Replace this Task with §2.1.

Skip Task 1.7. Leave `PROCMGR_TTY_SET_FG_LABEL` reserved.

### Task 1.8: Commit Stage 1

```bash
cargo test -p procmgr --features host-test
cargo xtask build 2>&1 | tail -10
```

PASS.

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(procmgr): pgid table + suspend/resume/signal IPC

procmgr gains a PgTable mapping pgid → [pid], a Stopped/Continued state
machine per process, and PROCMGR_PG_{CREATE,ATTACH,SIGNAL,SUSPEND,RESUME}
IPC labels. Suspend/resume drive the existing kernel ThreadSuspend/Resume
invoke ops — zero kernel changes. PROCMGR_JOB_NOTIFY_LABEL fires async to
parent on state transitions.

Phase 4 Plan D Stage 1.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 2 — TTY foreground pgid + signal routing

### Task 2.1: TTY tracks fg pgid per session

**Files:**
- Modify: `userspace/tty/src/main.rs`

- [ ] **Step 1: Add fg_pgid table**

```rust
struct TtyState {
    // ...existing fields...
    fg_pgid_per_session: BTreeMap<usize, usize>,
}
```

- [ ] **Step 2: Add new IPC label**

```rust
pub const TTY_SET_FG_LABEL: u32 = 30; // verify no collision
pub const TTY_GET_FG_LABEL: u32 = 31;
```

In `userspace/libcluu/src/ipc.rs`. Verify no collision with existing TTY labels.

- [ ] **Step 3: Handler**

```rust
TTY_SET_FG_LABEL => {
    let session = msg.words[0];
    let pgid    = msg.words[1];
    state.fg_pgid_per_session.insert(session, pgid);
    reply.words[0] = 0;
}
```

### Task 2.2: TTY reads detect Ctrl-C / Ctrl-Z

**Files:**
- Modify: `userspace/tty/src/main.rs`

- [ ] **Step 1: Find the input-byte handler**

```bash
grep -n 'fn process_input\|fn handle_input\|0x03\|0x1A\|ctrl_c\|ctrl_z' userspace/tty/src/main.rs | head
```

- [ ] **Step 2: Intercept Ctrl-C (0x03) and Ctrl-Z (0x1A) in raw mode**

When the foreground TTY is in canonical mode AND ISIG is set (default):

```rust
if byte == 0x03 {  // Ctrl-C
    if let Some(&pgid) = state.fg_pgid_per_session.get(&session) {
        send_pg_signal(pgid, SIGINT);
    }
    return; // do not pass byte to readers
}
if byte == 0x1A {  // Ctrl-Z
    if let Some(&pgid) = state.fg_pgid_per_session.get(&session) {
        send_pg_signal(pgid, SIGTSTP);
    }
    return;
}
```

`send_pg_signal` issues a one-way IPC to procmgr:

```rust
fn send_pg_signal(pgid: usize, signum: i32) {
    let mut msg = Message::new(PROCMGR_PG_SIGNAL_LABEL, [0;6], 2);
    msg.words[0] = pgid;
    msg.words[1] = signum as usize;
    let _ = ipc::send_oneway(procmgr_endpoint(), &mut msg);
}
```

### Task 2.3: TTY blocks bg processes from stdin

- [ ] **Step 1: On TTY read request, check caller pgid**

When a process calls TTY_READ, look up its pgid (procmgr query). Compare to fg_pgid:

```rust
let caller_pgid = procmgr_query_pgid(caller_tid);
let fg = state.fg_pgid_per_session.get(&session).copied();
if Some(caller_pgid) != fg {
    // Not foreground; reply with TTYIN error so libcluu can deliver SIGTTIN.
    reply.words[0] = -EBKGD;
    return;
}
```

`procmgr_query_pgid` is a new lightweight IPC label `PROCMGR_PID_PGID_QUERY` (read-only) — add it now:

```rust
PROCMGR_PID_PGID_QUERY_LABEL => {
    let tid = msg.words[0];
    let pid = process_table_pid_for_tid(tid);
    let pgid = pid.and_then(|p| pg_table.pgid_of(p)).unwrap_or(0);
    reply.words[0] = 0;
    reply.words[1] = pgid;
}
```

In libcluu, when posix `read(0)` returns EBKGD, raise SIGTTIN to self.

### Task 2.4: Commit Stage 2

```bash
cargo xtask build 2>&1 | tail -10
```

PASS. Run `l2_cd` and `l2_argv` to confirm baseline still works:

```bash
bash scripts/harness_run.sh l2_cd 2>&1 | tail -3
bash scripts/harness_run.sh l2_argv 2>&1 | tail -3
```

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(tty): foreground pgid routing + Ctrl-C/Ctrl-Z signal delivery

TTY tracks fg_pgid_per_session, decodes Ctrl-C and Ctrl-Z, sends
PROCMGR_PG_SIGNAL with SIGINT/SIGTSTP. Background processes attempting
to read stdin get -EBKGD and libcluu raises SIGTTIN. New TTY_SET_FG
and PROCMGR_PID_PGID_QUERY labels.

Phase 4 Plan D Stage 2.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 3 — Shell JobTable + builtins + spawn changes

### Task 3.1: Define JobTable

**Files:**
- Modify: `userspace/shell/src/commands/builtins/jobs.rs` (full rewrite)

- [ ] **Step 1: Replace the file with the new structure**

```rust
//! Job control builtins: jobs, fg, bg, wait, kill %N.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use libcluu::ipc::{
    Message, send_oneway, recv,
    PROCMGR_PG_CREATE_LABEL, PROCMGR_PG_ATTACH_LABEL,
    PROCMGR_PG_SIGNAL_LABEL, PROCMGR_PG_RESUME_LABEL,
    PROCMGR_JOB_NOTIFY_LABEL, TTY_SET_FG_LABEL,
};
use libcluu::signal::{SIGCONT, SIGTERM};

use super::registry::{Builtin, BuiltinRegistry, BuiltinResult};
use crate::ShellContext;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState { Running, Stopped, Done }

#[derive(Debug, Clone)]
pub struct Job {
    pub id: usize,             // 1-based shell job id
    pub pgid: usize,
    pub pids: Vec<usize>,
    pub state: JobState,
    pub cmd_line: String,
    pub bg: bool,
    pub last_exit: Option<i32>,
}

#[derive(Default)]
pub struct JobTable {
    next_id: usize,
    by_id: BTreeMap<usize, Job>,
}

impl JobTable {
    pub const fn new() -> Self { Self { next_id: 0, by_id: BTreeMap::new() } }
    pub fn add(&mut self, pgid: usize, pids: Vec<usize>, cmd_line: String, bg: bool) -> usize {
        self.next_id += 1;
        let id = self.next_id;
        self.by_id.insert(id, Job { id, pgid, pids, state: JobState::Running, cmd_line, bg, last_exit: None });
        id
    }
    pub fn get(&self, id: usize) -> Option<&Job> { self.by_id.get(&id) }
    pub fn get_mut(&mut self, id: usize) -> Option<&mut Job> { self.by_id.get_mut(&id) }
    pub fn remove(&mut self, id: usize) -> Option<Job> { self.by_id.remove(&id) }
    pub fn iter(&self) -> impl Iterator<Item = &Job> { self.by_id.values() }
    pub fn most_recent(&self) -> Option<&Job> { self.by_id.values().last() }
    pub fn by_pgid(&self, pgid: usize) -> Option<&Job> {
        self.by_id.values().find(|j| j.pgid == pgid)
    }
    pub fn by_pgid_mut(&mut self, pgid: usize) -> Option<&mut Job> {
        self.by_id.values_mut().find(|j| j.pgid == pgid)
    }
}

pub struct JobsBuiltin;
pub struct FgBuiltin;
pub struct BgBuiltin;
pub struct WaitBuiltin;
pub struct KillBuiltin;

impl Builtin for JobsBuiltin {
    fn name(&self) -> &'static str { "jobs" }
    fn run(&self, _args: &[String], context: &mut ShellContext) -> BuiltinResult {
        for j in context.jobs.iter() {
            let state = match j.state {
                JobState::Running => "Running",
                JobState::Stopped => "Stopped",
                JobState::Done    => "Done",
            };
            let line = format!("[{}]{} {}  {}\n",
                j.id, if Some(j.id) == context.jobs.most_recent().map(|x| x.id) { "+" } else { " " },
                state, j.cmd_line);
            let _ = libcluu::posix::_write(1, line.as_ptr() as *const _, line.len());
        }
        BuiltinResult::Ok(0)
    }
}

impl Builtin for FgBuiltin {
    fn name(&self) -> &'static str { "fg" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        let target_id = parse_jobspec(args, context).map_err(|e| e)?;
        let pgid = match context.jobs.get(target_id).map(|j| j.pgid) {
            Some(p) => p,
            None => { return BuiltinResult::Err(format!("fg: %{}: no such job", target_id)); }
        };
        // Resume.
        let mut resume = Message::new(PROCMGR_PG_RESUME_LABEL, [0;6], 1);
        resume.words[0] = pgid;
        let _ = send_oneway(context.procmgr_endpoint, &mut resume);
        // Move TTY foreground.
        set_tty_fg(context.tty_endpoint, context.session_id, pgid);
        // Mark Running.
        if let Some(j) = context.jobs.get_mut(target_id) { j.state = JobState::Running; j.bg = false; }
        // Wait for next state change (Stopped or Done).
        wait_for_job(target_id, context);
        // Restore shell as foreground.
        set_tty_fg(context.tty_endpoint, context.session_id, context.shell_pgid);
        BuiltinResult::Ok(context.jobs.get(target_id).and_then(|j| j.last_exit).unwrap_or(0))
    }
}

impl Builtin for BgBuiltin {
    fn name(&self) -> &'static str { "bg" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        let target_id = match parse_jobspec(args, context) { Ok(i) => i, Err(e) => return BuiltinResult::Err(e) };
        let pgid = match context.jobs.get(target_id).map(|j| j.pgid) {
            Some(p) => p,
            None => return BuiltinResult::Err(format!("bg: %{}: no such job", target_id)),
        };
        let mut resume = Message::new(PROCMGR_PG_RESUME_LABEL, [0;6], 1);
        resume.words[0] = pgid;
        let _ = send_oneway(context.procmgr_endpoint, &mut resume);
        if let Some(j) = context.jobs.get_mut(target_id) { j.state = JobState::Running; j.bg = true; }
        BuiltinResult::Ok(0)
    }
}

impl Builtin for WaitBuiltin {
    fn name(&self) -> &'static str { "wait" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        if args.is_empty() {
            // Wait for all background jobs.
            let ids: Vec<usize> = context.jobs.iter().filter(|j| j.state != JobState::Done).map(|j| j.id).collect();
            for id in ids { wait_for_job(id, context); }
            BuiltinResult::Ok(0)
        } else {
            let id = match parse_jobspec(args, context) { Ok(i) => i, Err(e) => return BuiltinResult::Err(e) };
            wait_for_job(id, context);
            BuiltinResult::Ok(context.jobs.get(id).and_then(|j| j.last_exit).unwrap_or(0))
        }
    }
}

impl Builtin for KillBuiltin {
    fn name(&self) -> &'static str { "kill" }
    fn run(&self, args: &[String], context: &mut ShellContext) -> BuiltinResult {
        if args.is_empty() { return BuiltinResult::Err("kill: usage: kill [-s SIG] PID|%JOB".into()); }
        // Minimal: kill %N → SIGTERM to pgid; kill PID → libcluu kill.
        let target = &args[0];
        if let Some(spec) = target.strip_prefix('%') {
            let id: usize = spec.parse().map_err(|_| String::from("kill: bad job spec"))?;
            let pgid = context.jobs.get(id).map(|j| j.pgid).ok_or_else(|| format!("kill: %{}: no such job", id))?;
            let mut sig = Message::new(PROCMGR_PG_SIGNAL_LABEL, [0;6], 2);
            sig.words[0] = pgid;
            sig.words[1] = SIGTERM as usize;
            let _ = send_oneway(context.procmgr_endpoint, &mut sig);
            BuiltinResult::Ok(0)
        } else {
            // Numeric PID — fall back to libcluu's kill primitive.
            let pid: usize = target.parse().map_err(|_| String::from("kill: bad pid"))?;
            let rc = libcluu::posix::kill(pid as i32, SIGTERM);
            BuiltinResult::Ok(rc)
        }
    }
}

fn parse_jobspec(args: &[String], context: &mut ShellContext) -> Result<usize, String> {
    if args.is_empty() {
        return context.jobs.most_recent().map(|j| j.id).ok_or_else(|| String::from("no current job"));
    }
    let s = &args[0];
    if let Some(spec) = s.strip_prefix('%') {
        return spec.parse().map_err(|_| String::from("bad job spec"));
    }
    s.parse().map_err(|_| String::from("bad job spec"))
}

fn set_tty_fg(tty_endpoint: usize, session: usize, pgid: usize) {
    let mut m = Message::new(TTY_SET_FG_LABEL, [0;6], 2);
    m.words[0] = session;
    m.words[1] = pgid;
    let _ = send_oneway(tty_endpoint, &mut m);
}

fn wait_for_job(id: usize, context: &mut ShellContext) {
    loop {
        let mut buf = [0u8; 64];
        let _ = libcluu::syscall::ipc_recv(context.exit_endpoint, &mut buf);
        let label = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
        if label == PROCMGR_JOB_NOTIFY_LABEL {
            // Parse JobNotify; update JobTable.
            let words: &[usize] = unsafe { core::slice::from_raw_parts(buf[8..].as_ptr() as *const usize, 4) };
            let pgid = words[0];
            let _pid = words[1];
            let state = words[2] as u32;
            let exit_code = words[3] as i32;
            if let Some(j) = context.jobs.by_pgid_mut(pgid) {
                match state {
                    1 => j.state = JobState::Stopped,
                    2 => j.state = JobState::Running,
                    3 => { j.state = JobState::Done; j.last_exit = Some(exit_code); }
                    _ => {}
                }
                if j.id == id && (j.state == JobState::Stopped || j.state == JobState::Done) {
                    return;
                }
            }
        } else {
            // Other notifications: forward or queue. Defer.
        }
    }
}

pub fn register(registry: &mut BuiltinRegistry) {
    registry.register(Box::new(JobsBuiltin));
    registry.register(Box::new(FgBuiltin));
    registry.register(Box::new(BgBuiltin));
    registry.register(Box::new(WaitBuiltin));
    registry.register(Box::new(KillBuiltin));
}
```

- [ ] **Step 2: Remove pre-jobs primitives**

Delete from registry: `SpawnBuiltin`, `SpawnBgBuiltin`, `StopBuiltin`, `ForegroundBuiltin`, `BackgroundBuiltin`, old `JobsBuiltin` (replaced above).

- [ ] **Step 3: Add fields to `ShellContext`**

```rust
pub struct ShellContext {
    // existing fields...
    pub jobs: jobs::JobTable,
    pub shell_pgid: usize,
    pub session_id: usize,
    pub procmgr_endpoint: usize,
    pub tty_endpoint: usize,
    pub exit_endpoint: usize,
}
```

Initialize at startup (Task 3.4).

### Task 3.2: Spawn path sets pgid

**Files:**
- Modify: `userspace/shell/src/commands/exec.rs`
- Modify: `userspace/shell/src/pipeline.rs`

- [ ] **Step 1: For each spawned child, ATTACH its pid to the new pgid**

In single-cmd `exec.rs`:

```rust
// Create pgid for this command.
let mut pg_create = Message::new(PROCMGR_PG_CREATE_LABEL, [0;6], 0);
let mut reply = Message::new(0, [0;6], 0);
let _ = ipc::call(context.procmgr_endpoint, &mut pg_create, libcluu::IpcFlags::empty());
let pgid = reply.words[1];

// Attach the child's pid (returned by spawn) to pgid.
let mut attach = Message::new(PROCMGR_PG_ATTACH_LABEL, [0;6], 2);
attach.words[0] = pgid;
attach.words[1] = child_pid;
let _ = ipc::send_oneway(context.procmgr_endpoint, &mut attach);

// If foreground: set TTY fg pgid before child runs.
if !is_background {
    set_tty_fg(context.tty_endpoint, context.session_id, pgid);
}

// Track in JobTable.
let job_id = context.jobs.add(pgid, vec![child_pid], cmd_line.clone(), is_background);
```

In `pipeline.rs`:
- Single pgid created at the start of the pipeline.
- Every stage's pid is attached to that pgid.
- TTY fg set to that pgid for foreground pipeline.

### Task 3.3: `&` syntax marks job as background

**Files:**
- Modify: shell parser (likely `crates/cluu_lang/src/ast.rs` — verify)

- [ ] **Step 1: Confirm `&` is parsed**

```bash
grep -rn 'background\|bg_token\|"&"\|Cmd::Bg' crates/cluu_lang/src/ userspace/shell/src/ 2>/dev/null | head
```

If the lexer/parser already produces an AST node for trailing `&`, exec.rs can read `cmd.is_background`. If not, extend the AST: `Pipeline { stages: Vec<Cmd>, bg: bool }`.

- [ ] **Step 2: When `bg=true`, exec.rs does not block on wait; prints `[id] pgid` and returns to prompt.**

### Task 3.4: REPL handles JOB_NOTIFY messages between commands

**Files:**
- Modify: `userspace/shell/src/main.rs`

- [ ] **Step 1: Initialize ShellContext.jobs at startup**

```rust
let exit_endpoint = libcluu::ipc::endpoint_create(...).unwrap();
let shell_pgid_msg = Message::new(PROCMGR_PG_CREATE_LABEL, ...);
// Reserve a pgid for the shell process itself.
let shell_pgid = pgid_create(procmgr_endpoint);
let _ = pg_attach(procmgr_endpoint, shell_pgid, my_pid);

let mut context = ShellContext {
    // ...
    jobs: jobs::JobTable::new(),
    shell_pgid,
    session_id: get_session_id(),
    procmgr_endpoint,
    tty_endpoint,
    exit_endpoint,
};
set_tty_fg(tty_endpoint, context.session_id, shell_pgid);
```

- [ ] **Step 2: After each prompt loop iteration, drain pending JOB_NOTIFY messages**

```rust
fn drain_job_notifications(context: &mut ShellContext) {
    loop {
        let mut buf = [0u8; 64];
        match libcluu::syscall::ipc_recv_nonblocking(context.exit_endpoint, &mut buf) {
            Ok(_n) => {
                // Parse and update JobTable; print "[id]+ Stopped" or "[id] Done" lines.
                handle_notify_buf(&buf, context);
            }
            Err(_) => break,
        }
    }
}
```

If `ipc_recv_nonblocking` doesn't exist, add it (poll/peek with zero timeout).

### Task 3.5: SIGTSTP/SIGINT handlers in shell

**Files:**
- Modify: `userspace/shell/src/main.rs`

- [ ] **Step 1: Install handlers BEFORE first child is spawned**

```rust
libcluu::signal::sigaction(SIGINT, |_| {
    // Shell ignores SIGINT itself; child gets it via TTY routing.
});
libcluu::signal::sigaction(SIGTSTP, |_| {
    // Same — shell ignores; foreground child gets it.
});
```

- [ ] **Step 2: TTY no longer raises Ctrl-C / Ctrl-Z to shell when shell is foreground but has these handlers.**

This is automatic — the TTY signals via PROCMGR_PG_SIGNAL to the foreground pgid, which is shell only when no child is running. With the handlers above, shell ignores them and re-prints prompt.

### Task 3.6: Harness cases

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`

- [ ] **Step 1: l2_jobs_basic**

```
l2_jobs_basic|full|MARKER_MODE=l2_jobs_basic TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

```sh
        l2_jobs_basic)
            SHELL_AUTOSTART_CMD_DEFAULT="sleep 30 & jobs; kill %1; wait"
            EXPECTED_CONTAINS=("[1]" "Running" "Done")
            ;;
```

- [ ] **Step 2: l2_jobs_ctrlz**

Needs the harness to feed Ctrl-Z bytes — check existing `l2_edit_*` cases for input-injection pattern.

```
l2_jobs_ctrlz|full|MARKER_MODE=l2_jobs_ctrlz TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

```sh
        l2_jobs_ctrlz)
            SHELL_AUTOSTART_CMD_DEFAULT="cat"
            INPUT_INJECT="line\n\x1A"   # then later "fg\n", "Ctrl-D"
            EXPECTED_CONTAINS=("Stopped" "fg" "line")
            ;;
```

If input-injection pattern doesn't yet exist, defer to a follow-up. Manually validate via interactive QEMU run; mark this case `pending_harness_input`.

- [ ] **Step 3: l2_jobs_pipeline, l2_jobs_bg_to_fg, l2_jobs_sigint_fg**

Per spec §6.7. Same skeleton; use cases to validate that pipelines clean up, fg→Ctrl-C exits 130, etc.

### Task 3.7: Run, commit Stage 3

```bash
bash scripts/harness_run.sh l2_jobs_basic 2>&1 | tail -10
```

PASS.

```bash
bash scripts/harness_matrix.sh 2>&1 | tee /tmp/matrix-after-D.log
```

Green.

```bash
git add -A
git commit -m "$(cat <<'EOF'
feat(shell): full POSIX job control via JobTable + procmgr/TTY IPC

Shell gains JobTable, jobs/fg/bg/wait/kill builtins, & syntax for
background. Pre-jobs primitives (Spawn/SpawnBg/Stop/Foreground/
Background) removed. Pipeline stages share a single pgid; TTY routes
Ctrl-C and Ctrl-Z to the foreground pgid via PROCMGR_PG_SIGNAL. Real
SIGSTOP via kernel ThreadSuspend; SIGCONT via ThreadResume. All
userspace — no kernel changes.

Phase 4 Plan D Stage 3.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Self-review

- **Spec coverage**: §6.1 architecture → Stage 1+2+3. §6.2 procmgr ownership → Stage 1. §6.3 IPC labels → Task 1.1. §6.4 Ctrl-Z flow → Tasks 1.6 + 2.2 + 3.4. §6.5 state machine → Task 1.3. §6.6 risks documented. §6.7 test cases → Task 3.6.
- **Placeholders**: `[PASTE]` markers absent. Where existing primitives (signal::sigaction, send_oneway) may need creation, the plan flags it inline.
- **Type consistency**: `ProcessState`, `JobState`, `JobNotify` consistent across procmgr/shell. Label constants single-sourced in `libcluu/src/ipc.rs`.
- **Risk**: Task 2.3 ("background process steals stdin → SIGTTIN") relies on libcluu mapping `-EBKGD` to a self-raise. If libcluu's `read` doesn't currently inspect specific error codes, add that path.

---

## Acceptance

Plan D done when:
- `cmd &` runs in background, returns to prompt
- `jobs` lists all jobs with state
- `fg %N` resumes Stopped job, takes TTY foreground
- `bg %N` resumes Stopped job in background
- Ctrl-C in foreground sends SIGINT (default → exit 130)
- Ctrl-Z in foreground sends SIGTSTP, job appears Stopped, shell returns to prompt
- `kill %N` terminates job
- 5 harness cases PASS
- `harness_matrix.sh` green
- Zero kernel commits in this Plan
