# Monotone views audit — 2026-05-14

After Plan 2 lands, every `set_view` call constructs a child view either from
`resolve_session_mounts` (envelope-driven) or from a Cluufile MOUNT directive.
Both must be narrower than (or equal to) the parent view they descend from.

**Monotone rule**: a child view may only expose paths that appear in the parent
view, and may only be writable where the parent is writable.

---

## Where `set_view` is called

`VfsViewTable::set_view` lives in `userspace/vfs/src/view.rs:71`.  It is
called exclusively from `vfs/src/main.rs:943` (inside `handle_set_view`) and
from the procmgr-side by the intermediate helpers:

```
procmgr::register_vfs_view_for_thread  →  send_vfs_set_view  →  vfs::handle_set_view  →  views.set_view
procmgr::flush_pending_vfs_views       →  send_vfs_set_view  →  (same)
procmgr::register_manager_vfs_view    →  send_vfs_set_view  →  (same)  [bootstrap only]
```

So the real question is: who calls `register_vfs_view_for_thread` (via
`install_view_and_run`) and what view do they build?

---

## Sites

| # | Call site (file:approx-line) | Context | Parent view source | Child view source | Narrows? | Notes |
|---|---|---|---|---|---|---|
| 1 | `main.rs:740` (register_manager_vfs_view) | Bootstrap: procmgr installs its own view in VFS before any client exists. | No parent — this IS the root view. | `default_view_for_profile(SUPERVISOR)` → `SUPERVISOR_MOUNTS` = `rw:/` | N/A — root | Bootstrap only. Guarded: called once, `manager_vfs_view_registered` flag prevents re-install. |
| 2 | `main.rs:1168` (try_auto_login) | Harness autostart: shell process for auto-login root at VT:0 | Procmgr's own SUPERVISOR view (`rw:/`). | `build_view_from_envelope(&envelope)` from the "supervisor" or "admin" envelope in `envelopes.toml`. Admin envelope lists `rw:/`, so the shell gets `rw:/`. User envelope would give narrower paths. | YES — envelope mounts are always a strict subset of `rw:/`. | `pid_to_view` records the new view. No envelope provides paths outside `rw:/`. |
| 3 | `main.rs:1344` (autostart_container / first-boot restart path) | Autostart binary (vtmgr, compositor, vt, etc.) spawned at boot. Caller is procmgr (no user container). | No parent container (`parent_container_id = 0`). Procmgr's view is SUPERVISOR. | `default_view_for_profile(requested_profile)`. Profile comes from image's Cluufile `[process] profile`. | YES — `default_view_for_profile` returns at most SUPERVISOR_MOUNTS (`rw:/`). A DEVICE-profile binary gets DEVICE_MOUNTS which is a strict subset of `rw:/`. | Top-level: no parent-view check needed. |
| 4 | `main.rs:1588` (restart path inside autostart_container) | Container restart after crash. Same as site 3 but for the restarted process. | Same as site 3 — procmgr root. | `default_view_for_profile(requested_profile)` (same as original). | YES — same profile, same view. No widening possible. | |
| 5 | `main.rs:2363` (handle_session_login, kind=1, graphical/cluuterm) | compositor-login path: authenticated user, spawns cluuterm. | Procmgr's SUPERVISOR view. | `build_view_from_mount_strings(resolve_session_mounts(&envelope, 1, vt, user))` — the `vt_graphical_mounts` list from the user's envelope. | YES — envelope graphical mounts are a subset of `rw:/`. See envelopes.toml: even the admin graphical envelope is bounded by explicit path list (not `rw:/` catch-all). | Parent identity is procmgr (implicit). `vt_index_graphical` is hardcoded to 4 (TODO noted inline). |
| 6 | `main.rs:2594` (handle_session_login, kind=0, tty) | tty-session login path: authenticated user, spawns shell on VT N. | Procmgr's SUPERVISOR view. | `build_view_from_mount_strings(resolve_session_mounts(&envelope, 0, vt, user))` — the `vt_text_mounts` list. | YES — text mounts (e.g. `ro:/bin`, `rw:/home/{user}`, `rw:/tmp`) are a strict subset of `rw:/`. | Parent identity is procmgr. |
| 7 | `main.rs:2818` (handle_escalate / sudo) | Privilege escalation: caller authenticates, spawns elevated command. | Caller's recorded view (`pid_to_view[caller_pid]`). | `build_view_for_profile_and_home(escalate_profile, user_home)` → starts from `admin_session_mounts()` or `default_mounts_for_profile(profile)` then appends `/home/<user>`. | CONCERN — see note. | The elevated view is built from the *escalation profile's* default mounts, NOT from the caller's view. For a `user`-profile caller escalating to `admin`, the child gets ADMIN_MOUNTS which is *different* (though generally comparable) to the caller's envelope view. No strict subset check is done here. However the caller must supply a valid password and `escalate` field in users.toml caps the ceiling, so the profile cannot exceed what was pre-authorized. The `can_narrow_view` function exists but is **not called** on this path. |
| 8 | `main.rs:3054` (handle_su) | `su` to a different user identity. Caller is an existing shell session. | Caller's container view (`pid_to_view[caller_pid]` or procmgr default). | `build_view_from_envelope(&target_envelope)` — builds from target user's envelope. | CONCERN — see note. | Same structural issue as site 7: child view is determined by *target identity's envelope* not by intersection with caller's view. `su root` from a user shell gives the root envelope (`rw:/`). This is by design (su is an identity switch), but it means the view **can widen** relative to the caller's view. The monotone rule holds *from procmgr's perspective* (procmgr always has SUPERVISOR view), but not *from the caller-shell's perspective*. |
| 9 | `main.rs:4121` (spawn_service, internal) | Internal procmgr-initiated service launch (called from the init path for primordial daemons). | No parent — procmgr is the spawner. | `default_view_for_profile(requested_profile)`. | YES — procmgr is root; any profile is a subset. | container_id=0 so no MemFs is appended. |
| 10 | `main.rs:4295` (handle_spawn_message / PROCMGR_SPAWN_LABEL) | POSIX `posix_spawn`: child forked by an existing container process. | Caller's container view: `pid_to_view[caller_pid]` (clone). | Exact clone of caller's view (`child_view_mounts`). | YES — clone is always equal to, never wider than, the parent. | This is the safest path: strict inheritance. |
| 11 | `main.rs:5673` (handle_container_run / PROCMGR_CONTAINER_RUN_LABEL) | `CONTAINER_RUN`: user runs a named image via the shell. | Caller's recorded view (`pid_to_view[caller_pid]`). | Complex — three branches: (a) nested+allow-inherit: caller's view filtered through `deny_paths`; (b) `deny_inherit=true`: image_dirs only; (c) top-level: `default_view_for_profile`. Then per-container system mounts (/data, /tmp, /log) are prepended using `policy_driven_memfs_mounts`. Finally `/` MemFs catch-all is appended. Strict UE13 `validate_cluufile_against_parent` check is run at line 5405. | YES (branches a,b) / CONCERN (branch c). | Branch (c) (top-level, `caller_container_id == 0`) uses `default_view_for_profile` from the *requested profile*, skipping any caller-view check. This path is reached when procmgr itself (no container_id) runs a container — correct since procmgr is always root. Branches (a) and (b) filter down or restrict. The UE13 validation at line 5405 guards Cluufile MOUNT policies against the parent view for the nested path. |

---

## Summary of concerns

### Site 7 (escalate/sudo) and Site 8 (su): intentional widening

Both `handle_escalate` and `handle_su` can produce a child view **wider** than
the caller's current envelope view:

- `su root` from a `user`-profile shell → child gets `rw:/` (admin/supervisor envelope).
- `sudo` with `escalate_profile = SUPERVISOR` → child gets SUPERVISOR_MOUNTS.

**Assessment**: This is *by design* — both operations are identity switches
backed by explicit authentication (password + `users.toml` ACL). The "parent"
in the monotone rule for these two sites is not the *caller's view* but
**procmgr's own view** (SUPERVISOR), since procmgr is the actual spawner and
always holds `rw:/`. So the rule still holds from the kernel-authority
perspective: the privilege escalation is authorized by procmgr on behalf of a
pre-authenticated identity. The `can_narrow_view` function (procmgr:6683)
exists but is not invoked here; adding an assertion would be a false-positive
for legitimate privilege escalation.

**Recommendation**: Document in code that escalate/su are *exempted* from
caller-view monotone checking because procmgr, not the calling process, is
the authoritative spawner. The `can_narrow_view` predicate should be extended
with a `skip_for_privileged_spawn` comment at its call site.

### Site 11 (CONTAINER_RUN, top-level): no caller-view check

When `caller_container_id == 0` (procmgr is the indirect caller), the
container view is built from `default_view_for_profile(requested_profile)`.
This is correct: procmgr's view is SUPERVISOR, so any profile-based subset
passes. No violation.

---

## Runtime assertion in `vfs/src/view.rs::set_view`

The `set_view` method signature is:

```rust
pub fn set_view(&mut self, client_id: usize, view: VfsView)
```

There is **no parent identity** available at this callsite. The VfsViewTable
knows nothing about who spawned whom; it only maps client_id → VfsView. The
parent-child relationship lives entirely in procmgr (`pid_to_container_id`,
`container_instances.parent_container_id`, `pid_to_view`).

Options considered:

1. **Add `parent_client_id: Option<usize>` parameter to `set_view`** — would
   require threading it through `handle_set_view` (VFS) and
   `send_vfs_set_view` (procmgr wire format). Feasible but changes the IPC
   wire format (adds a word to `VFS_SET_VIEW_LABEL`). Medium effort.

2. **Assert at the procmgr call site** (in `install_view_and_run` or
   `register_vfs_view_for_thread`) using `can_narrow_view` — no wire format
   change, procmgr already has both the parent view and the new view before
   sending to VFS. This is where the validation logically belongs.

3. **Leave VFS assertion as a TODO** and implement option 2 — chosen approach.

A `#[cfg(debug_assertions)]` check has been added to
`procmgr::install_view_and_run` (the single choke-point through which all
`register_vfs_view_for_thread` calls flow) to compare the new child view
against the parent's recorded view, with the escalate/su exemption documented
inline.

### Skip mechanism (post-fix)

The assertion has two explicit skip conditions, passed as parameters to
`install_view_and_run`:

- `parent_container_id == 0` — top-level/init spawns where procmgr is the
  authority; no parent view to check against.
- `is_identity_switch == true` — escalate/su paths (sites 7 and 8 above);
  the new view is authorized by procmgr policy, not constrained by the
  caller's envelope.  These callers pass `caller_container_id` as the parent
  but set `is_identity_switch = true` to suppress the assertion.

Prior to the fix in commit after `aaf754c`, the assertion was **non-functional**:
it attempted to look up the *child*'s `container_instances` entry to find
`parent_container_id`, but the child has not yet been inserted into
`container_instances` at the time of the call.  The lookup always returned
`None`, the `if parent_cid != 0` branch never executed, and the assertion
never fired.  The fix passes `parent_container_id` explicitly from every call
site so the lookup uses the *parent*, which is already registered.

Call sites 9 (posix_spawn, `main.rs:4400`) and 10 (container_run,
`main.rs:5780`) were also missing the `is_identity_switch` argument
(compile error after the 6-arg signature was added); both now pass `false`.

See the assertion at `userspace/procmgr/src/main.rs` inside
`install_view_and_run`, and the TODO comment in `userspace/vfs/src/view.rs`.

---

## Files changed

- `userspace/vfs/src/view.rs` — TODO comment with reasoning in `set_view`.
- `userspace/procmgr/src/main.rs` — `#[cfg(debug_assertions)]` check in
  `install_view_and_run`.
