# Task Context: Plan 2 — Terminal + PTY Unification

Session ID: 2026-05-19-plan2-pty-unification
Created: 2026-05-19T12:00:00Z
Status: in_progress

## Current Request
Implement Plan 2: Unify the legacy TTY service and cluuterm onto one PTS_* verb set (labels 100-110). Shared line-discipline library. POSIX terminal signal coverage. Per-session /dev/pts/ overlay. POSIX termios shims. TERM env propagation.

## Context Files (Standards to Follow)
- .opencode/context/core/standards/code-quality.md

## Reference Files (Source Material to Look At)
- docs/superpowers/plans/2026-05-19-implementer-brief.md (CLUU architecture primer)
- docs/superpowers/plans/2026-05-18-plan2-terminal-pty-unification.md (13-task implementation plan)
- .tmp/handoff-plan2.md (Plan 1 handoff state)
- userspace/cluu_proto/src/lib.rs (existing proto crate)
- userspace/cluu_proto/src/spawn.rs (reference for proto module style)
- userspace/libcluu/src/tty_core/line_discipline.rs (existing line discipline)
- userspace/libcluu/src/tty_core/mod.rs (existing tty_core)
- userspace/cluuterm/src/tty_backend.rs (existing cluuterm backend)
- userspace/cluuterm/src/main.rs (cluuterm main)
- userspace/tty/src/main.rs (tty service dispatch)
- userspace/tty/src/protocol.rs (legacy TTY_* labels)
- userspace/shell/src/ (shell dual-protocol branch)
- userspace/vfs/src/pts.rs (VFS pts handling)
- userspace/vfs/src/view.rs (VFS view derivation)
- userspace/vfs/src/main.rs (VFS dispatch)
- userspace/probes/argvprobe/ (probe template)
- userspace/libcluu/src/posix/ (existing posix shims)

## External Docs Fetched
None — no new external library dependencies beyond postcard/bitflags already in workspace.

## Components
1. cluu_proto::pts module — verb labels + types (Task 1)
2. libcluu::tty_core::line_discipline — LineDiscOutput API (Task 2)
3. libcluu::tty_core::routing — service-shared routing helper (Task 3)
4. cluuterm PTS_* verbs (Task 4)
5. tty service PTS_* verbs (Task 5)
6. POSIX termios/ioctl shims (Task 6)
7. Shell drops dual-protocol branch (Task 7)
8. VFS per-session /dev/pts overlay (Task 8)
9. Cluuterm registers pts in session (Task 9)
10. TERM=xterm-256color env (Task 10)
11. SIGWINCH on window resize (Task 11)
12. Delete dead TTY_* labels (Task 12)
13. 8 acceptance markers (Task 13)

## Constraints
- No new syscalls
- No timeouts (recv_with_timeout/call_with_timeout)
- No refactoring beyond plan scope
- Commit after every task
- Build green between tasks (cargo xtask build + harness_run.sh)
- Microkernel discipline: tokens, not ambient authority
- postcard serialization for all wire formats
- fd 0-3 (stdin, stdout, stderr, stdlog)

## Exit Criteria
- [ ] Task 1: cluu_proto::pts types + labels, 5 tests pass
- [ ] Task 2: LineDiscOutput API + 8 tests pass
- [ ] Task 3: Routing helper builds clean
- [ ] Task 4: Cluuterm speaks PTS_* verbs, boot smoke passes
- [ ] Task 5: TTY service speaks PTS_* verbs, boot smoke passes
- [ ] Task 6: POSIX termios shims build
- [ ] Task 7: Shell dual-protocol branch deleted, boot smoke passes
- [ ] Task 8: VFS per-session overlay builds
- [ ] Task 9: Cluuterm session_id wiring
- [ ] Task 10: TERM=xterm-256color env
- [ ] Task 11: SIGWINCH wiring
- [ ] Task 12: Zero-hit grep proofs for legacy TTY_* labels
- [ ] Task 13: 8 acceptance markers pass
- [ ] Final: cargo xtask build clean
- [ ] Final: harness_run.sh reaches compositor: ready