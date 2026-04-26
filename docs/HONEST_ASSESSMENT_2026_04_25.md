# CLUU — Honest Assessment & Revised Path

*Snapshot: 2026-04-25.*
*Audience: Balazs, in three months, on a Sunday afternoon when motivation is wobbly.*
*This is point-in-time. ROADMAP.md is the forever-doc. If they conflict, ROADMAP wins.*

---

## 1. The brutal-truth read of the code

**Kernel: actually good.** 9/10 in the audit you commissioned, and that audit doesn't lie. seL4-inspired capability IPC, ~1,200-1,600 cycles round-trip, SMAP/SMEP/Spectre/retpoline, proper fault routing, FPU/SSE eager save, fast IPC tier-1 done. **Stop polishing the kernel.** Every kernel hour from here is an hour not spent shipping the OS.

**Userspace: 3/10 *because* every interesting feature is half-built.** You don't have a userspace gap because of inability — you have it because you keep starting things at 80%. Login works. Shell runs. `cd`/`pwd` work. `mkdir`/`rm` work. `cp`/`mv` exist as plans. MicroPython has been "almost ready" for a year. There's no editor that runs. There's no pipe execution. The fix is not adding more half-features, it's finishing the next two.

**Procmgr: structurally smelly but functionally correct.** 5400 lines in one file with intertwined spawn/container/session/restart/procfs/IPC concerns. You're right to be uncomfortable. You're also right that refactoring it now would burn 1-2 weeks on something user-invisible. Deferring to Phase 3 is correct — it folds naturally into SpaceDestroy + leak audit work. Don't refactor today.

**Test harness: timing-based, brittle, and you know it.** Stream-tail (#73) closes 80% of the flake. gdbstub-driven testing is a hobby project inside a hobby project; defer until something specific demands it. Don't over-rebuild this either.

**The 40-day WIP that you sealed in 9cd0ddf was the most honest signal you've sent yourself in months.** You named the pattern, you fixed the symptom. You did not fix the process that produced it. The next 40-day WIP is one good kernel idea away from now unless you change how you respond to "good kernel idea" thoughts.

---

## 2. The brutal-truth read of how you work

**You design beautifully and finish slowly.** Today's session is a clean example: a Plan 2 binary (`/bin/rm`) was blocked by a real architectural issue (`/tmp` inheritance). Instead of patching the immediate blocker, you (correctly) brainstormed a principled `MOUNT <path> <policy>` Cluufile directive, executed eleven TDD-driven tasks across kernel, libcluu, procmgr, VFS, and harness, then discovered a deeper race that needed a kernel-touch fix, and brainstormed *that* into another fourteen-task plan. The work is excellent. **None of it advances Phase 1 exit criteria.** A user still cannot pipe `cat | grep | head` in your shell.

This is the loop you keep running:

```
small task →
  hits architectural friction →
    design beautiful fix →
      execute fix elegantly →
        discover deeper friction →
          repeat
```

The loop produces good code and a longer ship date. It feels like progress because each level *is* progress. It is also how you arrived at "MicroPython has been almost ready for a year."

**You are at high risk of optimizing the toolchain instead of advancing the goal.** The procmgr refactor, the harness rewrite, the gdbstub idea, the spec quality — all are real improvements. None of them are user-visible. A stranger downloading `cluu.iso` doesn't know any of them happened. They know whether `cluu.iso` exists.

**You catch your own drift patterns and then rationalize past them.** ROADMAP §4 is your handwriting; the four patterns are named with painful specificity. You also have an explicit "blocking-correctness exception" carve-out in the kernel freeze — a real and necessary escape hatch, but an escape hatch you've already used twice this session. The exception is fine when used twice in two months. It's a problem when used twice in two days.

**Long pause + lost motivation is not a character flaw, it's a system signal.** Every solo hobby project hits this. The cause is almost always the same: progress is invisible because cycles are too long. The fix is shorter cycles with smaller user-visible deliverables. You felt like you weren't moving because mount-policy and the race fix took a full session and produced no new feature a user can run. They were necessary. They were also necessary indications that the cycle is too long.

---

## 3. What was actually wrong with the roadmap

You sensed something off. Here is what I think it is:

**Network as Phase 4 means you may never ship.** ROADMAP §5 calls Phase 4 "the biggest risk in the whole plan" with a literal three-week pivot trigger. By placing it *before* Phase 5 (Ship), you've gated shipping on the riskiest phase. If Phase 4 stalls, Phase 5 never happens. This is the same logic that produced "MicroPython has been almost ready for a year" — gating a finishable thing behind an open-ended thing.

**Resource discipline (Phase 3) is half ship-blocker, half over-engineering.** SpaceDestroy is real and matters. Compiler-warning cleanup is hygiene, not a feature. Soak-test bounded memory is sensible but probably already true on a TUI workload. The phase as written mixes "must do before shipping" with "nice to have eventually."

**The phases are right; the order is wrong.**

---

## 4. The revised phased path

This is the order I'd recommend. Push back if you disagree — it's your project.

### Phase 0 (in flight) — Seal WIP + harness green

You're 90% there. Race fix (#71) is mid-implementation. Stream-tail harness (#73) is queued. **Done when:** `bash scripts/harness_suite.sh` reports ≥45/46, with `l2_owner_deny` (#70) the only acceptable known-fail.

### Phase 1 — Shell usability

Unchanged from ROADMAP §5. The exit criteria are good and concrete. **Discipline rule:** if a Phase 1 task hits architectural friction (like Plan 2 hit /tmp policy), patch it minimally first, capture the deeper fix as a follow-up, return to Phase 1. Do not turn one Phase 1 task into a four-task architecture project. The fix is allowed; deferring the test it unblocks is not.

### Phase 2 — Write code in CLUU

Unchanged from ROADMAP §5: MicroPython REPL + script execution + one editor. **Pick the editor first and pick the smallest one.** Don't shop. `kilo` is ~1000 lines and ports cleanly to a CLUU TTY. Port it; finish it; don't touch it again.

### Phase 3 (NEW) — Ship v1

This is the change. Skip ahead to the Ship phase. After Phase 2 you have:

- Login + shell + `cd`/`pwd`/pipes/redirect/edit
- MicroPython REPL that reads/writes files
- A working text editor

That is a usable TUI hobby OS. **It is shippable as v1.** Build the ISO, write the README (200 lines, with a GIF), publish on r/osdev. *Posting is the last action* — that's already in ROADMAP §5 Phase 5; just bring it forward. Do not gate shipping on network or perfect resource discipline. Ship now, fix in v2.

Why this is the right call:
- **A v1 release is the proof point that breaks "almost ready for a year."** Once a stranger has booted your ISO, the project is real. Until then it's potential.
- **Feedback only exists post-publish.** A perfect unposted OS gets nothing — you wrote that yourself in ROADMAP §5 Phase 5.
- **Motivation problems get smaller after public release.** Compounding engagement matters; isolated Saturday sessions toward an unposted goal don't.

### Phase 4 (was 3+4) — Production polish

After v1 is live and you have feedback:
- **SpaceDestroy** if leaks are visible to actual users.
- **Resource discipline** sized to the actual problem you observe, not the theoretical one.
- **Network** — virtio-net, DHCP, ping, sockets, wget. Three-week pivot trigger from ROADMAP still applies. Network as v1.1 or v1.2 is fine and normal.

### Phase 5 (was 5) — Folded into Phase 3

Shipping is what Phase 3 *is* in this revised plan. There is no separate "ship" phase; shipping is the v1 deliverable.

---

## 5. The next 30 days, concretely

If you'd asked me "what's the very next thing," answer:

1. **Today:** finish the race-fix sweep (#71 — Tasks 10-14). All four flaky cases must hit 10/10.
2. **This week:** stream-tail the harness (#73). One bash refactor. Eliminates the timing-flake noise.
3. **Next 2 weeks:** Phase 0 close-out. Run the full harness suite under the new harness. Update ROADMAP §5 Phase 0 to ✅ and tick the harness-green box in CURRENT_PHASE.md. **Commit and walk away from the kernel.**
4. **Next 4-6 weeks:** Phase 1. Pipes + redirection + line editing + history + tab + `$?` + `cp` + `mv` + `grep`/`head`/`tail`/`wc`. **Frequent small commits.** Each user-visible feature in its own commit. Aim for one feature per evening session.
5. **End of June:** Phase 2 starts. MicroPython, then `kilo`. Single-track.
6. **End of August:** v1 released. ISO posted. Hacker News thread.

Six months from now this document should be obsolete. If it isn't, re-read it.

---

## 6. The motivation problem, addressed directly

Your motivation didn't disappear because you got worse at engineering. It disappeared because the cycle is too long and the deliverable is invisible. The fix is **smaller cycles + visible deliverables**, not "find motivation."

Concrete tactical anchors:

- **Commit visible features daily, not weekly.** "Add `>` redirection" is a one-evening commit. "Wire pipe execution end-to-end" might be three evenings. Both are user-visible. Mount-policy + race-fix combined is two days of invisible-to-user infrastructure. Don't stop doing infrastructure when it's needed — just notice when you've done two days of it without a user-visible feature, and ask whether the next thing should be infrastructure or a feature.
- **Rule of thumb: user-visible feature every 3 sessions.** If you've gone four sessions without committing something a user can run differently, the patterns are winning. Re-read this doc.
- **Forgive the pauses.** A one-month pause didn't unmake your work. The kernel is still 9/10 the day you come back. Pick up at CURRENT_PHASE.md and continue.
- **Ship the v1 even if it embarrasses you.** Especially then. The unposted "almost-perfect" version is the version with no users. The posted "honestly-rough" version is the version that gets feedback.

---

## 7. What this document is not

- **Not a plan.** Plans live in `docs/superpowers/plans/`. This is a strategic mirror.
- **Not permanent.** Re-read in 3 months. Replace then if your patterns have shifted. Date in the filename is intentional.
- **Not blame.** Every project I've seen at this stage has these patterns. Naming them is how you beat them.

---

## Closing

The kernel is finished. The OS isn't. The gap is finishing, not engineering. Get to v1.
