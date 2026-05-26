# Autologin Removal + Harness Migration Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Delete the `try_auto_login` shortcut in root-procmgr and migrate every `l2_*` harness case from `SHELL_AUTOSTART_CMD_DEFAULT` to the interactive login flow (compositor → login → cluuterm → shell), driving credentials and test commands through QEMU sendkey.

**Architecture:** Compositor already spawns `/bin/login` at boot. The login binary already accepts keystrokes via the compositor input forwarding endpoint. After credentials, login spawns cluuterm → shell. The harness already supports both `SENDKEY_SEQUENCE_DEFAULT` (for credentials, fires unconditionally) and `TYPED_COMMANDS` (fires after `[USER] shell: ready` marker). This plan replaces the broken `SHELL_AUTOSTART_CMD` path with `SENDKEY_SEQUENCE_DEFAULT` for `root\nroot\n` + `TEST_COMMAND` for the case-specific payload, then deletes the dead procmgr code.

**Tech Stack:** Bash (harness), Rust (root-procmgr deletion), libcluu build script.

**Spec reference:** Memory `project_autostart_shell_fd0_fatal` (2026-05-18) documented the FATAL. Memory `feedback_path_a_stdio_assertion` forbids stdin fallback hacks; the correct fix is removing the bypass entirely.

**Prereq:** Existing harness primitives for sendkey + TYPED_COMMANDS already work (l2_login, l2_cluuterm_login validated).

---

## File structure

### Modified
- `scripts/harness_case_defaults.sh` — replace `SHELL_AUTOSTART_CMD_DEFAULT=...` with `TEST_COMMAND=...` + credentials helper in every case currently using autostart
- `scripts/harness_run.sh` — remove `CLUU_SHELL_AUTOSTART_CMD` export logic
- `scripts/harness_suite.sh` — remove `CLUU_SHELL_AUTOSTART_CMD` plumbing
- `userspace/root-procmgr/src/main.rs` — delete `try_auto_login()`, `auto_login_done` field, call sites at 2050/2064, import at 206
- `userspace/root-procmgr/build.rs` — drop `rerun-if-env-changed=CLUU_SHELL_AUTOSTART_CMD`
- `userspace/libcluu/build.rs` — drop `rerun-if-env-changed=CLUU_SHELL_AUTOSTART_CMD`
- `userspace/libcluu/src/build_env.rs` — delete `SHELL_AUTOSTART_CMD` + `HARNESS_AUTOLOGIN_ARMED` consts

### Created
- `scripts/harness_case_defaults.sh` gains a `CREDS_SENDKEY_ROOT` helper variable that holds the standard `sleep 5; root ret; sleep 2; root ret` sequence (extracted to avoid duplication across ~50 cases)

---

## Stage 1 — Add credentials helper + migrate l2_exit_status PoC

### Task 1.1: Add CREDS_SENDKEY_ROOT helper

**Files:**
- Modify: `scripts/harness_case_defaults.sh:13` (top of `set_case_defaults()` function, before the `case` block)

- [ ] **Step 1: Insert helper above the case statement**

Add right after the initial `SHELL_AUTOSTART_CMD_DEFAULT=""` reset (around line 16). The exact insertion point is between the reset block and the `case "$case_name" in` line. Look for the existing reset block and add:

```bash
    # Standard root/root credentials sendkey sequence for cases that drive
    # the interactive login flow. Each case that uses this MUST also set
    # SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1" and RUN_WAIT_DEFAULT to at least 45.
    CREDS_SENDKEY_ROOT=$'sleep 5\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret\nsleep 2\nsendkey r\nsendkey o\nsendkey o\nsendkey t\nsendkey ret'
```

### Task 1.2: Migrate l2_exit_status

**Files:**
- Modify: `scripts/harness_case_defaults.sh:168-171` (the `l2_exit_status)` block)

- [ ] **Step 1: Replace the case block**

Before:
```bash
            l2_exit_status)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="false ; echo \$?"
                ;;
```

After:
```bash
            l2_exit_status)
                TEST_COMMAND="false ; echo \$?"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
```

### Task 1.3: Run l2_exit_status PoC

- [ ] **Step 1: Build + run**

```bash
HARNESS_FORCE_BUILD=1 bash scripts/harness_run.sh l2_exit_status 2>&1 | tail -40
```

Expected: marker `[USER] shell: ready` observed; TYPED_COMMANDS sequence sends `false ; echo $?`; serial log contains `1`.

- [ ] **Step 2: Retry once if vt/manifest flake (per feedback_harness_usage_for_subagents)**

```bash
bash scripts/harness_run.sh l2_exit_status 2>&1 | tail -40
```

- [ ] **Step 3: Confirm `1` appears in the post-prompt output**

```bash
grep -E '^\s*1\s*$|\] 1$' /tmp/cluu-serial.log | head -5
```

### Task 1.4: Commit Stage 1

```bash
git add scripts/harness_case_defaults.sh
git commit -m "$(cat <<'EOF'
test(harness): migrate l2_exit_status to interactive login flow

Adds CREDS_SENDKEY_ROOT helper for root/root credential injection via
QEMU sendkey. l2_exit_status now drives the case through compositor →
login → cluuterm → shell, then types the test command via TYPED_COMMANDS.

First case migrated off the broken SHELL_AUTOSTART_CMD autostart path
(per project_autostart_shell_fd0_fatal). Remaining cases follow.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 2 — Migrate remaining l2_* cases batch-by-batch

### Task 2.1: Inventory + batch grouping

**Files:**
- Read: `scripts/harness_case_defaults.sh`

- [ ] **Step 1: Enumerate cases**

```bash
grep -nE 'SHELL_AUTOSTART_CMD_DEFAULT="' scripts/harness_case_defaults.sh \
  | grep -v '^[0-9]*:#' \
  | grep -v 'SHELL_AUTOSTART_CMD_DEFAULT=""'
```

Expected: ~50 lines, each `<lineno>: SHELL_AUTOSTART_CMD_DEFAULT="..."` under an `l2_*)` block.

- [ ] **Step 2: Group into batches of ~10 cases for separate commits**

Suggested grouping (mechanical):
- Batch A: probe-based cases (l2_argv, l2_vqprobe, l2_blk_*, l2_owner_deny, l2_cluufile_*, l2_envelope_user, l2_envelope_home_propagated, l2_mp_etc, l2_export)
- Batch B: file/dir builtins (l2_cd, l2_cd_inherit, l2_cp, l2_mv, l2_mkdir, l2_rm, l2_redir_stdout_file, l2_mount_private)
- Batch C: list/text utils (l2_ls, l2_ls_long, l2_ls_color, l2_ls_recursive, l2_cat_basic, l2_cp_recursive, l2_head_bytes, l2_wc_lines, l2_grep_recursive)
- Batch D: misc utils (l2_basename_basic, l2_dirname_basic, l2_sleep_basic, l2_which_basic, l2_printf_basic, l2_date_basic, l2_env_basic, l2_kill_basic, l2_sort_basic, l2_uniq_basic, l2_cut_basic, l2_tr_basic, l2_find_basic, l2_du_basic, l2_stat_basic)
- Batch E: editor (l2_edit_smoke, l2_edit_insert, l2_edit_undo, l2_edit_eacces)
- Batch F: jobs (l2_fg, l2_jobchurn, l2_jobmix, l2_jobs, l2_stop, l2_sigint, l2_waitpid)
- Batch G: pipes (l2_pipe_builtin, l2_pipe_builtin_chain, l2_pipe_basic, l2_pipe_env, l2_poll_pipes, l2_pipe_three)
- Batch H: misc (l2_argv, l2_bare_cmd, l2_shellrc, l2_tab_complete, l2_envelope_mounts, l2_alias_basic, l2_type_basic, l2_help_basic, l2_sleepy)

(If a case appears in two batches above, drop the duplicate — the actual list lives in the file.)

### Task 2.2 — 2.9: Per-batch migration

For EACH batch above, repeat:

- [ ] **Step 1: For each case in the batch, apply the same transform as Task 1.2**

Pattern transform for each case block (mechanical, regex-friendly but DO NOT auto-regex — eyeball each case because some have additional state):

Before:
```bash
            l2_FOO)
                TEST_COMMAND=""
                SHELL_AUTOSTART_CMD_DEFAULT="<cmd>"
                ;;
```

After:
```bash
            l2_FOO)
                TEST_COMMAND="<cmd>"
                SENDKEY_SEQUENCE_NOWAIT_DEFAULT="1"
                RUN_WAIT_DEFAULT="45"
                SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"
                ;;
```

**Special handling rules:**
- If the original case has `TEST_COMMAND="<x>"` non-empty AND `SHELL_AUTOSTART_CMD_DEFAULT="<y>"`, merge to `TEST_COMMAND="<y> ; <x>"` — autostart ran first, then the prompt typed `TEST_COMMAND`. Preserve order.
- If the case has `RUN_WAIT_DEFAULT` already set higher than 45, keep the higher value.
- If the case has other custom env (e.g., `MARKER_MODE=...`), preserve untouched.
- If the case has `POST_SENDKEY_DEFAULT` set, append it after the credentials helper: `SENDKEY_SEQUENCE_DEFAULT="$CREDS_SENDKEY_ROOT"$'\n'"$POST_SENDKEY_DEFAULT"` (or merge inline).

- [ ] **Step 2: Run each batch's cases**

```bash
for c in l2_CASE_A l2_CASE_B l2_CASE_C ...; do
  echo "=== $c ==="
  bash scripts/harness_run.sh "$c" 2>&1 | tail -5
done
```

Expected: each prints PASS / its required markers seen. Retry vt/manifest flake once.

- [ ] **Step 3: Commit batch**

```bash
git add scripts/harness_case_defaults.sh
git commit -m "test(harness): migrate batch <LETTER> cases to interactive login flow

Batch <LETTER>: <list-of-cases>

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

### Task 2.10: Verify nothing still uses SHELL_AUTOSTART_CMD_DEFAULT

- [ ] **Step 1: Audit**

```bash
grep -nE 'SHELL_AUTOSTART_CMD_DEFAULT="[^"]' scripts/harness_case_defaults.sh
```

Expected: only the empty-string reset at the top (line 16). If any case still has a non-empty value, it was missed — go back and migrate it.

---

## Stage 3 — Delete try_auto_login from root-procmgr

### Task 3.1: Delete try_auto_login + auto_login_done + call sites

**Files:**
- Modify: `userspace/root-procmgr/src/main.rs`

- [ ] **Step 1: Remove the import**

Delete line 206:
```rust
use libcluu::build_env::SHELL_AUTOSTART_CMD;
```

- [ ] **Step 2: Remove the field**

Delete line 297 (`auto_login_done: bool,`) from the procmgr state struct. Look for the surrounding struct definition; delete only this field.

- [ ] **Step 3: Remove the initializer**

Delete line 384 (`auto_login_done: false,`) from the corresponding `Self { ... }` constructor.

- [ ] **Step 4: Remove the function**

Delete lines 1230-1327 inclusive (the entire `fn try_auto_login(&mut self) { ... }`).

- [ ] **Step 5: Remove the call sites**

Delete the two call lines at 2050 and 2064. Each is a single `self.try_auto_login();` line — leave surrounding code intact.

- [ ] **Step 6: Verify compile**

```bash
cargo xtask build 2>&1 | tail -30
```

Expected: clean build (only warnings, no errors). If `build_shell_argv_payload` becomes unused, delete it too — grep for callers first.

- [ ] **Step 7: Commit Stage 3**

```bash
git add userspace/root-procmgr/src/main.rs
git commit -m "$(cat <<'EOF'
refactor(procmgr): delete try_auto_login dead path

All harness cases now drive the interactive login flow via QEMU sendkey
(prior commits). The auto-login shortcut bypassed session-procmgr and
left fd 0 non-VFS, producing the long-standing FATAL documented in
project_autostart_shell_fd0_fatal.

Removes:
- try_auto_login() function
- auto_login_done state field + initializer
- two call sites
- SHELL_AUTOSTART_CMD import

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 4 — Drop SHELL_AUTOSTART_CMD from libcluu

### Task 4.1: Delete libcluu build_env consts

**Files:**
- Modify: `userspace/libcluu/src/build_env.rs`

- [ ] **Step 1: Delete the consts**

Remove the entire block defining `SHELL_AUTOSTART_CMD` (line 7-9 area) and `HARNESS_AUTOLOGIN_ARMED` (line 12). Inspect the file first to identify exact line range; the two consts plus any explanatory comment block above them go away.

- [ ] **Step 2: Audit for any other users**

```bash
grep -rn 'HARNESS_AUTOLOGIN_ARMED\|SHELL_AUTOSTART_CMD' userspace/ --include='*.rs'
```

Expected: no hits. If anything else imports either const, remove that usage. (Root-procmgr was the only user as of investigation; recheck.)

- [ ] **Step 3: Drop build.rs env-change tracking**

In `userspace/libcluu/build.rs` line 2, delete:
```rust
println!("cargo:rerun-if-env-changed=CLUU_SHELL_AUTOSTART_CMD");
```

In `userspace/root-procmgr/build.rs` line 2, delete the same line.

- [ ] **Step 4: Verify compile**

```bash
cargo xtask build 2>&1 | tail -20
```

- [ ] **Step 5: Commit Stage 4**

```bash
git add userspace/libcluu/src/build_env.rs userspace/libcluu/build.rs userspace/root-procmgr/build.rs
git commit -m "refactor(libcluu): drop SHELL_AUTOSTART_CMD + HARNESS_AUTOLOGIN_ARMED

Both consts were consumed exclusively by try_auto_login (now deleted).

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>"
```

---

## Stage 5 — Strip CLUU_SHELL_AUTOSTART_CMD plumbing from harness scripts

### Task 5.1: Clean harness_run.sh

**Files:**
- Modify: `scripts/harness_run.sh`

- [ ] **Step 1: Delete the export block at lines 96-103**

Before (approximate):
```bash
if [ -z "${CLUU_SHELL_AUTOSTART_CMD:-}" ]; then
    if [ -n "$SHELL_AUTOSTART_CMD_DEFAULT" ]; then
        export CLUU_SHELL_AUTOSTART_CMD="$SHELL_AUTOSTART_CMD_DEFAULT"
    elif [ -n "$HARNESS_AUTOEXEC_CMD" ]; then
        export CLUU_SHELL_AUTOSTART_CMD="$HARNESS_AUTOEXEC_CMD"
    else
        export CLUU_SHELL_AUTOSTART_CMD=""
    fi
fi
```

After: delete entire block.

### Task 5.2: Clean harness_suite.sh

**Files:**
- Modify: `scripts/harness_suite.sh`

- [ ] **Step 1: Delete CLUU_SHELL_AUTOSTART_CMD references**

Lines 65-110 area contain effective-autostart computation. Remove every CLUU_SHELL_AUTOSTART_CMD and SHELL_AUTOSTART_CMD_DEFAULT reference — both unset calls and the `effective_autostart` variable that computes the value. Inspect carefully; preserve unrelated logic in the same lines.

### Task 5.3: Clean harness_case_defaults.sh comments

**Files:**
- Modify: `scripts/harness_case_defaults.sh`

- [ ] **Step 1: Delete the now-misleading header comments + reset**

Update header comments (lines 3-12 area) that mention `SHELL_AUTOSTART_CMD_DEFAULT`. Delete the empty reset `SHELL_AUTOSTART_CMD_DEFAULT=""` (line 16). Keep `CREDS_SENDKEY_ROOT` and the rest of the function body.

### Task 5.4: Verify

- [ ] **Step 1: Build + run one case to make sure nothing references the removed plumbing**

```bash
HARNESS_FORCE_BUILD=1 bash scripts/harness_run.sh l2_exit_status 2>&1 | tail -20
```

Expected: marker `[USER] shell: ready` + `1` in output.

- [ ] **Step 2: Audit for dangling references**

```bash
grep -rnE 'SHELL_AUTOSTART_CMD|HARNESS_AUTOLOGIN_ARMED|CLUU_SHELL_AUTOSTART_CMD' \
  userspace/ scripts/ 2>/dev/null
```

Expected: empty.

### Task 5.5: Commit Stage 5

```bash
git add scripts/harness_run.sh scripts/harness_suite.sh scripts/harness_case_defaults.sh
git commit -m "$(cat <<'EOF'
refactor(harness): drop CLUU_SHELL_AUTOSTART_CMD plumbing

All cases now drive the interactive login flow. The autostart env-var
chain (CLUU_SHELL_AUTOSTART_CMD ← SHELL_AUTOSTART_CMD_DEFAULT ←
HARNESS_AUTOEXEC_CMD) is unreachable and removed.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 6 — Full matrix verification

### Task 6.1: Run full matrix

- [ ] **Step 1: Execute**

```bash
HARNESS_FORCE_BUILD=1 bash scripts/harness_matrix.sh 2>&1 | tee /tmp/matrix-after-autologin-removal.log
```

- [ ] **Step 2: Count results**

```bash
grep -cE 'PASS|FAIL' /tmp/matrix-after-autologin-removal.log
grep -E 'FAIL' /tmp/matrix-after-autologin-removal.log
```

Expected: every l2_* shell case passes. If any fail, debug per-case:
1. Did credentials sendkey fire? Look for `[USER] login:` then `[USER] shell: ready`.
2. Did TYPED_COMMANDS fire? Look for echo of the typed command.
3. Did the expected marker appear?

Per-case timing knobs: bump `RUN_WAIT_DEFAULT` per case if needed, but >45s should be rare.

### Task 6.2: Commit matrix baseline

```bash
git add scripts/perf_ratchet.json 2>/dev/null || true
# If anything else needs adjusting, surface here.
git commit --allow-empty -m "$(cat <<'EOF'
ci(harness): matrix green post-autologin removal

Sanity commit after the autologin-removal series. Matrix run logged
to /tmp/matrix-after-autologin-removal.log.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Self-review

- **Spec coverage**: every `SHELL_AUTOSTART_CMD_DEFAULT` reference has a migration step (Stage 2). All procmgr autologin code is named for deletion (Stage 3). All build-script + libcluu consts named (Stage 4). All harness plumbing named (Stage 5). Final matrix gate (Stage 6).
- **Placeholders**: none. Every transform spells out the before/after. Batch grouping in Task 2.1 names cases; "and similar" never used.
- **Type consistency**: `CREDS_SENDKEY_ROOT`, `SENDKEY_SEQUENCE_DEFAULT`, `SENDKEY_SEQUENCE_NOWAIT_DEFAULT`, `RUN_WAIT_DEFAULT`, `TEST_COMMAND` — all match harness_run.sh variable names. `try_auto_login`, `auto_login_done`, `SHELL_AUTOSTART_CMD`, `HARNESS_AUTOLOGIN_ARMED` — all match grep results.
- **Risk**: Stage 2 is ~50 case edits; mechanical but bulky. Subagent-driven execution recommended, one batch per subagent call.

---

## Acceptance

Plan done when:
- No file references `SHELL_AUTOSTART_CMD`, `HARNESS_AUTOLOGIN_ARMED`, `CLUU_SHELL_AUTOSTART_CMD`, or `SHELL_AUTOSTART_CMD_DEFAULT`
- `try_auto_login` and `auto_login_done` deleted from root-procmgr
- `harness_matrix.sh` green
- `git log --oneline` shows the staged commit series for traceability
