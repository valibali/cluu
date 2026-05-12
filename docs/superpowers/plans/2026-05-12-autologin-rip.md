# Autologin Rip — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make a normal boot land on the interactive `login:` prompt on every text VT (VT0..VT3), not on an auto-spawned root shell. Preserve the existing test harness path so all `l2_*` markers that depend on `CLUU_SHELL_AUTOSTART_CMD` continue to work without per-test changes.

**Architecture:** Gate the autologin path on the existing `SHELL_AUTOSTART_CMD` build-time constant being non-empty.

- In `procmgr`, `try_auto_login` becomes a no-op when `SHELL_AUTOSTART_CMD.is_empty()`.
- In `tty`, `auto_login_pending` is set true only when an autostart command is also baked into the build (mirrors procmgr's gate). The signal is plumbed across the crate boundary by introducing one shared constant in `libcluu` so both crates read the same value.
- No other behaviour changes. The text-mode interactive login that already lives in `tty/src/context.rs` becomes the default user-facing entry point on every text VT.
- §4.7 of the parent spec (separate `getty` binary) is **subsumed** by this plan: `tty` itself already implements username/password prompt + `PROCMGR_SESSION_LOGIN_LABEL`. Banner + per-VT instance plumbing land in a follow-up plan.

**Tech Stack:** Rust (procmgr, tty, libcluu), CLUU harness (`scripts/harness_run.sh`).

**Parent spec:** `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.2 (and partial §4.7).

**Prior plan:** `docs/superpowers/plans/2026-05-12-vtmgr-boot-vt-fix.md` (commits 72006d6..7dee34e + e52c852 doc).

---

## Task 1: Add the autologin invariant marker

Before changing behaviour, make the existing state observable. The procmgr autologin path already logs `procmgr: auto-login root on VT:0`. We add a complementary "no autologin (env empty)" marker so we can tell, from the serial log alone, which path the build took.

**Files:**
- Modify: `userspace/procmgr/src/main.rs` (`try_auto_login`, ~lines 1026-1032).

- [ ] **Step 1: Add gate-noop marker**

Insert at the top of `try_auto_login`, immediately AFTER the existing `if self.auto_login_done { return; }` line and BEFORE the `if self.user_records.is_empty() { return; }`:

```rust
        if SHELL_AUTOSTART_CMD.is_empty() {
            // Gate diagnostic: production builds (no CLUU_SHELL_AUTOSTART_CMD)
            // never auto-login; harness builds bake a command and do.
            let _ = debug_print("procmgr: autologin skipped (no autostart cmd)");
            self.auto_login_done = true;
            return;
        }
```

This is a pure observability-and-gate change in one step. After Task 1 lands:
- Production build (env unset): marker `procmgr: autologin skipped (no autostart cmd)` appears once at boot; no `procmgr: auto-login root on VT:0` follows.
- Harness build (env set): the new line never fires; the existing `procmgr: auto-login root on VT:0` continues to fire.

- [ ] **Step 2: Build and run harness (autostart path)**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
grep -aE "procmgr: auto-login root|procmgr: autologin skipped" /tmp/cluu-serial-com2.log
```

Expected: `procmgr: auto-login root on VT:0` present (harness sets CLUU_SHELL_AUTOSTART_CMD via harness_case_defaults.sh). The "skipped" line MUST NOT appear.

- [ ] **Step 3: Build and run harness (no-autostart path)**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "procmgr: auto-login root|procmgr: autologin skipped" /tmp/cluu-serial-com2.log
```

`l2_vt4_default` does not set `SHELL_AUTOSTART_CMD_DEFAULT`. Expected: `procmgr: autologin skipped (no autostart cmd)` present; `procmgr: auto-login root` MUST NOT appear.

If neither marker mode behaves as expected, STOP — the harness env may set CLUU_SHELL_AUTOSTART_CMD globally. Check `scripts/harness_case_defaults.sh` for whether `SHELL_AUTOSTART_CMD_DEFAULT` is unset in `l2_vt4_default`. If it IS set, pick another no-autostart marker mode (e.g. `MARKER_MODE=none` with no env).

- [ ] **Step 4: Commit**

```bash
git add userspace/procmgr/src/main.rs
git commit -m "procmgr: gate try_auto_login on SHELL_AUTOSTART_CMD non-empty"
```

---

## Task 2: Verify tty shows login prompt when procmgr does not autologin

`tty/src/context.rs` already prints `tty:N: showing login prompt` when `auto_login_pending == false` and console is available. With Task 1 in place, this should fire on VT0 in no-autostart builds. This task is verification only — no code change unless the marker is missing.

**Files:** none (verification only).

- [ ] **Step 1: Confirm the marker fires on VT0 in no-autostart build**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "tty:0: showing login prompt|tty:0:.*auto-login wired" /tmp/cluu-serial-com2.log
```

Expected:
- `tty:0: showing login prompt` PRESENT.
- `tty:0: ... auto-login wired ...` MUST NOT appear.

Why it works even without Task 3: `auto_login_pending` is set true at construction (line 127), but `try_auto_login` is now a no-op, so procmgr never sends the `TTY_REGISTER` that calls `wire_shell_stdin`. The defensive flag never gets cleared, so `maybe_show_login_prompt` returns at the `if self.auto_login_pending` check (line 181) and the prompt does NOT appear yet. The prompt only fires once something flips the flag.

If the marker is missing as predicted, proceed to Task 3 to fix it. Do not commit anything in this task — it is observation only.

- [ ] **Step 2: Record finding**

Either confirm marker is missing on VT0 (expected — Task 3 fixes it) or report unexpected presence (Task 3 might not be needed). No commit.

---

## Task 3: Gate `auto_login_pending` on the same constant

Make `tty` agree with `procmgr`: when no autostart is baked into the build, `auto_login_pending` is false on every VT, including VT0.

The cleanest way is to introduce a single shared constant in `libcluu` and have both crates read it. This keeps the two halves in lockstep without duplicating the `option_env!` macro.

**Files:**
- Modify: `userspace/libcluu/src/lib.rs` (or wherever a build-time constants module lives — see Step 0 below).
- Modify: `userspace/procmgr/src/main.rs:193-196` (replace local constant with import).
- Modify: `userspace/tty/src/context.rs:127` (use shared constant in the `instance_id == 0 &&` guard).
- Modify: `userspace/tty/Cargo.toml` (no change expected — `libcluu` is already a dep).

- [ ] **Step 0: Pick the right home for the shared constant**

Run:

```bash
grep -n "option_env\|build.rs" userspace/libcluu/src/lib.rs userspace/libcluu/src/*.rs 2>/dev/null | head
ls userspace/libcluu/src/ | head
```

If a `boot.rs`, `runtime.rs`, or similar module already holds build-time wiring, add the constant there. Otherwise, create `userspace/libcluu/src/build_env.rs` with:

```rust
//! Build-time constants threaded from the build env into runtime code.
//!
//! Single source of truth for env-driven knobs so multiple crates do not
//! drift apart. Currently only the shell-autostart command, which gates
//! procmgr's auto-login path and tty's wait-for-autologin flag.

pub const SHELL_AUTOSTART_CMD: &str = match option_env!("CLUU_SHELL_AUTOSTART_CMD") {
    Some(cmd) => cmd,
    None => "",
};

pub const HARNESS_AUTOLOGIN_ARMED: bool = !SHELL_AUTOSTART_CMD.is_empty();
```

Also: add `userspace/libcluu/build.rs` (or extend the existing one — check first) with:

```rust
println!("cargo:rerun-if-env-changed=CLUU_SHELL_AUTOSTART_CMD");
```

This ensures cargo invalidates the libcluu rlib when the env var changes, so the constants are not stale.

Re-export from `userspace/libcluu/src/lib.rs`:

```rust
pub mod build_env;
```

- [ ] **Step 1: Switch procmgr to the shared constant**

In `userspace/procmgr/src/main.rs`, replace the local constant (lines ~193-196):

```rust
const SHELL_AUTOSTART_CMD: &str = match option_env!("CLUU_SHELL_AUTOSTART_CMD") {
    Some(cmd) => cmd,
    None => "",
};
```

with a re-export:

```rust
use libcluu::build_env::SHELL_AUTOSTART_CMD;
```

The build.rs `rerun-if-env-changed` line at `userspace/procmgr/build.rs:2` should stay — duplicate rerun-if doesn't hurt, and procmgr's own crate still depends on the env at link time via the imported constant. (Verify the libcluu rebuild also picks up env changes via Step 0's build.rs add.)

- [ ] **Step 2: Gate tty's `auto_login_pending`**

In `userspace/tty/src/context.rs` line 127:

```rust
            auto_login_pending: instance_id == 0 && libcluu::build_env::HARNESS_AUTOLOGIN_ARMED,
```

This is one identifier change. No new imports needed because `libcluu` is already imported throughout the file (see existing `libcluu::ipc::...` uses).

- [ ] **Step 3: Build and verify**

```bash
cargo xtask build 2>&1 | tail -20
```

Expected: no errors. If procmgr or tty fails to find `libcluu::build_env`, the re-export in lib.rs is missing.

- [ ] **Step 4: Harness verify no-autostart path**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh
grep -aE "tty:0: showing login prompt|tty:0:.*auto-login|procmgr: autologin skipped" /tmp/cluu-serial-com2.log
```

Expected:
- `procmgr: autologin skipped (no autostart cmd)` PRESENT.
- `tty:0: showing login prompt` PRESENT.
- `tty:0: ... auto-login wired ...` ABSENT.

- [ ] **Step 5: Harness verify autostart path still works**

```bash
HARNESS_FORCE_BUILD=1 MARKER_MODE=l2_path_symlink_resolve bash scripts/harness_run.sh
grep -aE "procmgr: auto-login root|tty:0:.*auto-login wired|procmgr: autologin skipped" /tmp/cluu-serial-com2.log
```

Expected:
- `procmgr: auto-login root on VT:0` PRESENT (harness sets autostart).
- `tty:0: ... auto-login wired ...` PRESENT.
- `procmgr: autologin skipped` ABSENT.
- The marker `l2_path_symlink_resolve` requires shows up at end (existing acceptance for that case).

- [ ] **Step 6: Commit**

```bash
git add userspace/libcluu/src/lib.rs userspace/libcluu/src/build_env.rs \
        userspace/libcluu/build.rs userspace/procmgr/src/main.rs \
        userspace/tty/src/context.rs
git commit -m "libcluu/procmgr/tty: gate autologin on SHELL_AUTOSTART_CMD constant"
```

(Adjust `git add` list if Step 0 placed the constant in an existing file rather than a new `build_env.rs`.)

---

## Task 4: Rip the now-dead pieces

With Task 3 in place, `wire_shell_stdin` and `handle_session_death` still touch `auto_login_pending`. The field is now build-conditional but never re-set after init. These call sites are harmless (they assign `false` to an already-`false` field in production builds, and behave correctly in harness builds). Leave them alone — they are part of the harness path.

What IS now dead and should be removed: nothing in this task. Task 4 exists in the plan slot as a placeholder for any cleanup found in code review. If the spec reviewer or code reviewer points out something concrete (e.g. an unused argument in `try_auto_login`), do it here. Otherwise SKIP this task and move directly to Task 5.

- [ ] **Step 1: Review-driven cleanup or skip**

Inspect `git diff 7dee34e..HEAD -- userspace/` and decide if any helpers got orphaned. If yes, remove and commit:

```bash
git add <files>
git commit -m "procmgr/tty: drop dead helpers after autologin gate"
```

If nothing is orphaned, document the skip:

```bash
# (no-op task)
```

---

## Task 5: Visual smoke + spec status

Confirm a no-autostart boot lands on `login:` on VT0 (not on a shell prompt) and that the compositor is still visible on VT4.

**Files:** `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.2 (and §4.7 partial note).

- [ ] **Step 1: Boot, dump fb, confirm**

Reuse the workflow recorded in `reference_fb_dump_smoke_workflow.md` (memory). In short:

```bash
HARNESS_FORCE_BUILD=0 MARKER_MODE=l2_vt4_default bash scripts/harness_run.sh \
  > /tmp/harness.out 2>&1 &
HARNESS_PID=$!
for _ in $(seq 1 60); do
  grep -q "compositor: ready" /tmp/cluu-serial-com2.log 2>/dev/null && break
  sleep 0.5
done
FB_PHYS=$(grep -oE 'fb @[0-9A-Fa-f]+' /tmp/cluu-serial-com2.log | head -1 | sed 's/fb @/0x/')
bash scripts/fb_dump.sh -p "$FB_PHYS" -o /tmp/autologin-rip-vt4
# Ctrl-Alt-F1 via monitor sendkey, capture VT0:
echo "sendkey ctrl-alt-f1" | socat - "UNIX-CONNECT:/tmp/cluu-qemu-monitor.sock"
sleep 1
bash scripts/fb_dump.sh -p "$FB_PHYS" -o /tmp/autologin-rip-vt0
wait "$HARNESS_PID" || true
```

Pass criteria:
- `/tmp/autologin-rip-vt4.png` — compositor visible (chrome / cluuterm-window-frame style content).
- `/tmp/autologin-rip-vt0.png` — text console showing `login:` prompt.
- No shell prompt on VT0.

If VT0 still shows a `$ ` shell, Task 3 step 2 didn't take — investigate.

- [ ] **Step 2: Update spec status**

Edit `docs/superpowers/specs/2026-05-12-login-flow-design.md` §4.2. Immediately after the heading line, insert:

```
**Status:** done in plan 2026-05-12-autologin-rip (commits <SHA1>..<SHA2>). Visual smoke 2026-05-12 confirmed VT0 shows login prompt; harness path preserved via SHELL_AUTOSTART_CMD gate.
```

Replace `<SHA1>..<SHA2>` with the Task 1 / Task 3 SHAs (and Task 4 if not skipped).

Also add a note to §4.7 (`getty — VT0–VT3 raw console`) header:

```
**Status:** partially done (interactive login on VT0..VT3 already provided by tty's own login mode, since plan 2026-05-12-autologin-rip). Remaining: sysinfo banner + dedicated getty binary if needed.
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/specs/2026-05-12-login-flow-design.md
git commit -m "docs/spec: autologin rip marked done; §4.7 partially subsumed"
```

---

## Self-review notes

- Touches: `userspace/procmgr/src/main.rs`, `userspace/tty/src/context.rs`, `userspace/libcluu/{src/lib.rs, src/build_env.rs (new), build.rs}`. Plus one spec edit. No kernel, no other crates.
- Two-axis test: harness (autostart set) keeps the old path; production (autostart unset) gets interactive login. Both are exercised at Tasks 1 step 2/3 and Task 3 step 4/5.
- `auto_login_pending` is left in the code path because the harness path still depends on it. Eventually (after getty plan + harness migration), the field can vanish entirely. Not this plan.
- Spec §4.7 is downgraded to "partial" because tty itself is already a getty.
- All commits land on `develop`, no force-push, no `--no-verify`, no `--amend`.
