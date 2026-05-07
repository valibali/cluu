# CLUU ROADMAP

*A personal compass, not a marketing document. Audience: Balazs, in a year, on a drifty Saturday afternoon.*

*Written: 2026-04-21*

---

## 1. State of the OS

CLUU is a ~68K-line Rust x86_64 microkernel plus ~20K lines of C and assembly, built over roughly 18 months of solo evenings and weekends. The kernel itself is solid: seL4-inspired, capability-based, IPC at ~1,200–1,600 cycles, measured boot, Spectre mitigations, SMAP/SMEP, proper fault handling. Rated 7.7/10 in its most recent audit — good enough. Userspace is where the gap lives: login works, but the shell has no `cd` or `pwd`, pipes don't actually execute, MicroPython has been "almost ready" for a year, no editor runs, no network, no documentation a stranger could follow. The kernel is 9/10; the apps are 3/10. This is not a proof-of-concept problem — it is a *finishing* problem. Every hour spent on the kernel from here is time not spent closing the apps gap.

---

## 2. The Choice

**What CLUU is becoming:** a *usable hobby OS* you can boot, log in to, navigate with a shell, edit text, write a Python script, and eventually browse a local web page. TUI first; GUI is a 2027+ problem. Small and comprehensible, not big and comprehensive.

**What CLUU is explicitly not:**

- **Not a Linux competitor.** Not trying to run Chromium, Postgres, systemd, or anything assuming a Linux personality beyond whatever POSIX surface newlib + the compat layer already covers.
- **Not a research artifact.** No paper, no novel scheduler, no exotic capability calculus beyond what's already built.
- **Not a teaching kernel.** Clean code is a happy side effect, not a design goal. Comments for future-you, not for a textbook reader.
- **Not a performance vehicle.** IPC is already fast enough. No more kernel microbenchmarks until something user-visible actually demands it.

**The rejected alternatives** (named because they will tempt you):

- *Ship it as a kernel-only educational demo* — faster to "done" but the 20K lines of userspace infrastructure become sunk cost. **Rejected.**
- *Pivot to Linux-binary compatibility* — would require years and make the kernel uninteresting. **Rejected.**
- *Keep polishing the kernel to 10/10* — infinite sink. **Rejected.**

---

## 3. The Commitment

**Kernel freeze: from 2026-04-21 through approximately 2026-10-21 (six months).** No speculative kernel work. No new audits. No IPC Tier-2 optimizations. No SMP. No new security hardening items. No GUI planning. The kernel is 9/10 — finished enough.

**The only exception:** a kernel bug that actively blocks Phase 1–5 work. Rule:

> Every kernel commit during the freeze MUST reference, in its first line, the userspace test case or failing scenario that forced it.
>
> *Example: "Fixes shell/test_cd.sh hang in recv() when stdin is a pipe to a dead child (Phase 1)."*
>
> If you cannot name the userspace failure, the commit does not go in. You are drifting.

The scope of the fix is exactly whatever the test needs — no adjacent cleanup, no "while I'm here let me also…", no prophylactic refactor. Commit, tag the testcase, return to userspace.

**Commit discipline** (a separate rule, unrelated to the freeze):

- **No uncommitted WIP older than 3 days on `develop`.** If 72h passes and the branch still has dirty files, stop new work, split into logical commits, push. Period.
- Prefer small bundled PRs over heroic 40-day ones. Review is cheap; retro-review of 40 days of entangled changes is not.
- Every commit message names *why*, not *what*. The diff tells you what.

**How the freeze ends:** on ~2026-10-21 or when Phase 3 completes (whichever is *later*), revisit. Not to resume kernel work by default — to ask honestly: *is the kernel still the bottleneck?* If the apps gap is closed enough to ship Phase 5, ship; kernel polish is a v1.1 problem.

---

## 4. Drift Patterns

Four patterns have cost real weeks. Naming them here makes them harder to rationalize in the moment.

### Pattern 1: "Quick kernel cleanup while I think"

**The move:** a warmup kernel task before starting the messy userspace work of the day. Feels productive; keeps the userspace blocker at arm's length.

**The counter:** if you catch yourself opening `kernel/src/` without a specific userspace testcase or failure referenced, close it. The open testcase is the only legitimate entry ticket during the freeze. No testcase, no kernel edit.

### Pattern 2: "This optimization will pay back in Phase 4"

**The move:** speculative kernel work justified by imagined future needs — usually performance.

**The counter:** Phase 4 is not here. The optimization is imaginary; the lost week is not. Note the idea somewhere out-of-band and move on. If Phase 4 actually needs it, you will know then — and will still have time, because you did Phase 1 first.

### Pattern 3: "Just one more audit"

**The move:** measuring the state of the system instead of advancing it. Produces readable documents; produces no user-visible capability.

**The counter:** if a prior audit document exists, re-read it. Do not write a new one. If a material state change has happened that the prior audit does not capture, add a dated update section to the *existing* file — do not start fresh. A new audit file during the freeze is a drift symptom.

### Pattern 4: 40-day WIP

**The move:** finishing-state-avoidance via perpetual work-in-progress. The kernel changes accumulate. Nothing merges. CI never sees it. Review is impossible.

**The counter:** the 3-day WIP rule from §3. If `git status` shows dirty files older than 3 days on `develop`, all new work stops until the branch is split into logical commits and pushed. This is the *only* rule in this document whose violation triggers a hard stop.

### Honorable mentions (less costly, still real)

- **"One more probe container"** — writing yet another `*probe` test container instead of the actual shell builtin it is supposed to probe.
- **"Just wire up Quake first so it looks cool"** — entertainment-driven prioritization. Quake runs on a Phase 4 stack you have not built yet.
- **"I'll move the strategic plan around one more time"** — re-planning as a substitute for executing. If this doc needs an edit, it is one-line terse, not a rewrite.

---

## 5. Phases

Each phase: **Goal** / **Exit criteria** (user-visible, all must be true) / **Allowed kernel work** (anything not on this list is drift) / **Known unknowns & pivot triggers**.

No dates. A phase is done when the capabilities exist, full stop.

---

### Phase 0 — Seal the 40-day WIP

**Goal:** get `develop` into a reviewable, mergeable, CI-verified state.

**Exit criteria:**

- [ ] R1 (SysV ABI preservation check) committed. *(implemented 2026-04-21, awaiting commit)*
- [ ] R2 (RDRAND zero-salt fix) committed. *(implemented 2026-04-21, awaiting commit)*
- [ ] WIP split into 4 logical commits per audit §0.4:
  - [ ] Commit 1: IPC Tier-1 optimizations
  - [ ] Commit 2: Security hardening (SMAP/SMEP/Spectre/retpoline)
  - [ ] Commit 3: Async notifications (A2)
  - [ ] Commit 4: TPM + userspace auth
- [ ] `bash scripts/harness_matrix.sh` runs green end-to-end.
- [ ] Every commit message names *why*, not *what*.
- [ ] `git status` clean on `develop`.

**Allowed kernel work:** whatever is needed to make the split clean and the matrix green. Nothing else.

**Known unknowns:** harness-matrix may surface latent bugs on first full run. If a bug predates the WIP, document and defer; if introduced by WIP, fix before split.

---

### Phase 1 — Shell usability ✅ DONE 2026-04-27

**Goal:** the shell feels like a shell, not a launcher.

**Exit criteria:**

- [ ] `cd /path`, `cd ..`, `cd` (home), `pwd` work correctly across sub-shells.
- [ ] Current working directory persists through spawned processes.
- [ ] `cat foo.txt | grep pattern | head -5` runs end-to-end with real pipe execution.
- [ ] Redirection works: `> file`, `>> file`, `< file`.
- [ ] `mkdir`, `rm`, `rm -r`, `cp`, `mv`, `grep`, `head`, `tail`, `wc` exist and behave like their Unix counterparts for the common cases.
- [ ] Line editing: backspace, left/right arrow, home, end, Ctrl-A, Ctrl-E, Ctrl-K, Ctrl-U.
- [ ] Command history: ↑/↓ retrieves previous commands within a session. (Persistent history deferred.)
- [ ] Tab completion for files and directories.
- [ ] `echo $?` returns the last command's exit status correctly.

**Allowed kernel work:** only bugs surfaced by Phase 1 tests — specifically the known `read(0, ...)` TTY deadlock. Named fix rule applies.

**Known unknowns & pivot triggers:** pipe execution may expose fd-inheritance bugs in the container/spawn path. Fix in procmgr/VFS first; kernel only if forced.

---

### Phase 2 — Write code in CLUU ✅ DONE 2026-05-06

**Goal:** you can run a script and edit a file without leaving the OS.

**Exit criteria:**

- [x] MicroPython starts, runs a one-liner, and reads a file from disk (`open('/etc/users.toml').read()`).
- [x] MicroPython writes a file (`open('/tmp/x.txt', 'w').write('hi')`).
- [x] MicroPython REPL handles multi-line input, Ctrl-C, Ctrl-D.
- [x] One text editor works: minimal vi-flavored TUI editor built from scratch (`/bin/edit`, ~3.8k LOC, piece-table + vim keymap + `:set` + atomic save).
- [x] You can edit a script in the editor, save it, run it, and see output — without rebooting (verified with `ls / edit hello.txt / :w / :q / ls`).

**Allowed kernel work:** `sched_yield` only if MicroPython's stub truly cannot be userspace-worked-around; plus any I/O completeness bug found by the editor port. Named fix rule applies.

**Known unknowns & pivot triggers:** MicroPython has been "almost ready" for a year. The lurking cause is not known. **First action:** write down *exactly* what fails, in 3 sentences. If you cannot, you do not yet know the problem — you know only the feeling of being stuck. Probe to find out before anything else.

**Closing notes (2026-05-06):**
- MicroPython end-to-end was confirmed 2026-04-29 (`l2_mp_etc` green, REPL interactive).
- Editor shipped 2026-04-30; `:w` persistence and `ls / edit / :w / :q / ls` cycle confirmed today after fixing a deterministic console crash on the second `ls`.
- That crash had a kernel root cause: PI24's `MAP_SHARE_PHYS` shared physical frames between VFS's ELF cache and consumer address spaces *without refcounting them in `frame_registry`*. Disabled in `vfs/main.rs` as the fastest correctness fix; spawn cost is back to pre-PI24 (~600ms hot, per-segment memcpy). Re-enabling it correctly is the first item in Phase 2 → Phase 3 transition (named userspace failure: today's console wild-jump). Tracked in `memory/project_map_share_phys_uaf.md`.
- virtio-blk modernized 2026-05-07 (branch `virtio-modern`): rebuilt on a reusable `userspace/virtio-core/` crate with virtio 1.0+ modern PCI transport, IRQ-driven completion, BlkSessionClient public IPC. `l2_blk_basic` and `l2_blk_concurrent` green; system boots end-to-end on the modern stack. Writes still go through legacy code (modern `write_bytes` returns NotImplemented); perf floor (≥150 MB/s) deferred — needs T5.7 multi-in-flight at the IPC boundary. The reusable virtio-core is the foundation for Phase 4's virtio-net. Tracked in `memory/project_virtio_blk_modern.md`.

---

### Phase 3 — Resource discipline

**Goal:** the OS can run a workload for an hour without leaking.

**Exit criteria:**

- [ ] `SpaceDestroy` invoke op lands — longest-deferred memory-leak source closed.
- [ ] Userspace `poll()`/`select()` work for pipes, TTYs, and /dev pseudo-files. (Sockets deferred to Phase 4.)
- [ ] Compiler warnings across the tree < 5 total (currently ~30).
- [ ] H9/H10 overflow counters exposed in `/proc` and visible from `top`.
- [ ] Soak test: a shell session with ~1000 repeated `cat | grep | head` pipelines shows bounded memory in `/proc/meminfo` and no orphan processes in `ps`.

**Allowed kernel work:** `SpaceDestroy` invoke op (a pre-planned phase deliverable, not a drift exception) and H9/H10 exposure plumbing. Named fix rule applies for anything else.

**Known unknowns & pivot triggers:** soak test may reveal leaks beyond `SpaceDestroy`. **Pivot trigger:** if a second non-trivial leak surfaces, extend Phase 3 until it is closed; do not skip ahead to "chase Phase 4 momentum." Leaks in a shipping OS are a credibility killer.

---

### Phase 4 — Network

**Goal:** the OS talks to the network.

**Exit criteria:**

- [ ] virtio-net driver attaches, link comes up in QEMU.
- [ ] DHCP client acquires an IP from QEMU's user-mode network.
- [ ] ARP table builds, `ping 8.8.8.8` replies.
- [ ] Userspace BSD-style socket API (`socket`, `bind`, `connect`, `listen`, `accept`, `send`, `recv`) covers TCP and UDP.
- [ ] `wget http://example.com` (or equivalent tiny HTTP/1.1 client) fetches and prints a page.
- [ ] DNS resolution works — simple recursive with hardcoded roots, or via the router's resolver over DHCP.

**Allowed kernel work:** only if the virtio-net driver forces a kernel-side IRQ-delivery fix. Named fix rule applies.

**Known unknowns & pivot triggers:** biggest risk in the whole plan. TCP is genuinely hard. **Pivot trigger:** if after 3 weeks of Phase 4 you do not have DHCP + ping, ship UDP-only and defer TCP to v1.1. Note the pivot decision somewhere durable — do not silently slip.

---

### Phase 5 — Ship

**Goal:** a stranger can run CLUU from a download link and see it work.

**Exit criteria:**

- [ ] `make iso` (or equivalent) produces `cluu.iso` that boots in stock QEMU with a one-line command.
- [ ] `README.md` at repo root: 200 lines max, includes a GIF of login → shell → Python → edit → save → run.
- [ ] Build instructions a Linux user can follow in under 15 minutes, verified by at least one dry-run from a clean checkout.
- [ ] Blog post or GitHub release notes: what CLUU is, what it runs, what it does not, known limits. Honest framing — "hobby OS, two years solo, here is what it does."
- [ ] Posted to /r/osdev or Hacker News. **Posting is the last action** — not "maybe I'll post when I feel good about it."

**Allowed kernel work:** bug fixes surfaced by the clean-checkout dry-run only. No polishing.

**Known unknowns & pivot triggers:** the instinct to keep polishing instead of posting is a drift pattern. **Counter:** a post with a flaw gets feedback. A perfect unposted OS gets nothing. Post.

---

## 6. How to use this doc

**When to re-read ROADMAP.md:**

- Every Monday morning, or whatever you sit down after a break. Five minutes. Start at §3 (The Commitment), skim §4 (Drift Patterns), confirm what phase you are in.
- Any time you notice yourself opening `kernel/src/` during the freeze. If this happens, close the kernel files, re-read §3, then decide.
- Any time a month has passed and it feels like you haven't moved. Read §1 and check what you would change. If you cannot name a capability that moved, the drift patterns are winning.

**When NOT to edit ROADMAP.md:**

- You want to change a phase's exit criteria because the current one is hard. Exit criteria are the contract; editing them to match what you already did is rationalization. If a criterion is genuinely wrong, note *why* in the commit message and have the change be obviously small.
- You want to rewrite the Drift Patterns section because it feels finger-pointy. It is supposed to. Leave it.
- You want to add Phase 6. Don't. If you reach Phase 5, this document's job is done. Start a new one then.

**When to edit ROADMAP.md:**

- A phase completed — tick its header to **✅ DONE** with the completion date. Nothing else.
- The freeze expired (~2026-10-21) and you have decided whether to extend, end, or revisit. Update §3.
- A genuinely-new drift pattern cost you a week. Add it to §4 *after* the week is over, not during.

**Companion practice:** keep a separate (non-public) tactical note for what you are doing this week, which exit criteria are already ticked, current blockers, deferred ideas. That note changes often. This roadmap doesn't.
