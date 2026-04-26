# r/osdev Post Draft

> Pre-post Show & Tell text. Copy from the `--- BEGIN POST ---` line to
> `--- END POST ---` and paste into r/osdev. Replace `<repo-url>` with
> the actual GitHub URL and `<gif-url>` with your uploaded GIF/asciinema
> link before publishing.
>
> Tone: honest, technical, no marketing. r/osdev will eat performative
> copy alive — they want to see real work and real limitations.

---

## Title options (pick one)

1. **Show: CLUU — a hobby microkernel where mkdir, rm, and cp each ship as their own capability-scoped container**
2. **18 months solo on a Rust microkernel: capability IPC + a Cluufile per binary**
3. **CLUU (pre-v1): seL4-style microkernel where every userspace binary is a declared container**

My pick: option 1 — the most concrete, leads with a thing readers can mentally run.

---

## Tags / Flair (pick what r/osdev uses)

- "Showcase" or "Project" flair (whichever exists this week)
- Crosspost-friendly: also OK to share on r/rust afterwards if engagement is good

---

## --- BEGIN POST ---

**TL;DR.** I've spent ~18 months solo building **CLUU**, a Rust microkernel + minimal POSIX userspace. The kernel is seL4-inspired (capability tokens, ~1.2-1.6k cycles for a full call/reply round-trip), and the distinctive bit is that **every userspace binary — including `mkdir`, `rm`, `cp`, `mv`, the shell itself — runs as its own container with a declarative `Cluufile` manifest**. Posting now (pre-v1) to break my own feedback drought rather than wait for "perfect."

**What's distinctive.** A `Cluufile` is Dockerfile-shaped but scoped to a single binary:

```
FROM minimal
PROFILE ipc vfs registry
MOUNT /tmp inherit
BUILD "cargo build ..." target/.../rm.elf /bin/rm
ENTRYPOINT /bin/rm
```

The profile is a capability bitmask (IPC, VFS, REGISTRY, ADMIN, DEVICE, SUPERVISOR). Mount policy controls how the container's `/tmp`, `/log`, etc. interact with its parent's view — for example shells declare `MOUNT /tmp private`, so `spawn mkdir /tmp/x; spawn rm -r /tmp/x` actually works (the spawned containers inherit shell's `/tmp` MemFs by default). No new kernel syscalls were added for any of this; it's all userspace policy on top of capability invoke ops.

**What works (you can boot the ISO and try):**

- Login + multi-user (TPM-hashed `/etc/users.toml`).
- DIY shell: `cd`, `pwd`, `ls`, `cat`, `echo`, `touch`, `ps`, `top`, `spawn`, `jobs`, `fg`/`bg`, `kill`, `sudo`, `su`, ↑/↓ history.
- `/bin/mkdir`, `/bin/rm -r`, `/bin/cp`, `/bin/mv` — each its own container.
- A live `/proc` filesystem (per-PID `stat`/`status`/`cmdline`); `top` reads it.
- Two virtual terminals (Alt-F1/F2), TTY scrollback, graceful shutdown (Ctrl-Alt-Del).
- **Framebuffer-rendered console.** Text is drawn into the GPU framebuffer, not legacy VGA text mode. Userspace programs can `framebuffer_acquire()` to grab the FB and write raw pixels — the primitive is there. There's no compositor / window manager yet; that's v2 work.
- **MicroPython** runs, executes scripts, reads files (caveats below).
- A POSIX-ish C runtime (custom-patched newlib targeting `x86_64-cluu-elf`) — C programs build with the standard toolchain and use stdio/malloc/pthreads/signals.

**What does NOT work yet (honest list):**

- No pipes (`cat | grep` is parsed but not executed as a real pipeline).
- No redirection (`>`, `>>`, `<`).
- No tab completion. Arrow keys do history (↑/↓) but ←/→ inside a typed line do nothing yet.
- MicroPython REPL line editing is missing, no sockets, no threads, heap limits are tight.
- No editor that runs (`kilo` port is queued for Phase 2).
- **No network at all** — no driver, no socket layer, nothing.

**How to boot.** Tested on Debian 12 / Ubuntu 22.04 with KVM. Build instructions in [README.md](<repo-url>/blob/master/README.md). Roughly:

```
cargo xtask build && cargo xtask run
```

Default login is `admin` / `admin`. Then try `cat /etc/welcome.txt` for a short on-screen tour.

**Demo:** <gif-url> (a 90-second walk through login → ls → `cat /etc/architecture.txt` → mount-policy demo → MicroPython one-liner).

**Repo:** <repo-url>

**Why I'm posting this pre-v1.** Solo work has no built-in feedback loop. The kernel was internally audited at 9/10 and is now frozen until userspace catches up; the userspace is at maybe 3/10 because every interesting feature is half-built. I caught myself re-auditing instead of shipping. This is the antidote — get it in front of people who've been here before.

**What I'd love feedback on:**

1. Does the Cluufile + container-per-binary model feel like a useful primitive, or am I overconstraining myself?
2. Anyone with experience porting `kilo` (or another small TUI editor) to a non-Linux POSIX-ish target — gotchas worth knowing?
3. The kernel freeze: I committed to no kernel work for ~6 months unless a userspace test forces it. Sustainable, or am I going to regret it?

**The roadmap is in [docs/ROADMAP.md](<repo-url>/blob/master/docs/ROADMAP.md)**, the honest self-assessment that drove the freeze decision is in [docs/HONEST_ASSESSMENT_2026_04_25.md](<repo-url>/blob/master/docs/HONEST_ASSESSMENT_2026_04_25.md). Both worth a skim if the project itself piques you.

Happy to answer questions, accept critique, or commiserate.

## --- END POST ---

---

## Pre-publish checklist

Before clicking submit:

- [ ] Repo is public on GitHub (or wherever) — confirm `<repo-url>` resolves anonymously.
- [ ] Latest `develop` is pushed and visible.
- [ ] README links inside the post all resolve from a fresh visit (no relative-path 404s — GitHub auto-resolves but double-check).
- [ ] Default `admin` / `admin` password works on a fresh boot (build a clean image, `cargo xtask run`, login).
- [ ] Demo GIF/asciinema is uploaded and the link is public.
- [ ] License file (LICENSE / MIT) is at repo root.
- [ ] No leftover WIP commits, no `git status` dirty files on `develop`.
- [ ] CLAUDE.md / GEMINI.md / agent-only files: those don't matter for the post, but check there's nothing embarrassing in plain sight.
- [ ] Replace `admin` / `admin` with `cluu` / `cluu` if the default password feels too wide-open. Or at least put a "change this before exposing it on the public internet" note in /etc/users.toml.

## After posting

- Tab open r/osdev and your repo's Issues page.
- First 24-48 hours is when comments arrive. Reply within a few hours where reasonable — engagement compounds.
- If someone reports a real bug, file an issue and acknowledge in the thread. Don't disappear into a multi-day fix during the comment window — note it for later.
- Resist the urge to immediately add features people suggest. Capture as issues; ship Phase 1 first.

## If the post falls flat

Sometimes Reddit just doesn't bite. That's fine. Cross-post to r/rust 24-48h later with the same body but a Rust-flavored title ("18 months building a Rust microkernel — Cluufile container model"). Different audience, same content.

If r/rust also doesn't bite: write a Hacker News submission with a different angle (focus on the *capability* aspect, since HN engages with security primitives). Don't write three new posts; reuse the body.
