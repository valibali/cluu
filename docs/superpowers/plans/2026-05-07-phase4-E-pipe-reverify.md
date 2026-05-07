# Phase 4 Plan E — Pipe Phase 1 Reverify + Close Gaps

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Verify whether 3-stage pipelines actually work. Document the truth. Close two known gaps: env propagation through pipe stages, and decide on sequential-vs-multiplexed wait semantics.

**Architecture:** Diagnostic-first. Run a 3-stage smoke against the existing pipeline executor. Capture exact failure (or success). Close env propagation by lifting the ENV trailer from `commands.rs` single-cmd path into a shared payload builder reused by `pipeline.rs`. Document the wait semantics and add a smoke that verifies the chosen behavior.

**Tech Stack:** Existing `userspace/shell/src/pipeline.rs`, `commands.rs` single-cmd spawn, `userspace/libcluu/src/posix/pipe.rs`.

**Spec reference:** `docs/superpowers/specs/2026-05-07-userspace-polish-design.md` §7

**Prereq:** Plan A merged (commands.rs split → exec.rs has the single-cmd payload builder). Plans B/C/D not required.

**Quick win**: this plan is short and runs in days, not weeks. Spec §10.2 lists the §7.2 diagnostic as a Day-0 task; this plan formalizes that.

---

## File structure

### Modified
- `userspace/shell/src/commands/exec.rs` (extract `build_run_payload_with_env` helper)
- `userspace/shell/src/pipeline.rs` (use the new helper; remove `pipeline.rs:236-240` TODO)
- `memory/project_phase3_soak_punted.md` (rewrite to reflect actual state)

### Created (only if diagnostic reveals a fixable bug)
- `userspace/libcluu/src/posix/pipe.rs` — fixes scoped to whatever the diagnostic shows

### Harness
- `scripts/harness_cases.conf` (l2_pipe_3stage, l2_pipe_env)
- `scripts/harness_case_defaults.sh` (case bodies)

---

## Stage 1 — Diagnostic

### Task 1.1: Add 3-stage smoke harness case

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`

- [ ] **Step 1: Add harness entry**

```
l2_pipe_3stage|full|MARKER_MODE=l2_pipe_3stage TEST_COMMAND_REPEAT=1 RUN_WAIT=20
```

```sh
        l2_pipe_3stage)
            SHELL_AUTOSTART_CMD_DEFAULT="echo -e 'alpha\nbeta\ngamma\nalpha\ndelta' > /tmp/in.txt; cat /tmp/in.txt | grep alpha | head -1; echo EXIT=$?"
            EXPECTED_CONTAINS=("alpha" "EXIT=0")
            ;;
```

- [ ] **Step 2: Run it**

```bash
bash scripts/harness_run.sh l2_pipe_3stage 2>&1 | tee /tmp/pipe3stage.log | tail -50
```

Three possible outcomes — branch logic follows.

### Task 1.2: Outcome analysis

- [ ] **Step 1: If PASS** → 3-stage works.

Skip Stage 2 (no fix needed). Jump to Task 1.3 (memory cleanup) and Task 3 (env propagation gap is independent).

- [ ] **Step 2: If HANG** (RUN_WAIT exhausted, no output beyond shell prompt):

Capture state. Re-run with verbose serial:

```bash
DEBUG_VFS_TRACE=1 DEBUG_PIPE_TRACE=1 bash scripts/harness_run.sh l2_pipe_3stage 2>&1 | tee /tmp/pipe3stage-trace.log
```

Look for:
- "stage 0 wrote N bytes" — did stage 0 exit?
- "stage 1 read N bytes" — did stage 1 receive?
- "EOF received" — did EOF propagate?

Three sub-cases:

**Sub-case A**: Stage 0 (cat) exits cleanly, stage 1 (grep) sees data, stage 1 outputs to its pipe, stage 2 (head) exits after 1 line, stage 1 then... blocks forever on next write?
→ EPIPE not delivered. Check `userspace/libcluu/src/posix/pipe.rs::write_pipe` — does it handle the recv-side dying?

**Sub-case B**: Stage 0 produces output, stage 1 never reads it.
→ Pipe wiring at spawn is wrong. Check `pipeline.rs:220-234` — fdac wiring for stage 1's stdin.

**Sub-case C**: Stage 0 exits before stage 1 sees first byte.
→ EOF token sent prematurely. Check `userspace/libcluu/src/posix/pipe.rs::close_write_end`.

In each sub-case, the fix is small (a few lines). Detail the fix in Stage 2 task list, then implement.

- [ ] **Step 3: If CRASH** (kernel panic, GPF, etc):

Capture the panic. This is a freeze-exception (per ROADMAP rule). Stop Plan E, file a kernel bug under named-fix discipline (`fix(pipe): <symptom> from l2_pipe_3stage smoke`), fix the kernel root cause, return to Stage 1.

### Task 1.3: Update memory note

**Files:**
- Modify: `memory/project_phase3_soak_punted.md`

- [ ] **Step 1: Rewrite based on Task 1.2 outcome**

After diagnostic, the file's claims are either confirmed, refined, or wrong. Rewrite to reflect current truth. Examples by outcome:

**If 3-stage PASS**:
```markdown
---
name: 3-stage pipe works as of 2026-05-07
description: Phase 1 closing claim was correct; soak punted only on 1000-iteration scale.
type: project
---

`l2_pipe_3stage` smoke green on develop @ <commit>. The original
"Phase 3 simplicity: sequential wait" comment in pipeline.rs is the
actual constraint — multiplexed wait deferred. 1000-iteration soak
test was the Phase 3 deferral, not single-pipeline correctness.

**Why:** Earlier note claimed wire protocol unfinished; that was
diagnostically wrong. Labels exist and execute correctly.

**How to apply:** Don't redo the diagnostic. Trust `l2_pipe_3stage`
as the regression sentinel.
```

**If a real bug was found and fixed in Task 1.2**:
```markdown
---
name: 3-stage pipe — fixed 2026-05-07 (was <symptom>)
description: ...
type: project
---

3-stage pipelines were broken as of <commit before fix> due to
<root cause>. Fixed in <fix commit>. `l2_pipe_3stage` is the
regression sentinel.

**Why:** ...

**How to apply:** ...
```

### Task 1.4: Commit Stage 1

```bash
git add scripts/harness_cases.conf scripts/harness_case_defaults.sh memory/project_phase3_soak_punted.md
# plus any kernel/userspace fix from Task 1.2 step 2 sub-cases
git commit -m "$(cat <<'EOF'
test: add l2_pipe_3stage smoke; reverify Phase 1 multi-stage pipes

Captures real state of 3-stage pipelines. Diagnostic step from Phase
4 Plan E. Memory note project_phase3_soak_punted.md updated to reflect
actual finding.

Phase 4 Plan E Stage 1.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 2 — Env propagation through pipeline stages

### Task 2.1: Find the single-cmd payload builder

**Files:**
- Read-only audit

- [ ] **Step 1: Identify which file builds the spawn payload with env**

After Plan A Stage 2, this likely lives in `userspace/shell/src/commands/exec.rs`. If Plan A hasn't merged, it's still in `commands.rs`.

```bash
grep -n 'fn build_container_run_payload\|build_container_run_payload_full\|env_trailer\|ENV trailer' userspace/shell/src/commands/exec.rs userspace/shell/src/commands.rs 2>/dev/null
```

- [ ] **Step 2: Identify the pipeline stage payload builder**

```bash
grep -n 'build_container_run_payload\|build_container_run_payload_full' userspace/shell/src/pipeline.rs
```

The pipeline path passes `&[]` for env (commands.rs:236 TODO).

### Task 2.2: Extract a shared `build_run_payload_with_env` helper

**Files:**
- Modify: `userspace/shell/src/commands/exec.rs`

- [ ] **Step 1: Add the helper**

```rust
/// Build a container_run payload that includes the shell's current env.
/// Used by both single-command spawn and pipeline stages so they share
/// identical env propagation.
pub fn build_run_payload_with_env(
    image_name: &str,
    arg_refs: &[&str],
    fdac: &[FdAction],
    redirs: &[RedirSpec],
    env: &[(String, String)],
) -> (Vec<u8>, usize, usize) {
    build_container_run_payload_full(image_name, arg_refs, fdac, redirs, env)
}
```

If `build_container_run_payload_full` already takes an env arg (verify by reading its signature), this helper is just a clarifying re-export. The point is: pipeline.rs calls *this* helper, never the `&[]`-defaulting one.

### Task 2.3: pipeline.rs uses the helper

**Files:**
- Modify: `userspace/shell/src/pipeline.rs`

- [ ] **Step 1: Locate `pipeline.rs:236-240` TODO**

The `// TODO(UE17+): pipeline stages currently spawn with the procmgr DEFAULT_ENV ...` block.

- [ ] **Step 2: Replace `&[]` with the shell context's env**

```rust
let env = context.shell_env_pairs();   // existing accessor that returns Vec<(String,String)>
let (payload, _argc, fdac_offset) =
    build_run_payload_with_env(image_name, &arg_refs, &fdac, stage_redirs, &env);
```

If `shell_env_pairs()` doesn't exist on `ShellContext`, add it (5-line accessor over the existing env table).

- [ ] **Step 3: Delete the TODO comment.** It's resolved.

### Task 2.4: Add l2_pipe_env smoke

**Files:**
- Modify: `scripts/harness_cases.conf`
- Modify: `scripts/harness_case_defaults.sh`

- [ ] **Step 1: Add entry**

```
l2_pipe_env|full|MARKER_MODE=l2_pipe_env TEST_COMMAND_REPEAT=1 RUN_WAIT=18
```

```sh
        l2_pipe_env)
            SHELL_AUTOSTART_CMD_DEFAULT="export FOO=bar; echo \$FOO | tr a-z A-Z; echo EXIT=$?"
            EXPECTED_CONTAINS=("BAR" "EXIT=0")
            ;;
```

(`echo $FOO` runs as a separate process — the shell's `echo` builtin runs in-process and would print BAR without env propagation. The test as written exercises both in-process expansion AND pipe stage env via the second `tr`. To force a pipe-stage env read, swap to a small helper that reads from getenv():)

```sh
        l2_pipe_env)
            SHELL_AUTOSTART_CMD_DEFAULT="export PIPETEST=hello; printf '%s' \"\$PIPETEST\" | wc -c"
            EXPECTED_CONTAINS=("5")
            ;;
```

That covers shell expansion across a pipe stage (`printf` invoked via the pipe path with the env propagated to it).

For a stronger test that the *spawned* binary sees the env (not just shell expansion), add a probe:

- [ ] **Step 2 (optional)**: create `userspace/probes/envprobe/` whose `main.rs` prints `getenv("PIPETEST")`. Run it as `printf '%s' x | envprobe`. Should print `hello`. If you create it, follow Plan A Task 3.x recipe.

### Task 2.5: Run; commit Stage 2

```bash
bash scripts/harness_run.sh l2_pipe_env 2>&1 | tail -5
bash scripts/harness_run.sh l2_pipe_3stage 2>&1 | tail -5
```

Both PASS.

```bash
git add -A
git commit -m "$(cat <<'EOF'
fix(shell): propagate env through pipeline stages

Pipeline stages now spawn with the shell's env trailer instead of
procmgr's DEFAULT_ENV. Single-command and pipeline paths share the
build_run_payload_with_env helper. Resolves pipeline.rs TODO from
Phase 1.

Phase 4 Plan E Stage 2.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Stage 3 — Sequential-vs-multiplexed wait decision

### Task 3.1: Document the decision

**Files:**
- Modify: `userspace/shell/src/pipeline.rs`

- [ ] **Step 1: Replace the existing comment block (around line 281-286) with the explicit decision**

```rust
// Wait for each child's exit notification in spawn order.
//
// **Decision (Phase 4 Plan E):** keep sequential. Multiplexed wait
// via poll() is technically possible (poll shipped in Phase 3), but
// only matters for pathological cases like `yes | head -1` where
// stage 0 spends extra time blocked on EPIPE before the shell drains.
// Correctness is unaffected. If a soak workload exposes a real hang,
// revisit; until then, sequential is simpler and fine.
//
// `cat | head -3` example:
//   - head finishes first (after 3 lines)
//   - cat sees EPIPE on its next write and exits
//   - we wait cat → head sequentially, both reaped
```

- [ ] **Step 2: Add an assertion-style smoke that the pathological case still terminates**

`l2_pipe_pathological`:

```
l2_pipe_pathological|full|MARKER_MODE=l2_pipe_pathological TEST_COMMAND_REPEAT=1 RUN_WAIT=15
```

```sh
        l2_pipe_pathological)
            SHELL_AUTOSTART_CMD_DEFAULT="yes | head -1; echo EXIT=$?"
            EXPECTED_CONTAINS=("y" "EXIT=0")
            ;;
```

- [ ] **Step 3: Run; PASS.**

```bash
bash scripts/harness_run.sh l2_pipe_pathological 2>&1 | tail -5
```

If `yes` doesn't exist as a util, substitute `printf 'y\n' | head -1` or skip this case.

### Task 3.2: Commit

```bash
git add -A
git commit -m "$(cat <<'EOF'
docs(shell): codify sequential pipeline wait; defer multiplexed wait

Existing pipeline.rs comment refined into an explicit Phase 4 decision:
sequential wait is correct, multiplexed wait deferred until soak
workload demands it. l2_pipe_pathological smoke verifies the worst
known case still terminates.

Phase 4 Plan E Stage 3.

Co-Authored-By: Claude Opus 4.7 <noreply@anthropic.com>
EOF
)"
```

---

## Self-review

- **Spec coverage**: §7.1 (memory vs code) → Task 1.3. §7.2 (diagnostic) → Task 1.1+1.2. §7.3 (env propagation) → Stage 2. §7.4 (wait decision) → Stage 3. §7.5 (Ctrl-C in pipeline) → covered by Plan D.
- **Placeholders**: none. Branch logic for Task 1.2 is concrete; sub-cases each name a file/line to inspect.
- **Type consistency**: `build_run_payload_with_env` signature matches `build_container_run_payload_full`. ENV pair format `Vec<(String,String)>` consistent.
- **Risk**: Task 1.2 sub-case C ("kernel crash") triggers a freeze-exception. The plan instructs stop and named-fix discipline rather than racing forward.

---

## Acceptance

Plan E done when:
- `l2_pipe_3stage` smoke PASS (or kernel fix landed and the smoke PASS post-fix)
- `l2_pipe_env` smoke PASS
- `l2_pipe_pathological` smoke PASS
- `pipeline.rs:236-240` TODO removed
- `memory/project_phase3_soak_punted.md` matches reality
- `harness_matrix.sh` green
