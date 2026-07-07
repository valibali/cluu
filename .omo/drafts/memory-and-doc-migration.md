# Draft: memory-and-doc-migration

## Status
- phase: approval-gate
- intent: CLEAR
- classify: Architecture (system design, 5+ modules, long-term impact)

## Request
1. Plan for ALL proposed memory-related upgrades (C3, C6, M1-M16; C1/C2/C4/C5 already done)
2. Retire docs/ folder; transfer ALL knowledge (not file-level) into doc/book/ (the rustdoc-rendered book)

## Topology lock (components)
| ID | Component | Outcome | Status | Evidence |
|----|-----------|---------|--------|----------|
| A | Memory code upgrades | C3 MAP_GUARD + C6 bump-pointer nursery + M1-M16 implemented and verified | planned | allocator.rs, kernel/syscall/handlers.rs, posix/memory.rs |
| B | Doc knowledge extraction | All knowledge from docs/*.md extracted into doc/book/ chapters | planned | 11 docs/ files (5232 lines) |
| C | Superpowers knowledge extraction | Design decisions from 66 superpowers/ specs/plans (52K lines) extracted into relevant book chapters | planned | docs/superpowers/{specs,plans,audits,designs}/ |
| D | Book restructuring | doc/book/ expanded with new chapters; doc/src/lib.rs updated with new modules | planned | 13 existing chapters → ~20 chapters |
| E | Cross-reference updates | README.md (5 refs), AGENTS.md (4 refs), .rs file comments updated to point to doc/book/ | planned | grep verified |
| F | docs/ retirement | docs/ directory deleted after all knowledge transferred | planned | user instruction |

## Genuine fork
1. **superpowers/ fate after extraction** — see approval brief

## Approval gate
- pending action: write .omo/plans/memory-and-doc-migration.md
- approach: ONE plan, two parallel tracks (memory code + doc migration), ~12-16 waves
