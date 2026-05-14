# Plan 4: PROCMGR_SPAWN broker for compositor-initiated apps

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let the user-mode compositor open a new app from its menu by sending a single `PROCMGR_SPAWN_SESSION_LABEL` request to procmgr; procmgr verifies the caller is the live session compositor and spawns the named image as a sibling in the same session container, inheriting the user envelope. No additional capability handed to the compositor.

**Architecture:** Pure broker pattern. Compositor holds zero spawn capability of its own — it just sends an IPC request. Procmgr looks up the caller's authenticated `sender_tid`, finds the matching session container, validates the request, spawns the child as a sibling. Existing `PROCMGR_SPAWN_LABEL` is the libcluu posix_spawn channel; this plan adds a separate label so the broker path can't be triggered by arbitrary processes — only by a registered session-compositor.

**Tech Stack:** Rust, existing procmgr IPC infrastructure, existing envelope + view machinery from Plan 2.

**Depends on:** Plans 1 + 2 + 3.

---

## Pre-flight context

| File | Action |
|---|---|
| `userspace/libcluu/src/ipc.rs` | Define `PROCMGR_SPAWN_SESSION_LABEL` constant |
| `userspace/procmgr/src/main.rs` | Handle the new label in the dispatch (around line 1928); add `handle_spawn_session` |
| `userspace/compositor/src/main.rs` | When user activates a menu entry, send `PROCMGR_SPAWN_SESSION_LABEL` |
| `containers/cluuterm/Cluufile` | First menu app — already shipped, just verify it's visible to the user envelope's view |
| `scripts/harness_*` | Marker `l2_compositor_menu_cluuterm` |

Wire format for `PROCMGR_SPAWN_SESSION_LABEL`:

```
words[0] = payload_len  (path bytes; null-terminated)
words[1] = session_cid  (must match caller's session)
words[2] = flags        (reserved, 0)
nwords   = 3
payload  = "image-path\0"   (e.g., "/var/images/cluuterm/bin/cluuterm")
```

Reply: `words[0] = errno`, `words[1] = pid` on success.

---

## Task 1: Define the IPC label + payload spec

**Files:**
- Modify: `userspace/libcluu/src/ipc.rs`

- [ ] **Step 1: Add the constant**

Near the other `PROCMGR_*_LABEL` constants:

```rust
/// Compositor-only broker spawn: ask procmgr to spawn an app as a sibling
/// in the caller's session container. Procmgr verifies the caller is the
/// session's root compositor. Payload: NUL-terminated image path.
/// Reply words[0]=errno, words[1]=pid.
pub const PROCMGR_SPAWN_SESSION_LABEL: u32 = /* next free in the range */;
```

Pick the next unused value (`grep -n PROCMGR.*_LABEL userspace/libcluu/src/ipc.rs` to see existing).

- [ ] **Step 2: Build**

```bash
cargo xtask build 2>&1 | tail -3
```

Expected: clean build (no users yet).

- [ ] **Step 3: Commit**

```bash
git add userspace/libcluu/src/ipc.rs
git commit -m "ipc: PROCMGR_SPAWN_SESSION_LABEL constant"
```

---

## Task 2: Procmgr handles the broker request

**Files:**
- Modify: `userspace/procmgr/src/main.rs` — extend the dispatch around line 1928 (where `PROCMGR_SPAWN_SERVICE_LABEL` is handled).

- [ ] **Step 1: Add the dispatch arm**

Find the `if msg.tag.label == PROCMGR_SPAWN_SERVICE_LABEL` block. Add right after it:

```rust
        if msg.tag.label == libcluu::ipc::PROCMGR_SPAWN_SESSION_LABEL {
            return self.handle_spawn_session(msg, payload, sender_tid);
        }
```

- [ ] **Step 2: Implement `handle_spawn_session`**

Add this method on `impl ProcessManager`:

```rust
    /// Broker spawn: compositor asks procmgr to launch an app as a sibling
    /// in its session container. Verifies the caller is the registered
    /// session compositor (sender_tid matches the session's root pid's
    /// main thread).
    fn handle_spawn_session(
        &mut self,
        msg: &Message,
        payload: &[u8],
        sender_tid: usize,
    ) -> Result<()> {
        let reply_token = extract_reply_id(msg);
        let mut reply_msg = Message::new(libcluu::ipc::PROCMGR_SPAWN_SESSION_LABEL, [0; 6], 2);

        let payload_len = msg.words[0];
        let claimed_session_cid = msg.words[1] as u64;

        // 1. Validate sender is the session compositor.
        let session_entry = match self.session_table
            .iter()
            .find(|(_, e)| e.container_id == claimed_session_cid)
            .map(|(_, e)| e.clone())
        {
            Some(e) => e,
            None => {
                let _ = debug_print(&format!(
                    "procmgr: SPAWN_SESSION unknown session_cid={}", claimed_session_cid
                ));
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // session_entry.pid is the compositor's pid. Look up its main-thread tid.
        let compositor_tid = match self.pid_to_tid.get(&session_entry.pid).copied() {
            Some(t) => t,
            None => {
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        if sender_tid != compositor_tid {
            let _ = debug_print(&format!(
                "procmgr: SPAWN_SESSION reject: sender_tid={} != compositor_tid={}",
                sender_tid, compositor_tid
            ));
            reply_msg.words[0] = Error::PermissionDenied.to_errno() as usize;
            if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
            return Ok(());
        }

        // 2. Parse image path from payload.
        let path_bytes = &payload[..payload_len.min(payload.len())];
        let path_str = match core::str::from_utf8(path_bytes.split(|&b| b == 0).next().unwrap_or(&[])) {
            Ok(s) if !s.is_empty() => s,
            _ => {
                reply_msg.words[0] = Error::InvalidArgument.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };

        // 3. Resolve envelope from session_entry.profile + username, recompute
        //    mounts using Plan 2's helper.
        let envelope = match envelopes::lookup_envelope(&self.envelopes, &self.profile_name_for(session_entry.profile)) {
            Some(e) => e.clone(),
            None => {
                reply_msg.words[0] = Error::NotFound.to_errno() as usize;
                if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
                return Ok(());
            }
        };
        let resolved_mounts = envelopes::resolve_session_mounts(
            &envelope, 1, session_entry.vt_index, &session_entry.username
        );
        let view_mounts = Self::build_view_from_mount_strings(&resolved_mounts);
        let resolved_env = envelopes::resolve_env(&envelope, &session_entry.username);
        let (user_env, user_envc) = build_envelope_env_payload(&resolved_env);

        // 4. Spawn. argv = [basename]; FDAC = empty (the app posix_spawn's its
        //    own children with FDAC if needed — cluuterm does PTS_REGISTER +
        //    posix_spawn for /bin/shell internally).
        let basename = path_str.rsplit_once('/').map(|(_, b)| b).unwrap_or(path_str);
        let (argv_payload, argc) = build_argv_payload(&[basename]);
        let new_cid = self.next_container_id();

        match self.spawn_service_with_env(
            path_str,
            DEFAULT_PRIORITY,
            &argv_payload,
            argc,
            &user_env,
            user_envc,
            self.procmgr_main_tid()?,
            self.next_spawn_seq(),
            self.clock_sample(),
            &[],                  // no FDAC
            session_entry.profile,
            0,
            0,
            &[],
            Some(&view_mounts),
            &[],
            &[],
            THREAD_CREATE_START_SUSPENDED,
        ) {
            Ok((thread_token, _cookie, pid, _)) => {
                self.pid_to_container_id.insert(pid, new_cid);
                self.install_view_and_run(thread_token, &view_mounts, session_entry.profile, new_cid);
                self.container_instances.insert(new_cid, ContainerInstance {
                    name: String::from(basename),
                    instance_name: self.next_instance_name(new_cid, basename),
                    session_id: claimed_session_cid,
                    container_id: new_cid,
                    parent_container_id: claimed_session_cid,
                    pid,
                    image_path: String::from(path_str),
                    mapped_pages: (SERVICE_STACK_SIZE / PAGE_SIZE + 1) as u32,
                    restart_policy: RestartPolicy::Never,
                    restart_count: 0,
                    last_exit_code: 0,
                    restart_attempt_start: 0,
                    quota: QuotaSpec::default(),
                    live_processes: 1,
                });
                self.container_children
                    .entry(claimed_session_cid)
                    .or_insert_with(Vec::new)
                    .push(new_cid);
                let _ = debug_print(&format!(
                    "procmgr: SPAWN_SESSION ok path={} pid={} session_cid={}",
                    path_str, pid, claimed_session_cid
                ));
                reply_msg.words[0] = 0;
                reply_msg.words[1] = pid;
            }
            Err(e) => {
                let _ = debug_print(&format!(
                    "procmgr: SPAWN_SESSION spawn failed path={} err={:?}", path_str, e
                ));
                reply_msg.words[0] = e.to_errno() as usize;
            }
        }
        if let Some(tok) = reply_token { let _ = ipc::reply(tok, &reply_msg, IpcFlags::empty()); }
        Ok(())
    }
```

Helper `profile_name_for(profile: CapProfile) -> &str` may already exist; if not, add a `match` from `CapProfile::User => "user"`, `::Admin => "admin"`, etc.

- [ ] **Step 3: Build**

```bash
cargo xtask build 2>&1 | tail -5
```

Expected: clean build.

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/main.rs userspace/libcluu/src/ipc.rs
git commit -m "procmgr: handle PROCMGR_SPAWN_SESSION_LABEL broker requests"
```

---

## Task 3: Compositor sends broker request on menu activation

**Files:**
- Modify: `userspace/compositor/src/main.rs` — the menu activation handler.

- [ ] **Step 1: Locate the menu activation path**

Per the compositor-menus spec memory, `F1` opens the system menu, `Apps` submenu lists apps from Cluufiles. Find the handler — search `compositor: spawn_demo: requested` (existing trace) for the test path.

- [ ] **Step 2: Replace direct procmgr-spawn with PROCMGR_SPAWN_SESSION**

Wherever the menu activates an app today (likely a call to procmgr's existing spawn channel), replace with:

```rust
    let procmgr_ep = registry::subscribe_output("procmgr", "spawn")?;
    let path = "/var/images/cluuterm/bin/cluuterm";
    let mut payload = Vec::new();
    payload.extend_from_slice(path.as_bytes());
    payload.push(0);
    let msg = Message::new(
        libcluu::ipc::PROCMGR_SPAWN_SESSION_LABEL,
        [payload.len(), my_session_cid, 0, 0, 0, 0],
        3,
    );
    let mut reply = Message::new(0, [0; 6], 0);
    if let Err(e) = libcluu::ipc::call_with_payload(procmgr_ep, &msg, &payload, &mut reply) {
        debug_print(&alloc::format!("compositor: SPAWN_SESSION failed {:?}", e));
        return;
    }
    if reply.words[0] != 0 {
        debug_print(&alloc::format!(
            "compositor: SPAWN_SESSION errno={} path={}", reply.words[0], path
        ));
        return;
    }
    debug_print(&alloc::format!(
        "compositor: SPAWN_SESSION ok pid={} path={}", reply.words[1], path
    ));
```

`my_session_cid` is the compositor's session container id — pass it through from procmgr at compositor spawn time. Easiest path: add a `PARAM_SESSION_CID` slot in ProcessInfo populated by procmgr in the user-compositor spawn. Then compositor reads `info.params[PARAM_SESSION_CID]` at startup.

- [ ] **Step 3: Plumb PARAM_SESSION_CID through**

- In `userspace/libcluu/src/boot.rs`, add `pub const PARAM_SESSION_CID: usize = 17;` (next free).
- In procmgr's user-compositor spawn (Plan 3 Task 4), add the override `(libcluu::boot::PARAM_SESSION_CID, session_cid as u64)` to `param_overrides`.
- In compositor startup, read it into a local `my_session_cid: u64`.

- [ ] **Step 4: Build + smoke**

```bash
cargo xtask build 2>&1 | tail -3
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_compositor_swap_login bash scripts/harness_run.sh
```

Existing markers still pass.

- [ ] **Step 5: Commit**

```bash
git add userspace/libcluu/src/boot.rs userspace/procmgr/src/main.rs userspace/compositor/src/main.rs
git commit -m "compositor: send PROCMGR_SPAWN_SESSION on menu activation"
```

---

## Task 4: Harness marker — compositor menu launches cluuterm

**Files:**
- Modify: `scripts/harness_case_defaults.sh` + `scripts/harness_run.sh`

- [ ] **Step 1: Add marker**

In `scripts/harness_case_defaults.sh`:

```bash
            l2_compositor_menu_cluuterm)
                TEST_COMMAND=""
                # After login, the user compositor spawns initial cluuterm
                # automatically (today). Then ctrl-alt-n opens a second
                # cluuterm via the menu/broker path. Marker checks for two
                # SPAWN_SESSION ok traces.
                SENDKEY_SEQUENCE_DEFAULT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 1\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 3\nsendkey ctrl-alt-n'
                ;;
```

In `scripts/harness_run.sh`:

```bash
    l2_compositor_menu_cluuterm)
        required_markers=(
            "TSC calibrated"
            "compositor: session_mode=1 (user)"
            "compositor: SPAWN_SESSION ok"
            "procmgr: SPAWN_SESSION ok"
        )
        ;;
```

- [ ] **Step 2: Run**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_compositor_menu_cluuterm bash scripts/harness_run.sh
```

- [ ] **Step 3: Commit harness**

```bash
git add scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "harness: l2_compositor_menu_cluuterm marker for broker spawn"
```

---

## Task 5: Reject cross-session and unauthorized SPAWN_SESSION

**Files:**
- Add tests inside `userspace/procmgr/src/main.rs` if a unit-test surface exists for procmgr (otherwise covered by harness manual smoke).

- [ ] **Step 1: Manual smoke — spawn a malicious sender**

Use an existing probe binary (e.g., `userspace/probes/ownerprobe/src/main.rs`) to send a `PROCMGR_SPAWN_SESSION_LABEL` request with a session_cid the prober doesn't own. Expected reply: `errno = EPERM`.

Add a new harness MARKER_MODE `l2_spawn_session_reject` that boots a probe binary doing this and grep for `procmgr: SPAWN_SESSION reject: sender_tid=` in the serial log.

- [ ] **Step 2: Commit the probe + harness**

```bash
git add userspace/probes/spawnsessprobe/src/main.rs scripts/harness_case_defaults.sh scripts/harness_run.sh
git commit -m "probe+harness: verify SPAWN_SESSION rejects wrong-session callers"
```

---

## Task 6: Memory updates

**Files:**
- Add: `/home/vlb2bp/.claude/projects/-home-vlb2bp-git-cluu/memory/project_spawn_session_broker_2026_05_14.md`
- Edit: `MEMORY.md`.

- [ ] **Step 1: Write memory**

```markdown
---
name: spawn-session-broker-2026-05-14
description: "Compositor opens apps via PROCMGR_SPAWN_SESSION_LABEL broker. Procmgr verifies sender_tid == session compositor; spawns app as session-container sibling with user envelope."
metadata:
  type: project
---

User-mode compositor holds no spawn capability. To open an app, it sends
PROCMGR_SPAWN_SESSION_LABEL { session_cid, image_path } to procmgr.
Procmgr authenticates sender_tid against the session's root pid main
thread, resolves the user envelope again, spawns as a sibling under
session_cid. Replies with pid on success or errno on reject.

**Why:** Keeps the broker pattern intact (procmgr is the only entity
with spawn rights). Compositor is just another userspace process. The
sender_tid check makes the broker un-bypassable from rogue session
processes.

**How to apply:** Any new "compositor opens X" path must go through
this label. Never give the compositor a direct procmgr spawn token.
```

---

## Self-review checklist

- `l2_compositor_menu_cluuterm` green.
- `l2_spawn_session_reject` confirms a non-compositor caller is rejected.
- Compositor binary contains zero direct `endpoint_create` / `thread_create` calls (grep).
- Memory updated.
