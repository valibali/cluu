# CLUU user envelope + env / PATH / shellrc — design spec

**Date:** 2026-04-28
**Status:** Draft, awaiting user review.
**Author context:** spec arose from a Phase-2 MicroPython bug investigation —
`open('/etc/motd')` failed because no spawned binary's mount view included
`/etc/`. The fix turned out to be a missing infrastructure layer for
session-level env/mount setup, not a per-binary patch. This spec covers
that layer end-to-end.

---

## 1. Summary

Establish a **user envelope** at session-login time that defines the
mount view, environment variables, and PATH for a given user's profile
class (admin / user / service). The envelope flows through procmgr →
shell → spawned binaries via existing capability-derivation machinery.
Cluufile MOUNT directives narrow within the envelope per the existing
mount-policy work; mismatches between Cluufile demands and envelope
provisions cause spawn to fail loudly (strict semantics).

The shell gains POSIX bash-compatible behaviors: `export` semantics,
PATH-based bare-command resolution (typing `cat foo` instead of
`spawn cat foo`), and `~/.shellrc` sourcing. Newlib's `_environ` is
mirrored from the shell's exported vars one-way; C-side `setenv` from
a child does not propagate back.

**Primary user-visible win:** `cat /etc/motd | grep CLUU | head -3`
runs end-to-end with real file content. MicroPython's
`open('/etc/motd').read()` works. The "system feels like a system"
moment that ROADMAP.md §5 Phase 2 is supposed to deliver.

## 2. Goals & non-goals

**Goals:**

- Default mount view for every user-spawned binary includes `/etc`,
  `/lib`, `/usr`, `/bin` (read-only) plus `/var/log`, `/tmp`, `/home`
  (read-write).
- Admin-class users get `rw` on system paths.
- Cluufile MOUNT directives narrow within the envelope; mismatches fail
  spawn loudly (Q4 strict).
- Standard env vars (`HOME`, `USER`, `LOGNAME`, `PATH`, `SHELL`, `TERM`,
  `PWD`) are set at session-login per profile class.
- Shell supports `export`, `~/.shellrc`, and bare-command resolution.
- Monotonic-cap discipline preserved: `binary.caps ⊆ shell.caps ⊆
  envelope.caps ⊆ procmgr.caps`. Every step narrows; never widens.

**Non-goals (explicit, with rationale in §12):**

- Per-user envelope overrides (per-class only).
- Bidirectional env mirror.
- TOML-driven cap profile definitions (cap profiles stay hardcoded for
  v1; envelope migration is a self-contained step).
- Aliases, shell functions, configurable prompts.
- `env -i`, `env FOO=bar cmd` one-shot overrides.

## 3. Foundational decisions

Settled during brainstorming:

| Decision | Choice | Rationale |
|---|---|---|
| Envelope scope | Per-profile-class (admin/user/service) | Reuse the existing `users.toml` `profile` field; covers the realistic spread. |
| Storage | Separate `/etc/envelopes.toml` | Data-driven from day one; admin can edit without rebuild. |
| Mount syntax | Inline-table TOML | Most extensible for future modes (private, deny). |
| Cluufile composition | Strict (fail loudly on mismatch) | Cluufile is a *spec* of needs; silent narrow hides bugs. |
| Unmentioned paths | Inherit from envelope | "Open by default, narrow by Cluufile" — the principle. |
| PATH semantics | POSIX colon-list | Universal; any newcomer knows it. |
| Export semantics | Bash-style | `set` = local, `export` = inherited by children. |
| shellrc paths | Fixed `/etc/shellrc` + `~/.shellrc` | KISS. |
| Env mirror | Shell → newlib (one-way) | C-side `setenv` doesn't escape the binary; correct POSIX. |
| `{user}` substitution | Resolved at session-login | One-shot; no token leaks downstream. |

## 4. Architecture overview

Three layers, each owning one concern:

```
                   /etc/envelopes.toml       /etc/users.toml
                          │                         │
                          ▼                         ▼
                  ┌────────────────────────────────────┐
                  │ procmgr (boot)                     │
                  │  • parses both TOMLs               │
                  │  • on session-login:               │
                  │    user → profile → envelope       │
                  │    + {user}-substitute env_template│
                  │    → builds spawn block (mounts +  │
                  │      env) for shell                │
                  └────────────────────────────────────┘
                          │ container_run shell
                          ▼
                  ┌────────────────────────────────────┐
                  │ shell                              │
                  │  • inherits envelope mounts + env  │
                  │  • sources /etc/shellrc, ~/.shellrc│
                  │  • PATH lookup on bare commands    │
                  │  • export for child inheritance    │
                  │  • shell→newlib env mirror (one-way)│
                  └────────────────────────────────────┘
                          │ container_run binary
                          ▼  (with intersected mounts +
                          │   exported env subset)
                  ┌────────────────────────────────────┐
                  │ /bin/X binary                      │
                  │  • Cluufile MOUNT directives       │
                  │    must be ⊆ envelope (strict)     │
                  │  • Unmentioned paths inherit from  │
                  │    envelope                        │
                  │  • newlib getenv() reads env       │
                  └────────────────────────────────────┘
```

**Key invariant** (preserves monotonic-cap discipline):

```
binary.caps ⊆ shell.caps ⊆ envelope.caps ⊆ procmgr.caps
```

Each `⊆` is a narrowing, never a widening. The envelope is the
*highest cap-level any user binary can ever reach*. Procmgr's full FS
access never propagates to user code.

## 5. `/etc/envelopes.toml` schema

Three envelopes ship out-of-the-box: `admin`, `user`, `service`. Full
ship-as-default contents:

```toml
# /etc/envelopes.toml — system-wide envelope definitions.
# Per-profile-class. users.toml's [[user]].profile field selects which
# envelope is applied at session-login.

[[envelope]]
name = "user"

# Mounts: every path the binary may see, with rw/ro mode.
# Cluufile MOUNT directives narrow within these paths (§7).
# Cluufile-requested paths NOT in this list cause spawn to fail (§7).
mounts = [
    { path = "/",         mode = "ro" },
    { path = "/etc",      mode = "ro" },
    { path = "/lib",      mode = "ro" },
    { path = "/usr",      mode = "ro" },
    { path = "/bin",      mode = "ro" },
    { path = "/var/log",  mode = "rw" },
    { path = "/tmp",      mode = "rw" },
    { path = "/home",     mode = "rw" },
]

# Static env vars. Same string for every user with this profile.
[envelope.env]
SHELL = "/bin/shell"
TERM = "cluu"
PATH = "/bin:/usr/bin"
LANG = "C"

# Templated env vars. {user} substituted at session-login.
[envelope.env_template]
HOME = "/home/{user}"
USER = "{user}"
LOGNAME = "{user}"
PWD = "/home/{user}"


[[envelope]]
name = "admin"

mounts = [
    { path = "/",         mode = "rw" },
    { path = "/etc",      mode = "rw" },
    { path = "/lib",      mode = "rw" },
    { path = "/usr",      mode = "rw" },
    { path = "/bin",      mode = "rw" },
    { path = "/var",      mode = "rw" },
    { path = "/tmp",      mode = "rw" },
    { path = "/home",     mode = "rw" },
]

[envelope.env]
SHELL = "/bin/shell"
TERM = "cluu"
PATH = "/sbin:/bin:/usr/sbin:/usr/bin"
LANG = "C"

[envelope.env_template]
HOME = "/home/{user}"
USER = "{user}"
LOGNAME = "{user}"
PWD = "/home/{user}"


[[envelope]]
name = "service"
# Stripped envelope for daemons spawned by procmgr/init at boot.
# No /home, no /tmp by default — services declare what they need
# in their own Cluufiles (which then narrow within this envelope).

mounts = [
    { path = "/",         mode = "ro" },
    { path = "/etc",      mode = "ro" },
    { path = "/lib",      mode = "ro" },
    { path = "/var/log",  mode = "rw" },
]

[envelope.env]
PATH = "/sbin:/bin"
TERM = "dumb"
LANG = "C"

# No env_template — services don't have a "user".
```

**Schema notes:**

- **Order matters in `mounts`** — more-specific paths must come after
  broader paths so longest-prefix-match resolves correctly. `/var/log
  rw` after `/ ro` means `/var/log` overrides root readonly. Procmgr
  does longest-prefix-match at view-construction.
- **`env` vs `env_template` are two tables**, not one with magic
  substitution-marker syntax. Avoids accidentally substituting `{user}`
  in keys that contain `{` for legitimate reasons.
- **No per-user override.** Per-user customization comes from
  users.toml's profile field selecting a different envelope.
- **`service` envelope has no `/home`.** Boot daemons (registry, vfs,
  virtio-blk, etc.) shouldn't see user homes. Their Cluufiles can
  declare narrower if needed.

## 6. Session-login resolution flow

```
Step 1. User authenticates (existing flow): login → users.toml lookup
        → matched [[user]] record { name, profile, password_hash, ... }
        → caller has the user's identity and profile string.

Step 2. Resolve envelope:
        envelope = lookup_envelope_by_name(user.profile)
        if envelope is None:
            login fails with "no envelope defined for profile X" (loud
            in serial log; user sees clean "login failed" on TTY).

Step 3. Substitute env_template:
        resolved_env = envelope.env  ∪
                       envelope.env_template.map(|k,v|
                           (k, v.replace("{user}", user.name)))

Step 4. Build spawn block for shell:
        - tokens[STDIN/STDOUT/STDERR/STDLOG] = the VT's tty endpoints
        - mount list = envelope.mounts (passed via VFS_SET_VIEW)
        - env block = resolved_env (packed into ProcessInfo page)
        - argv = ["/bin/shell"]
        - cwd = "/home/$USER" (from resolved_env's PWD)

Step 5. Spawn shell via existing container_run path with the
        constructed spawn block. Shell starts inside its envelope.
```

**Implementation points:**

1. **`/etc/envelopes.toml` parsing happens once at procmgr boot**,
   results cached in `ProcessManager`. Same eager-loading pattern as
   `users.toml`.

2. **Two new procmgr struct fields:**
   ```rust
   envelopes: BTreeMap<String, Envelope>,
   //  where:
   struct Envelope {
       mounts: Vec<MountSpec>,
       env: BTreeMap<String, String>,
       env_template: BTreeMap<String, String>,
   }
   struct MountSpec {
       path: String,
       mode: MountMode,  // Ro | Rw
   }
   ```

3. **Templated substitution is one-shot at login**, not lazy. Shell sees
   `HOME=/home/balazs` as a plain string — no `{user}` token surfaces
   in the env block.

4. **Env block routes through existing infrastructure**: `env_data` /
   `envc` fields in ProcessInfo are already plumbed end-to-end. Just
   needs the source of `env_data` to switch from "hardcoded defaults"
   to "resolved envelope".

5. **Mount list flows through VFS_SET_VIEW** (existing mount-policy
   infrastructure). The `VfsMount` struct gains a `writable: bool`
   field; VFS enforces RW vs RO at open time.

6. **Failure modes:**
   - Profile name not in envelopes.toml → login rejected with clear
     error.
   - envelopes.toml malformed at boot → procmgr panics with parse
     error before any spawns. Boot-time-fatal, like users.toml today.

7. **CWD inheritance.** `PWD = /home/{user}` becomes the shell's
   startup cwd. Already plumbed through PARAM_CWD_OFFSET/LEN.

## 7. Cluufile composition rules

**Rule 1 — Cluufile is the binary's specification of needs.**

```
MOUNT <path> <mode>     # mode = ro | rw | private
```

This declares: "this binary needs `<path>` in `<mode>`."

**Rule 2 — For each Cluufile MOUNT directive:**

1. Look up `<path>` in the parent's view (which already inherits from
   envelope).
2. If `<path>` not in parent view → **spawn fails** with
   `procmgr: cluufile mismatch: /bin/X requires /Y, parent does not
   provide`.
3. If `<path>` is in parent view but `<mode>` is more permissive
   (Cluufile asks `rw`, parent has `ro`) → **spawn fails** with
   `procmgr: cluufile mismatch: /bin/X requires /Y rw, parent has ro`.
4. If parent provides AT LEAST what Cluufile asks for → effective
   mount = Cluufile's `<mode>` (the narrower of the two).

**Rule 3 — Unmentioned paths inherit from parent.**

A Cluufile that mentions nothing inherits the full envelope unchanged.

**Rule 4 — `private` is special** (existing semantics):

`MOUNT /tmp private` creates a fresh empty MemFs at `/tmp` for this
binary, regardless of what the envelope provides. The binary cannot
SEE the envelope's `/tmp` — it has its own. This is a *replacement*,
not a narrowing. Always permitted (you're not asking for *more*).

**Three concrete examples:**

**Example 1 — `cat` runs fine under user envelope.**

`containers/cat/Cluufile`:
```
PROFILE ipc vfs
MOUNT /etc readonly
ENTRYPOINT /bin/cat
```

User envelope has `/etc ro`. Cluufile asks `ro`. Match. Cat sees `/etc
ro` plus everything else inherited from user envelope.

**Example 2 — `cat` runs fine under admin envelope (narrower than env).**

Admin envelope has `/etc rw`. Cat's Cluufile says `MOUNT /etc readonly`.
Rule 2 → cat gets RO at `/etc` (narrower wins). Cat sees admin's `/etc`
contents but cannot write — even when run by admin user. **Cluufile-as-
spec means cat's behavior is identical regardless of which envelope
launched it.** Portability.

**Example 3 — Hypothetical sysadmin tool fails under user envelope.**

`/bin/usermod` Cluufile:
```
PROFILE ipc vfs
MOUNT /etc readwrite
ENTRYPOINT /bin/usermod
```

User runs `usermod`: shell's view has `/etc ro`. Cluufile asks `rw`.
**Spawn fails** with `procmgr: cluufile mismatch: /bin/usermod requires
/etc rw, parent has ro`. Visible in serial log + clean error to user
TTY. `$? = 126`.

Admin runs `usermod`: shell's view has `/etc rw`. Match. Spawn
succeeds.

**Parser extensions:**

`MOUNT <path> readonly` and `MOUNT <path> ro` should both be accepted.
Same for `readwrite | rw`, `private | priv`. Today's parser already
accepts `inherit | private` keywords; extend with the new mode names.
Backward-compat: existing Cluufiles that say `MOUNT /tmp inherit` get
reinterpreted as "inherit envelope's /tmp at envelope's mode".

## 8. Shell behavior

Five mechanics, all in `userspace/shell/`:

### 8.1 — PATH-based bare-command resolution

When the user types `cat foo`:

1. Tokenize via cluu_lang as today.
2. First-word lookup order:
   - **Builtin?** Run as builtin (cd, export, true, false, test,
     repeat, ...). Done.
   - **Absolute path?** (`/bin/cat`) Send to procmgr's `container_run`
     directly with that name resolved to a container. Done.
   - **Bare name?** Walk `$PATH` left-to-right. For each `<dir>` in
     PATH, check if `<dir>/<name>` is a recognized container image.
     First hit wins; spawn it.
   - **Not found anywhere?** Print `shell: <name>: command not found`
     and `$? = 127`.

The `spawn` keyword stays as an explicit alias for "run this name as
a binary even if it shadows a builtin." Useful for testing.

### 8.2 — `export` semantics

Two distinct namespaces inside `CommandContext`:

```rust
struct CommandContext {
    vars: BTreeMap<String, String>,   // shell-local
    exported: BTreeSet<String>,       // names marked exported
    // ...
}
```

- `set FOO=bar` (or just `FOO=bar` at start of line) — sets in `vars`,
  NOT in `exported`. Visible to shell builtins (`echo $FOO`).
- `export FOO` (no value) — adds `FOO` to `exported` if it exists in
  `vars`.
- `export FOO=bar` — sets in `vars` AND adds to `exported`.
- `unset FOO` — removes from both.
- When spawning a child: env block = envelope's resolved env, with
  `vars ∩ exported` overlaid on top. **Shell's exported value wins on
  conflict** — user customization takes precedence over envelope
  defaults. The envelope provides the *initial* env at session-login;
  exported shell vars update it onward.

### 8.3 — `~/.shellrc` sourcing

At shell startup, after the initial env is established but BEFORE the
interactive REPL begins:

```
1. Try /etc/shellrc — if exists, read line by line, execute each line
   via the shell's existing executor. Failures print warnings to
   stderr; don't abort shell startup.

2. Try ~/.shellrc (where ~ = $HOME from envelope) — same flow.

3. Print prompt, enter REPL.
```

Both files are sequences of shell commands. Comments (`#`) and blank
lines skipped. No special syntax; full shell language available.

### 8.4 — Shell→newlib env mirror (one-way)

When the shell spawns a child, the env block (`vars ∩ exported`,
unioned with envelope) is packed into the spawn payload's existing env
trailer. The child's libcluu `_start` already decodes this into
newlib's `_environ`. **No new mechanism**; the shell just needs to
populate the payload from `vars ∩ exported`.

The shell itself ALSO mirrors its own `vars ∩ exported` into
`_environ` so shell builtins calling `getenv()` see the live shell
env. Mirror happens on every change (set/unset/export) — synchronous.

`setenv()` from a binary affects ONLY that binary's `_environ`, not
the shell's. Standard POSIX semantics.

### 8.5 — Special vars handled by shell itself

- `$?` — last command exit status (already implemented).
- `$$` — shell pid.
- `$0` — `/bin/shell`.
- `$#`, `$1..$9` — script args (when shell is sourcing, e.g.
  `~/.shellrc`).
- `$PWD` — auto-updated on `cd`. Mirrors to env (POSIX requirement).
- `$OLDPWD` — previous `$PWD`. `cd -` swaps them.

These are *implicit* — set by the shell, not by users. Match bash
semantics.

## 9. Error handling and edge cases

**Boot-time:**

- `/etc/envelopes.toml` missing or malformed → procmgr panics with
  clear message before any login.
- `/etc/users.toml` references envelope name not in envelopes.toml →
  procmgr logs warning at boot but doesn't panic. The mismatched user
  simply can't log in.
- No `[[envelope]] name = "user"` defined → only services and admin
  can log in. Logged at boot. Not fatal — admin can still recover.

**Login-time:**

- Profile lookup fails at session-login → login rejected. User sees
  clean "login failed" on TTY; serial log has root cause.
- `{user}` substitution edge cases — usernames are POSIX-restricted
  (alpha, digit, `_`, `-`); substitution is a literal `String::replace`
  with no shell-injection surface.

**Spawn-time (Cluufile vs envelope mismatch — strict):**

- Cluufile asks `MOUNT /etc rw`, envelope provides `/etc ro` → spawn
  fails with cluufile-mismatch error. Exit cookie code 126. Shell
  prints `shell: /bin/X: cannot execute (insufficient capabilities)`.
- Cluufile asks for path not in envelope at all → same failure,
  message identifies the missing path.
- Cluufile path malformed (relative path, `..`, etc.) → existing path
  validation rejects; preserve.

**Shell:**

- `/etc/shellrc` doesn't exist → silently skipped. Not an error.
- `~/.shellrc` doesn't exist → silently skipped. New users.
- Bad rc file syntax → shell logs warning to stderr (`shell:
  shellrc:line N: parse error`) and skips that line. Shell startup
  never aborts on bad rc files.
- `exit` in a sourced file exits the shell (POSIX behavior). Loops
  hang the shell; Ctrl-C breaks via existing SIGINT path.
- `HOME` env var not set (envelope misconfigured) → shell falls back
  to `/`. Functional but unfriendly. We rely on envelope correctness;
  not making the shell defensive.

**Bare-command resolution:**

- `PATH` empty or unset → bare commands fall through to "not found"
  immediately. Builtins still work. Absolute paths still work.
- `PATH` contains nonexistent dirs → silently skipped per directory.
- Builtin name shadowed by binary in PATH — builtin still wins.
  `spawn cd` or full path bypasses.

**Env mirror:**

- Shell `set FOO=bar` inside a sourced rc file — `vars` updated;
  mirrored to `_environ` immediately. If FOO not exported, child
  binaries don't see it. Standard.
- `export FOO` without value, FOO not in vars — POSIX says marks for
  export; if FOO is never set, no env entry added.
- Conflicting var name between exported and envelope.env (e.g., user
  `export PATH=...`) — exported wins. Shell's child gets user's PATH.

## 10. Testing

Harness cases (in `scripts/harness_cases.conf` style):

| Case | Verifies | Method |
|---|---|---|
| `l2_envelope_user` | User-profile login produces correct env block | spawn a probe printing `getenv("HOME")`, `getenv("USER")`, `getenv("PATH")` to debug_print; assert markers |
| `l2_envelope_admin` | Admin-profile login produces admin env (e.g., PATH includes `/sbin`) | same with admin user |
| `l2_envelope_mounts` | User envelope's `/etc ro` is enforced; binary fails to write | probe attempts `open("/etc/probe", O_WRONLY)` → assert errno=EACCES marker |
| `l2_cluufile_match` | Cluufile MOUNT consistent with envelope → spawn succeeds | spawn `/bin/cat /etc/motd` under user envelope; assert exit cookie 0 |
| `l2_cluufile_mismatch` | Cluufile demands more than envelope → spawn fails | spawn a probe whose Cluufile says `MOUNT /etc rw` under user envelope; assert "cluufile mismatch" log + exit cookie 126 |
| `l2_bare_cmd` | Bare-command lookup walks PATH | shell autostart `cat /etc/motd` (no `spawn` prefix); assert "Welcome to CLUU" marker |
| `l2_export` | `export FOO=bar` propagates to child; `set FOO=bar` (no export) doesn't | autostart `set X=local; export Y=exported; spawn envprobe`; envprobe asserts X unset, Y == "exported" |
| `l2_shellrc` | `~/.shellrc` sources at startup | seed `/home/balazs/.shellrc` with `export FOO=ricerocks`; autostart `spawn envprobe`; assert FOO=ricerocks |
| `l2_etc_shellrc` | `/etc/shellrc` before `~/.shellrc`; user wins on conflict | system rc: `export FOO=system`; user rc: `export FOO=user`; envprobe asserts FOO=user |
| `l2_mp_etc` | The original bug: mp can read `/etc/motd` once envelope grants /etc | spawn `mp -c "open('/etc/motd').read()"`; assert exit cookie 0 |

**Probes needed:**

- `envprobe` already exists per the conf file — extend to support
  arbitrary keys via argv (e.g., `spawn envprobe FOO HOME PATH`).
- New: tiny Cluufile-mismatch probe (Cluufile demands `/etc rw`) for
  `l2_cluufile_mismatch`. ~20 lines of C.

**Per-case work:**

- Most cases ride on existing harness machinery (autostart command +
  required_markers).
- `l2_etc_shellrc` and `l2_shellrc` require creating `/etc/shellrc` and
  `/home/$USER/.shellrc` files in the userdisk image at build time —
  extend `xtask`'s `[userdisk]` staging to copy them from
  `userspace/etc/` and a new `userspace/home/balazs/`.

**Regression coverage:**

- After spec lands, full `harness_matrix.sh` should stay green for
  everything except (already-pending) `l2_owner_deny`. The
  mount-policy change is broad enough that quick regression at PR
  review is essential.
- Add a `b_envelope_setup` micro-bench: time from "TSC calibrated" to
  "shell: ready". Should stay < +100 ms vs current baseline (9.3 s
  post-MAP_SHARE_PHYS).

## 11. Acceptance criteria

The work is "done" when all of the following hold:

1. `/etc/envelopes.toml` ships in the userdisk image with the three
   ship-as-default envelopes (admin, user, service).
2. Procmgr parses both `users.toml` and `envelopes.toml` at boot;
   panics on malformed envelopes.toml.
3. Session-login resolves `user.profile → envelope` and constructs the
   spawn block with mounts + env. Failed lookup → login rejected.
4. Shell sources `/etc/shellrc` then `~/.shellrc` at startup. Missing
   files silently skipped; syntax errors logged but don't abort.
5. Bare-command resolution walks `$PATH`. `cat /etc/motd` (no `spawn`
   prefix) works.
6. `export FOO=bar` makes `FOO` visible to spawned children;
   `set FOO=bar` does not.
7. Cluufile MOUNT directives matching the envelope succeed; mismatches
   fail spawn with clear error.
8. Shell's `vars ∩ exported` mirrors to newlib `_environ` for shell
   builtins to access via `getenv()`.
9. The original mp bug is closed: `spawn mp -c
   "open('/etc/motd').read()"` exits with code 0.
10. Full harness matrix stays green (pre-existing flakes excepted).

## 12. Out of scope (deferred to v1.x or later)

- **Per-user envelope override.** Per-user customization comes from
  selecting a different envelope. Per-user overrides on top of class
  envelopes is post-v1.
- **Configurable shellrc paths.** `CLUU_SHELLRC=...` env-var override
  deferred — one-line `getenv` later if needed.
- **Bidirectional env mirror.** C-side `setenv` not propagating back
  to the shell is correct POSIX.
- **`mounts_private` and `mounts_deny` modes** in envelopes.toml.
  Inline-table schema supports adding the field later without
  breakage. Defer until concrete need.
- **Aliases and functions.** Bash has `alias ll='ls -la'` and shell
  functions. cluu_lang grammar would need extensions.
- **`PS1` / `PS2` prompt customization.** Today's prompt is hardcoded
  `${USER}@cluu>`. Defer until someone wants ricer prompts.
- **`env -i`** and **`env FOO=bar cmd`**. Shell ergonomics, deferred.
- **TOML parser hardening.** v1 ships with a basic parser. Future
  hardening: validate at xtask build-time so malformed files are
  caught before deployment.
- **The mp debug instrumentation in `userspace/micropython/main.c`.**
  Currently uncommitted. Once envelope work confirms the fix, revert.

## 13. References

- `userspace/procmgr/src/main.rs` — session-login, ProcessManager state.
- `userspace/procmgr/src/mount_policy.rs` — existing mount policy
  module; will be extended with mode (rw/ro) and integrated with
  envelope resolution.
- `userspace/shell/src/commands.rs` — CommandContext, builtin
  registry, executor.
- `userspace/shell/src/main.rs` — shell startup, prompt loop.
- `userspace/libcluu/src/posix/` — newlib `_environ` decoder, env
  helpers.
- `etc/users.toml` — existing user records; extends with envelope
  resolution.
- `etc/envelopes.toml` — NEW file shipped with this spec.
- `~/cluu-notes/CURRENT_PHASE.md` — Phase 2 entry plan; update on
  spec acceptance.
- `~/.claude/projects/-home-vlb2bp-git-cluu/memory/project_micropython_diagnostic.md`
  — the bug investigation that surfaced this spec.
