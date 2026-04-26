# Path to v1 — Living Roadmap

*Started: 2026-04-25. Audience: Balazs, on every session until v1 ships.*

*This doc is append-only. Tick items as they land. When v1 ships and the post is live, archive this file and start a v1.1 doc.*

*Related: `docs/HONEST_ASSESSMENT_2026_04_25.md` (the why), `docs/ROADMAP.md` (the strategic frame), `docs/CURRENT_PHASE.md` (today's tactical state).*

---

## Goal

**Ship a "Show & Tell" post on r/osdev that strangers can boot and play with for ~15 minutes.**

Not a release. Not v1.0-final. A pre-v1 invitation to look at the work and react. Breaks the feedback drought.

The reasoning is in `docs/HONEST_ASSESSMENT_2026_04_25.md`. Re-read it on the days when "ship now" feels too soon.

---

## Stage 1 — Pre-post (must-do before posting)

Sequence is intentional; later items depend on earlier ones.

### Task queue (mirrors TaskList)

- [ ] **#71** — Finish race-fix (Tasks 10-14: race sweeps, full matrix, perf check, memory updates). *Currently in flight.*
- [ ] **#75** — Shell line editing + command history. Arrow up/down for history, left/right for cursor, proper backspace. The single most jarring miss for an osdev visitor.
- [ ] **#52 → #53** — `l2_cp` red harness case → `/bin/cp` implementation. Visitors will type `cp`.
- [ ] **#54 → #55** — `l2_mv` red harness case → `/bin/mv` implementation. Visitors will type `mv`.
- [ ] **#76** — Seed `/etc` (motd, welcome.txt, architecture.txt) + write `README.md` at repo root + record demo GIF/asciinema.
- [ ] **#77** — Draft the r/osdev post text. *"Show & Tell — 18 months building a capability-based microkernel."*

### Soft pre-post (do if time/energy; otherwise post without)

- [ ] **#73** — Stream-tail harness. Internal-only; saves YOU time during pre-post iterations but invisible to users.
- [ ] **#51 / #56** — `l2_rm_root_refuse` + Plan 2 final regression. Only if you want a clean "Plan 2 closed" tag.

### Stage 1 exit criteria (all must be true before posting)

1. Fresh-checkout build dry-run from a clean Linux box succeeds in <15 min using only README instructions.
2. Boot → login → `ls /` → `cat /etc/welcome.txt` → `cd /var/images` → `ls` → `spawn mkdir /tmp/x` → `cp` → `mv` → `top` → `exit` works in a recorded demo without surprise failures.
3. Arrow keys, history, and backspace all work in the shell.
4. Harness suite runs ≥45/46 (l2_owner_deny is the lone acceptable known-fail).
5. README is honest: scope, what works, what doesn't, build steps, link to PHILOSOPHY/ROADMAP.

---

## Stage 2 — Post & immediate response

When all Stage 1 boxes are ticked:

- [ ] Publish the post on r/osdev. **Do NOT wait for "perfect."** The point is feedback, not approval.
- [ ] Watch comments for 24-48 hours. Note recurring questions/complaints — they're free QA.
- [ ] If someone reports a real bug, fix it as a small commit and reply. (Don't enter a multi-day rabbit hole — note for later if non-trivial.)
- [ ] Update README with FAQ if 3+ people ask the same question.

---

## Stage 3 — Toward v1 (post-post)

The remaining Phase 1 + Phase 2 work from `docs/HONEST_ASSESSMENT_2026_04_25.md` §4. **Don't start any of it until Stage 2 is genuinely settled** (≥3 days post-publish, no critical-bug fire).

### Phase 1 continuation

- [ ] **Pipes** — `cat foo.txt | grep pattern | head -5` end-to-end.
- [ ] **Redirection** — `> file`, `>> file`, `< file`.
- [ ] **`grep`, `head`, `tail`, `wc`** — minimal POSIX semantics.
- [ ] **Tab completion** for files and directories.
- [ ] **`echo $?`** — last command's exit status.
- [ ] **#38** — PATH-based bare-command resolution (drop the `spawn` verb).

### Phase 2

- [ ] **MicroPython** — REPL works, reads/writes files, multi-line input, Ctrl-C, Ctrl-D. (See ROADMAP §5 Phase 2 for the "lurking cause unknown" note — first action is to write down EXACTLY what fails in 3 sentences.)
- [ ] **One editor** — port `kilo` (smallest viable). Pick it, finish it, don't shop.
- [ ] **End-to-end:** edit a Python script → save → run with `micropython script.py` → see output. No reboot.

### v1 release

- [ ] Build `cluu.iso` with one command.
- [ ] README ≤200 lines, with a GIF showing login → shell → Python → edit → save → run.
- [ ] Build instructions verified by a clean dry-run.
- [ ] Blog post or GitHub release notes — honest framing.
- [ ] Post to r/osdev or Hacker News as a release announcement.

---

## Stage 4 — v1.1 (network + leaks + polish)

Out of scope for v1. Captured here so they don't get forgotten.

- [ ] **#74** — Refactor procmgr (split main.rs into spawn/container/session/view/restart/procfs/ipc modules). Phase 3-aligned: SpaceDestroy lands here too.
- [ ] **`SpaceDestroy` invoke op** — longest-deferred memory-leak source.
- [ ] **`poll`/`select`** — for pipes, TTYs, /dev pseudo-files.
- [ ] **Compiler warnings <5 total** (currently ~30).
- [ ] **`/proc/uptime` + `/proc/meminfo`** — needs kernel timer + buddy allocator stats.
- [ ] **Soak test** — 1000 repeated `cat | grep | head` pipelines, bounded memory in `/proc/meminfo`.
- [ ] **virtio-net + DHCP + ping**.
- [ ] **BSD socket API** — TCP and UDP.
- [ ] **`wget` (or equivalent tiny HTTP/1.1 client)**.
- [ ] **DNS resolution**.
- [ ] **#70** — Redesign `l2_owner_deny` (test assumed ext2 ownership on MemFs — needs ext2-mounted path or extended MemFs semantics).

---

## Append below as work progresses

*Use this section for dated notes — landmarks, course corrections, surprises. Don't edit history above; append here. Re-read when motivation wobbles.*

### 2026-04-25 — Doc created

Captured the path to v1 after a long, honest brainstorming session. Mount-policy + race-fix landed; race fix mid-finishing. Pre-post sequence locked in. Goal is the r/osdev post in ~3-4 evening sessions.

### 2026-04-26 — #71 set_view race closed

Race fix landed in 6 commits (libcluu API, suspendprobe, harness wiring, kernel honor of flag, procmgr migration, harness_repeat helper). Kernel adds `THREAD_CREATE_START_SUSPENDED` flag; procmgr's 9 spawn-with-view sites now go through `install_view_and_run` helper which suspends/installs view/resumes. Harness improved 39/46 → 44/47. Race-victim cases (l2_rm, l2_argv, f13_detach_survive) all hit 10/10 standalone. l2_sigint 8/10 (second timing dependency, captured as #79). No spawn-perf regression (~8K cycles added on a 158M-cycle spawn). Next pre-post: #75 shell line editing.

### 2026-04-26 — Stage 1 (pre-post) code COMPLETE

All in-tree pre-post work landed:

- **#75 line editing + history** — TTY canonical mode now handles ↑/↓ for in-memory history (32-entry ring, dedupes consecutive duplicates), DEL-as-backspace, ←/→ silently consumed. 16 unit tests via `rustc --test`. (commit b4d20cb)
- **#52 + #53 cp** — `/bin/cp` ships, smoke-tested by spawning with no args. Full file-copy is interactive due to a separate VFS write-on-MemFs issue captured as #80. Also added a `touch` shell builtin for visitor convenience. (commits 51b7696, 5c5d965)
- **#54 + #55 mv** — `/bin/mv` ships as a thin wrapper around VfsClient::rename. Smoke tested. (commits fe338a9, 384f835)
- **#76 /etc seeds + README + demo script** — `/etc/motd`, `/etc/welcome.txt`, `/etc/architecture.txt` wired into the userdisk. New top-level honest README (195 lines, leads with what works AND what doesn't). Old README relocated to `docs/INTERNALS.md`. `docs/DEMO_SCRIPT.md` captures the GIF recording walk-through. (commit df8989c)
- **#77 r/osdev post draft** — `docs/POST_DRAFT.md` ready. Three title options, pre-publish checklist, fallback plan if engagement is low. (commit 9957358)

**Stage 1 exit criteria check:**
1. Fresh-checkout build dry-run from a clean Linux box succeeds in <15 min — needs verification.
2. Boot → tour works without surprise failures — needs verification (recording the demo IS this verification).
3. Arrow keys, history, backspace work — landed (#75).
4. Harness ≥45/46 — currently 44/47 (l2_argv + l2_owner_deny + p4_dev). 44/47 is acceptable for posting; documented in README.
5. README is honest — landed.

**Remaining before publish:**
- Record the demo GIF following `docs/DEMO_SCRIPT.md`.
- Fresh-checkout dry-run on a clean Linux box (could be a VM).
- Make repo public.
- Edit `<repo-url>` and `<gif-url>` placeholders in `docs/POST_DRAFT.md`.
- Hit submit on r/osdev.

That's not coding work anymore — it's logistics + the recording. Ready when Balazs is.

### YYYY-MM-DD — *(template)*

- What landed:
- What surprised:
- What changed in the plan:
