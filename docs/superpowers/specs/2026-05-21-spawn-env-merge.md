# Spec: Procmgr-side env merge for `handle_spawn_unified`

## Problem

Shell logged into a session never sees `PATH`, `HOME`, or `USER`, so any
bare command (e.g. `ls`) ends with `shell: unsupported command`.

Trace at HEAD `ebad680`:

1. `login` creates session with `ProfileSpec.env = {HOME, USER, TERM}`.
2. `login` spawns `cluuterm` with `SpawnEnvelope.env = {HOME, USER}`.
3. `cluuterm` spawns `shell` with `SpawnEnvelope.env = {TERM}`.
4. `procmgr::handle_spawn_unified` copies `envelope.env` verbatim into
   the argv/env payload — no envelope-default merge.
5. Shell reaches `path_lookup`, walks an empty `$PATH`, rejects `ls`.

The chain works for `procmgr::spawn_service_with_env` (auto-login, su)
because that path calls `envelopes::resolve_env(&envelope, &username)`
before packing the wire payload. The unified spawn path skips that step.

## Goal

Single source of truth for default env (`PATH`, `SHELL`, `LANG`, `TERM`,
`HOME`, `USER`, `LOGNAME`, `PWD`) lives in `/etc/envelopes.toml`.
`handle_spawn_unified` merges that profile's resolved env under the
caller-supplied `envelope.env`, then packs the result.

Caller env wins on key conflict (matches `spawn_service_with_env`).

## Non-Goals

- Don't touch `cluuterm` or `login` env lists. They stay minimal.
- Don't add a new IPC verb or change `SpawnEnvelope` wire format.
- Don't change boot autostart / service spawning (already merges via
  `spawn_service_with_env`).
- No `environ`-forwarding from caller's libc state. Procmgr is the
  policy authority; clients don't carry env.

## Design

### Resolution

In `handle_spawn_unified`, after `envelope` is deserialized and the
manifest cache is warm:

1. Resolve caller's session via `resolve_caller_session(sender_tid)`.
   - **None** (no enclosing session — boot/service path): skip merge,
     pack `envelope.env` as-is. Same behavior as today.
   - **Some(session)**: use `session.username` to look up
     `user_records[username].profile_name` (envelope name).
2. `envelopes::lookup_envelope(&self.envelopes, &profile_name)` →
   `Option<&Envelope>`. If `None`, log warning, fall through to
   no-merge (preserve current bypass-friendly behavior).
3. `envelopes::resolve_env(envelope_def, &username)` →
   `BTreeMap<String, String>` with `{user}` substituted.
4. Merge: start from resolved envelope env. For each `(k, v)` in
   `envelope.env`, **overwrite**. Caller wins.
5. Pack `BTreeMap` into wire format with the existing
   `build_envelope_env_payload(&merged)` helper.

### Why caller wins

- Login may set `HOME=/home/balazs/work` — must not be silently rebased
  to `/home/balazs` by envelope template.
- `TERM=xterm-256color` from cluuterm must beat `TERM=cluu` default.
- Allows future per-image overrides without policy changes.

### Why no merge on no-session

- Procmgr bootstraps itself before session table exists. The
  primordial spawn path doesn't go through `handle_spawn_unified`, but
  service spawns (init, vfs, registry) might — and they should not
  inherit `[envelope.user]` defaults.
- `spawn_service_with_env` already handles its own env explicitly.

### Behavior table

| Caller has session | Envelope found | Behavior                       |
|--------------------|----------------|--------------------------------|
| yes                | yes            | merge: envelope ∪ caller       |
| yes                | no             | caller-only (+ warning log)    |
| no                 | n/a            | caller-only (current behavior) |

## Acceptance

- After login → cluuterm → shell, typing `ls` resolves via `$PATH` and
  attempts spawn. (Whether `ls` is wired is out of scope.)
- `HOME=/home/<user>` and `USER=<user>` reach the shell.
- `env` builtin (or `echo $PATH`) shows `/bin:/usr/bin` for user
  profile.
- Caller-supplied `TERM=xterm-256color` (from cluuterm) survives the
  merge; envelope default `TERM=cluu` is shadowed.
- No regression in service spawn (procmgr's own boot path, init's
  primordial spawns).

## Files touched

- `userspace/procmgr/src/main.rs` — `handle_spawn_unified` body, ~30
  lines inserted before `for (k, v) in &envelope.env` block.
- `userspace/procmgr/src/envelopes.rs` — drop `#[allow(dead_code)]`
  on `lookup_envelope` and `resolve_env` (they become live).
- Test only: `userspace/procmgr/src/envelopes.rs` `#[cfg(test)]` block
  — already covers `resolve_env`; one more case for merge precedence.

No new files, no wire changes, no protocol bump.

## Related

- [[project_envelope_substitution_2026_05_14]] — envelope vt/{user}
  substitution; this spec extends the same merge to unified-spawn path.
- [[project_spawn_hooks_unwired]] — `handle_spawn_unified` currently
  bypasses spec-1 hooks; merge lands above the bypass, unaffected.
