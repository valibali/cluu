# Mount-Policy Design — Per-Path Inheritance Declaration in Cluufile

**Status:** Design — pending user review
**Date:** 2026-04-23
**Authors:** Balazs Valkony (user), Claude Opus 4.7 (collaborator)

## Problem

CLUU containers inherit their VFS view from the parent container when nested
(`userspace/procmgr/src/main.rs:4549`), but `/tmp` is unconditionally swapped
to a fresh per-container MemFs (procmgr:4583–4593), regardless of nesting.
This is inconsistent with how every other path behaves and breaks the
shell-pipeline ergonomics that Plan 2's `/bin/rm`, `/bin/cp`, `/bin/mv` tests
depend on:

```
spawn mkdir /tmp/rmtest      # container A writes to A's MemFs
spawn mkdir /tmp/rmtest/inner # container B sees empty /tmp (fresh MemFs) → NotFound
spawn rm -r /tmp/rmtest      # container C also sees empty /tmp → NotFound
```

This also explains the known-flake `l2_owner_deny` test: its failure mode is
the same cross-container `/tmp` isolation.

The goal is to replace the hardcoded `/tmp`-specific isolation with a
principled, declarative surface in Cluufile that container authors use to
express per-path inheritance intent.

## Non-Goals

- Shared `/tmp` across unrelated sessions (tmpfs gets its scope from where it
  is first established in the call chain; unrelated top-level containers still
  have their own `/tmp`).
- Backing-store choice (MemFs vs ext2) — that is `PERSISTENT`'s job. Mount
  policy only governs whether a path inherits its parent's mount or gets a
  fresh backend of whatever type is already configured.
- Kernel-level changes. This is entirely userspace (procmgr + container-build
  tool + VFS view-registration path).

## Design

### New Cluufile directive

```
MOUNT <path> <policy>
```

Where `<policy>` ∈ `{inherit, private, ro}`:

| Policy    | Semantics                                                                 |
|-----------|---------------------------------------------------------------------------|
| `inherit` | Use parent's mount entry for this path verbatim (same backend, same src). |
| `private` | Replace any inherited mount with a fresh per-container backend.           |
| `ro`      | Inherit parent's mount but force `writable = false` for this container.   |

Multiple `MOUNT` directives are allowed (one per path). Re-declaring the same
path is an error (caught at Cluufile parse time).

### Default policy (when Cluufile declares no `MOUNT`)

| Path                      | Current Default | Proposed Default | Change? |
|---------------------------|-----------------|-----------------|---------|
| `/tmp`                    | private         | **inherit**     | YES     |
| `/log`                    | private         | private         | —       |
| `/data` (w/ PERSISTENT)   | private         | private         | —       |
| all others (view-inherited) | inherit       | inherit         | —       |

The only default that changes is `/tmp`. Everything else keeps current
behavior. `/log` stays private because per-container log scopes are the point
of `/log`. `/data` via `PERSISTENT` stays private because its whole purpose is
per-container persistent storage.

Rationale for `/tmp → inherit`: it matches shell-pipe ergonomics (the shell
and every program it spawns share a `/tmp`), and containers that want the old
per-spawn-private behavior opt in with a one-line Cluufile entry:

```
MOUNT /tmp private
```

### Interaction with `DENY_INHERIT`

`DENY_INHERIT` is the whole-view sledgehammer: when set, the nested container
starts from an empty view (only its declared image dirs). `MOUNT` directives
are **ignored** when `DENY_INHERIT` is set, because there is nothing to inherit
in the first place; all paths are private by definition. This matches what
`DENY_INHERIT` already does.

### Interaction with `DENY`

`DENY <path>` filters out the path from the inherited view. `MOUNT <path> X`
on a path also listed in `DENY` is an error at Cluufile parse time (ambiguous
intent).

### Interaction with `PERSISTENT`

`PERSISTENT <path>` already establishes a fresh per-container ext2 backend at
that path — semantically equivalent to `MOUNT <path> private` with a specific
backend. Declaring both `PERSISTENT` and `MOUNT` on the same path is an error
at Cluufile parse time (redundant/conflicting intent). A path with `PERSISTENT`
and no `MOUNT` behaves as private (preserved current behavior).

### Interaction with nesting (top-level vs nested)

The policy only engages when this spawn has a parent to inherit from
(`caller_container_id != 0`). For top-level spawns (boot, autostart), every
path is private by construction — there is no parent view. `MOUNT inherit` on
a top-level spawn is a no-op (not an error — useful for containers that can
run in either role).

### `/tmp` scope: who establishes it?

Under the new rules, `/tmp` is established at the topmost container that
doesn't set `MOUNT /tmp inherit` (i.e., uses the default inherit OR explicitly
declares `private`). For a typical user session:

- `init` (top-level): gets a private `/tmp` (boot-scoped, unused).
- `shell` (nested under init, autostart manifest): inherits — but init's
  `/tmp` is unused, so shell effectively gets its session `/tmp` here. **The
  shell's Cluufile should declare `MOUNT /tmp private`** to make the scope
  boundary explicit: every program the shell spawns inherits this per-shell
  `/tmp`, but the shell itself doesn't share `/tmp` with init.
- `/bin/mkdir`, `/bin/rm` (nested under shell): inherit shell's `/tmp`.

This shell-anchored design gives you exactly one `/tmp` per login session, and
containers inside that session share it.

## Implementation Surfaces

### Cluufile parser (`tools/container-build/src/main.rs`)

Add `MOUNT` to the directive match, emit manifest entries:

```toml
[[mounts.policy]]
path = "/tmp"
policy = "private"
```

Parse validation:
- `<policy>` must be one of `inherit`, `private`, `ro`
- No two `MOUNT` directives for the same path
- Conflict check with `DENY` list

### Manifest (`target/containers/<name>/manifest.toml`)

New optional table:

```toml
[[mounts.policy]]
path = "/tmp"
policy = "private"
```

Backward compatible — existing manifests without this table get default
policies applied.

### Procmgr (`userspace/procmgr/src/main.rs:4540–4612`)

Replace the hardcoded `/tmp` swap block with a policy-driven loop:

```rust
let mount_policies = doc.table("mounts")
    .and_then(|t| t.get_array("policy"))
    .map(parse_mount_policies)
    .unwrap_or_default();

for (path, policy) in resolve_effective_policies(&mount_policies, defaults) {
    match policy {
        MountPolicy::Inherit => { /* no-op: already in view_mounts from inheritance */ }
        MountPolicy::Private => {
            apply_private_mount(&mut view_mounts, &path, container_id);
        }
        MountPolicy::Ro => {
            mark_readonly(&mut view_mounts, &path);
        }
    }
}
```

The `defaults` table encodes the table above (`/tmp → inherit`,
`/log → private`, etc.).

### VFS side (`userspace/vfs/src/main.rs:706–760`)

The VFS already accepts any `view::ViewMount` list it's given. No logic change
needed in VFS; procmgr constructs the right list upstream.

### Existing Cluufiles

- `containers/shell/Cluufile`: add `MOUNT /tmp private` (explicit
  session-scope boundary; future-proof documentation).
- All other existing Cluufiles: no change needed. Default-inherit `/tmp`
  matches what nested containers want anyway.

## Test Plan

1. **Regression**: full harness matrix passes, including `l2_owner_deny`
   (previously known-flaky — should now go reliably green or reliably red with
   a clearer failure mode depending on what the test actually asserts; expect
   a separate small fix if its fail mode was genuinely relying on the
   isolation-as-bug).

2. **Plan 2 unblock**: `l2_mkdir`, `l2_rm`, `l2_cp`, `l2_mv`, `l2_rm_root_refuse`
   all pass. The shell autostart `spawn mkdir x; spawn mkdir x/y; spawn rm -r x`
   works end-to-end because `x` persists across nested spawns.

3. **Isolation verification**: new harness case `l2_mount_private` that
   declares a container with `MOUNT /tmp private`, verifies its `/tmp` does
   NOT see the shell's `/tmp/...` contents.

4. **Default preservation**: `/log` does not bleed across spawns (existing
   behavior stays).

## Scope Boundary

In scope for the implementation plan:
- `MOUNT` directive parsing + manifest serialization + procmgr consumption
- Default policy table (`/tmp` flips, others preserved)
- `l2_mkdir/rm/cp/mv` harness case revalidation
- One new isolation-verification case
- Updating `shell` Cluufile with explicit `MOUNT /tmp private`

Out of scope (follow-ups if needed):
- `ro` policy implementation if not trivially useful in the short term (can
  ship with `inherit` and `private` only and add `ro` later).
- A `MOUNT /tmp shared <name>` variant for cross-session coordination.
- Revisiting `l2_owner_deny` semantics — that test probably needs a
  redesign regardless of this fix.

## Risks

- **Session boundary inconsistency** if `shell/Cluufile` is not updated with
  `MOUNT /tmp private`: the shell would share its `/tmp` with its parent
  (init), which is invisible and broken. Mitigation: the plan task for the
  shell Cluufile update is a hard prerequisite, and init's `/tmp` is never
  actually mounted anywhere visible, so practically the failure mode is just
  "shell's `/tmp` scope is implicit" — annoying but not corrupt.

- **Existing Cluufiles behaving unexpectedly**: any container that was relying
  on `/tmp` being fresh (e.g., a test probe that assumes `/tmp` is empty at
  startup) will now see the shell's `/tmp` contents. Fix: audit probes; add
  explicit `MOUNT /tmp private` where needed. The only known probe in this
  category at time of writing is none — grep confirmed.

## Decisions

- `<policy>` values: `inherit`, `private`, `ro` — user accepted.
- Default for `/tmp`: `inherit` — user prefers shell-pipe ergonomics over
  per-spawn isolation.
- `ro` can ship or be deferred; plan decides based on implementation cost.
- Cluufile directive name: `MOUNT`. Alternative considered: `VIEW` (matches
  VFS terminology). `MOUNT` wins because the manifest already uses `[mounts]`
  as the table name.
