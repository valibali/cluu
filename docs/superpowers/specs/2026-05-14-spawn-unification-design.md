# Spawn-path unification: posix_spawn under CONTAINER_RUN

**Date:** 2026-05-14
**Status:** spec draft
**Why:** Every CLUU process is a container, and the manifest is the
authoritative declaration of what the process can touch. The dual-path
`PROCMGR_SPAWN_LABEL` (path-based, manifest-less) + `PROCMGR_CONTAINER_RUN_LABEL`
(image-name, manifest-driven) is historic debt that already produced
Bug B (env drop on posix_spawn). Unify on the manifest path.

## Goals

1. POSIX `posix_spawn("/bin/X", argv, envp)` continues to work at the
   userspace API surface. Source compatibility for ported POSIX programs
   stays.
2. Under the hood, every spawn resolves to an image name and goes
   through `CONTAINER_RUN`. Procmgr has ONE spawn handler.
3. Every spawned process has a manifest (Cluufile) that declares its
   rights, mounts, endpoints, devices. No implicit grants.
4. `PROCMGR_SPAWN_LABEL` and `handle_spawn_message` are deleted.

## Non-goals

- Source-level changes to ported POSIX binaries (newlib, MicroPython,
  etc.). The libcluu `posix_spawn` shim handles the translation.
- Performance regression for hot-path child spawns from the shell. The
  manifest read is cached after first use (per-image manifest cache).

## Architecture

### Client side: libcluu `posix_spawn`

Currently `userspace/libcluu/src/posix/process.rs::posix_spawn` builds
`PROCMGR_SPAWN_LABEL` payload from `(path, argv, envp, fdactions)`.
After unification:

```
posix_spawn("/bin/ls", argv, envp):
    1. resolve /bin/ls → image name:
       - readlink "/bin/ls" → "/var/images/coreutils/bin/ls"
       - parse out the image name "coreutils" (between /var/images/ and /bin/)
       - if not a symlink, fall back to extracting image from manifest's
         entrypoint search (slow path; warn).
    2. build CONTAINER_RUN payload (same as build_container_run_payload_full):
       words: [name_offset_or_argc, image_name_len, fdac_offset, param_offset,
               param_count]
       payload: image_name + argv + FDAC + (env + cwd trailers).
    3. send PROCMGR_CONTAINER_RUN_LABEL.
    4. receive container_run reply, return as posix_spawn-style result.
```

The `/bin/<name>` symlinks are populated at image-install time. The
xtask image-builder generates them when it lays out `/var/images/<image>`
based on the manifest's `ENTRYPOINT` declaration.

### Filesystem layout

For each image `<name>`:
```
/var/images/<name>/
    manifest.toml
    bin/
        <entrypoint>     (real ELF)
        ...
```

Plus symlinks at boot/install time:
```
/bin/<entrypoint> → /var/images/<name>/bin/<entrypoint>
```

`Cluufile`'s `ENTRYPOINT /bin/<name>` directive tells the image builder
to create the symlink. If two images both declare `/bin/cluuterm`, the
later install wins (warn) — same conflict semantics as Linux distro
packages.

### Procmgr side

`handle_spawn_message` and `PROCMGR_SPAWN_LABEL` deleted entirely.
All spawns flow through `handle_container_run`. The "no SPAWN cap"
gate already implemented in `handle_spawn_message` moves into the
`handle_container_run` entry check: a caller without `RIGHT_CONTAINER_RUN`
(or a downgraded SPAWN-cap variant for "spawn within same image set")
gets `PermissionDenied`.

**Profile / capability derivation:**
- Manifest declares the maximum capability set the image asks for.
- Procmgr narrows down by the caller's view + session envelope.
- The child PROFILE = manifest-declared AND-ed with caller's grant
  authority. Same as today's `handle_container_run` logic.

**Backward-compat tail:** For pure ephemeral utilities (echo, true,
false), a minimal manifest is fine — `PROFILE empty`, `MOUNT /` (read-only
narrowed by parent view), no rights. This still respects narrowing.

## Migration steps

1. Audit every `posix_spawn` call in userspace. List source files and
   target paths. Verify each target has a corresponding image with
   manifest. If gaps exist, write/install missing manifests first.
2. Build the symlink generator in xtask. For each image's `ENTRYPOINT`,
   create `/bin/<entrypoint>` → `/var/images/<image>/bin/<entrypoint>`.
3. Add path → image-name resolver in libcluu (`posix_spawn` body change).
4. Replace `PROCMGR_SPAWN_LABEL` send with `PROCMGR_CONTAINER_RUN_LABEL`.
5. Delete `handle_spawn_message`. Keep `PROCMGR_SPAWN_LABEL` const for one
   release as a tombstone, then delete.
6. Add a regression harness marker that proves a path-based `posix_spawn`
   from the shell still works (no API break) AND now carries the manifest's
   declared rights, not the caller's free-form spread.
7. Performance verification: re-run `b_spawn_warm` and `l2_jobchurn_heavy`
   markers. Cache manifests in procmgr (per-image, invalidate on image
   reinstall) to avoid per-spawn VFS-read overhead.

## Risks

- **Symlink resolution at posix_spawn time** adds a VFS round-trip per
  spawn. Mitigate: libcluu caches resolved (path → image-name) pairs.
- **Manifest cache invalidation** when an image is reinstalled. Either
  send a `PROCMGR_IMAGE_INVALIDATE` notification, or stat the manifest
  on every spawn and check mtime. Cost minimal.
- **Symlink namespace collisions** if two images declare `/bin/X`. Make
  the installer enforce unique entrypoints, or scope per-session (later
  symlink wins). Decide before step 2.
- **Pre-existing binaries without manifests** (kernel-side test
  helpers?) — none today; verify in step 1.

## Open questions

- Should the per-session `/bin` symlink view differ by envelope? E.g.,
  user envelope sees only "user-safe" entrypoints, admin sees more.
  Maps cleanly onto the `vt_text` / `vt_graphical` envelope work.
  Probably YES but deferred to a follow-up.
- Caching strategy: per-procmgr-instance LRU, or unbounded growth?
  Image set is bounded (~ 30 today), so unbounded is fine.

## Acceptance criteria

- `cargo xtask build` clean.
- All current harness markers stay green.
- New marker `l2_spawn_via_manifest_only` proves a path-based
  `posix_spawn` carries the manifest's declared rights (NOT the caller's
  free-form set). Verifiable by spawning a binary whose manifest declares
  fewer rights than the caller — child must run with the narrower set.
- `git grep PROCMGR_SPAWN_LABEL` returns zero hits after step 5.

## Linked

- `docs/superpowers/plans/2026-05-14-plan2-envelope-vt-user-substitution.md`
  closed Bug B with a minimal env-trailer patch; this plan retires the
  dual-path entirely.
- Memory: `project_container_run_posix_spawn_unify.md`,
  `project_spawn_cap_composable.md`.
