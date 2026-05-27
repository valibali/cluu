# Procmgr Cap-Refactor Coverage Matrix

> Phase 14.2 deliverable for `procmgr-cap-refactor` branch.
> Authoritative inventory of which handler × branch is covered by which test artifact.

## Scope

Crates under coverage gate (15% allowed delta vs develop baseline):

| Crate | Path | Role |
| ----- | ---- | ---- |
| `procmgr-common` | `userspace/libs/procmgr-common/` | Shared types, envelopes, mint-guard, pid encoding |
| `cluu-root-procmgr` | `userspace/root-procmgr/` | Root supervisor; bootstraps primordials |
| `cluu-session-procmgr` | `userspace/session-procmgr/` | Per-user-session supervisor |

## Test Artifacts

Two tiers:

1. **Host-tests** (`cargo test --workspace --features host-test`) — unit + property tests under `#[cfg(test)]`, no QEMU.
2. **In-target probes** (`/var/images/pm_*/`) — Rust no_std binaries booted under QEMU; emit `MARKER:<name>:PASS` or `<name>: PASS`.

## Coverage Matrix

| Surface | Handler / Branch | Host-test | In-target probe |
| ------- | ---------------- | --------- | --------------- |
| **VfsViewManager cap** | derive narrow (sid, mask) | `view_table::tests::derive_narrow` | `pm_vfs_view_scope` (case_a) |
| | derive widen → rejected | `view_table::tests::derive_widen_denied` | `pm_vfs_view_scope` (case_b) |
| | derive sid-change → rejected | `view_table::tests::derive_sid_change_denied` | `pm_vfs_view_scope` (case_c) |
| | root mint arbitrary sid | `view_table::tests::root_mint_any_sid` | `pm_vfs_view_scope` (case_d) |
| **PID encoding** | sid \| local roundtrip | `pid::tests::roundtrip` | `pm_pid_layout` (case_a) |
| | sid fits 8-bit | `pid::tests::sid_fits_u8` | `pm_pid_layout` (case_b) |
| **Session lifecycle** | create + query + destroy | `session_table::tests::lifecycle` | `l3_session_create_destroy`, `l3_session_query` |
| | derive_token narrow | `cap_broker::tests::derive_narrow` | `l3_session_derive_narrow` |
| | set_leader monotone | `session_table::tests::set_leader_monotone` | `l3_session_set_leader_monotone` |
| | leader exit cascade | `child_monitor::tests::leader_exit` | `l3_session_leader_exit_cascades` |
| | session_id recycle / monotone | `session_directory::tests::recycle` | `pm_session_id_recycle` |
| | cross-session isolation | `session_table::tests::cross_session_no_alias` | `pm_cross_session_no_leak` |
| | session destroy revokes derived caps | `cap_broker::tests::revoke_on_destroy` | `pm_cap_revoke_stale` |
| | session crash cascade | `child_monitor::tests::leader_crash` | `pm_session_crash_cascade` |
| **Spawn protocol** | unified spawn envelope | `spawn::tests::envelope_roundtrip` | (covered by every probe that spawns) |
| | FdInherit narrow | `cap_broker::tests::fd_inherit_narrow` | `denyprobe` (`f11_deny_inherit`) |
| | spawn env merge | `envelopes::tests::merge_caller_wins` | `f10_view_passthrough` |
| **Restart policy** | RestartPolicy wire roundtrip | `restart::tests::policy_roundtrip` | `pm_service_restart` (structural) |
| | OnFailure backoff | `restart_root::tests::on_failure_backoff` | — (deferred; needs supervisor manifest) |
| | crash-loop disarm | `restart_root::tests::crash_loop_disarms` | — (deferred) |
| **Two-pmgr bootstrap** | both endpoints registered | `services::tests::registry_seed` | `pm_bootstrap_two_pmgr` |
| | root and session disjoint authority | — (architectural invariant) | `pm_bootstrap_two_pmgr` |
| **PROC_QUERY_ALL cap** | unprivileged denied | `proc_query_all::tests::unpriv_denied` | `pm_proc_query_all_cap` |
| | privileged returns list | `proc_query_all::tests::priv_returns_list` | — (host-only; needs cap-mint scaffolding in target) |
| **Mint-guard RAII** | drop on early-return frees | `mint_guard::tests::drop_releases` | — |
| | success consumes the guard | `mint_guard::tests::commit_consumes` | — |

## Known Coverage Gaps

These are explicitly deferred from the cap-refactor scope:

- **Live restart-loop in QEMU** — needs a supervised image declaring `RestartPolicy::Always` + a fault-injection hook. `pm_service_restart` ships as a structural test only.
- **Procmgr-internal crash-cascade** — root-procmgr restarting session-procmgr is exercised by init's primordial monitor (Phase I/15) but no in-target probe specifically targets the procmgr-restart path.
- **PROC_QUERY_ALL with elevated cap** — the privileged path is exercised by host-tests; the in-target probe `pm_proc_query_all_cap` only verifies the deny path.

These gaps are tracked in the followups memory file (`project_autologin_removal_followups_2026_05_26.md`) and should land before the next cap-refactor revision, not before this branch merges.

## Coverage Targets

- **Line coverage**: ≥ 95 % for each of the three procmgr crates (`procmgr-common`, `cluu-root-procmgr`, `cluu-session-procmgr`).
- **Branch coverage**: ≥ 95 % for the same set.
- Enforced by `cargo xtask coverage-check` (Phase 14.1). The xtask wraps `cargo llvm-cov --workspace --features host-test --json` and asserts thresholds; CI runs it on PR.

Probes themselves are NOT in the coverage gate — they are integration smokes, not units. Their PASS lines on serial output are the contract.
