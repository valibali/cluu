# Current Phase

**Phase:** 0 — Seal the 40-day WIP
**Started:** 2026-04-21
**Last updated:** 2026-04-21 (post-commit b574664)

---

## Goal

Get `develop` into a reviewable, mergeable, CI-verified state.

## Exit criteria

*(copied from `ROADMAP.md` §5 — do not edit here; tick as completed)*

- [x] R1: SysV ABI preservation check committed *(b574664; boot log "SysV ABI preservation check passed (RBX/RBP/R12-R14)")*
- [x] R2: RDRAND zero-salt fix committed *(b574664; hash_password now returns Option)*
- [x] WIP bundled as single commit b574664 *(deviation from audit §0.4 four-commit plan: files were too entangled to split cleanly after the fact; commit message itemizes the 6 feature areas and explains the tradeoff)*
- [ ] `bash scripts/harness_matrix.sh` runs green end-to-end
- [x] Commit messages name *why*, not *what* *(b574664, 4134de3)*
- [x] `git status` clean on `develop`

## Doing now

WIP sealed as b574664; hello smoke test green. Immediate next action: run `bash scripts/harness_matrix.sh` end-to-end. If all cases pass, Phase 0 is done and we roll into Phase 1 (Shell usability). If any case regresses, bisect against the last-known-green commit (d40502c) before touching anything else.

## Blockers

None.

## Pivot triggers

- If `harness_matrix.sh` surfaces a regression that takes longer than one day to diagnose, stop splitting and bisect first. Do not accumulate more changes on top of a broken baseline.

## Deferred kernel ideas

*(add an entry each time you catch yourself wanting to "quickly optimize X" mid-phase. Do not act on any of these during the freeze. Review at phase end.)*

- *(empty)*

## Notes to self

*(freeform scratch area; ok to delete at phase end)*

- Deviation from the audit §0.4 four-commit plan was deliberate: `syscall.rs`, `syscall_entry.asm`, and `interrupts.asm` hold overlapping changes across IPC Tier-1, SMAP, and R1. A clean file-level split would have required reverting and re-authoring. The single commit's message enumerates the feature areas so `git log --grep` still works.
