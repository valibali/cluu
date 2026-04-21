# Current Phase

**Phase:** 0 — Seal the 40-day WIP
**Started:** 2026-04-21
**Last updated:** 2026-04-21

---

## Goal

Get `develop` into a reviewable, mergeable, CI-verified state.

## Exit criteria

*(copied from `ROADMAP.md` §5 — do not edit here; tick as completed)*

- [ ] R1: SysV ABI preservation check committed *(code implemented 2026-04-21, verified at boot, awaiting commit)*
- [ ] R2: RDRAND zero-salt fix committed *(code implemented 2026-04-21, awaiting commit)*
- [ ] WIP split into 4 logical commits per audit §0.4:
  - [ ] Commit 1 — IPC Tier-1 optimizations
  - [ ] Commit 2 — Security hardening (SMAP/SMEP/Spectre/retpoline)
  - [ ] Commit 3 — Async notifications (A2)
  - [ ] Commit 4 — TPM + userspace auth
- [ ] `bash scripts/harness_matrix.sh` runs green end-to-end
- [ ] Every commit message names *why*, not *what*
- [ ] `git status` clean on `develop`

## Doing now

R1+R2 code implemented and smoke-tested. Immediate next action: commit R1+R2 as the first split commit ("Phase 0.1 — residual risks"), **then** stage the 4 WIP-split commits starting with IPC Tier-1 optimizations.

## Blockers

None.

## Pivot triggers

- If `harness_matrix.sh` surfaces a regression that takes longer than one day to diagnose, stop splitting and bisect first. Do not accumulate more changes on top of a broken baseline.

## Deferred kernel ideas

*(add an entry each time you catch yourself wanting to "quickly optimize X" mid-phase. Do not act on any of these during the freeze. Review at phase end.)*

- *(empty)*

## Notes to self

*(freeform scratch area; ok to delete at phase end)*

- R1+R2 are already committable as their own "Phase 0.1 — residual risks" commit. Consider landing that first, separately from the 4-commit WIP split, so the split starts from a clean verified baseline.
