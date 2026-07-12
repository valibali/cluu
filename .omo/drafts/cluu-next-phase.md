---
slug: cluu-next-phase
status: drafting
intent: clear
pending-action: write .omo/plans/cluu-next-phase.md
approach: Phase 3 finish (poll/select) → Phase 5 networking (smoltcp) → terminal fixes (256-color, alt-screen, CSI cap) → libtui v0 (scoped TUI framework) → scriptable editor (MicroPython plugins, emacs/nvim philosophy) → app ecosystem
---

# Draft: cluu-next-phase

## Components (topology ledger)

| id | outcome (one line) | status | evidence path |
|---|---|---|---|
| C1 | Phase 3: poll()/select() for pipes, TTYs, /dev pseudo-files | active | roadmap.md:202-209 |
| C2 | Phase 5: virtio-net driver + netd (smoltcp) + socket API + wget + DNS | active | roadmap.md:299-321, virtio-core exists |
| C3 | Terminal fixes: 256-color SGR, alt-screen, CSI param cap lift, bold/underline/reverse rendering | active | state.rs:13 (4-param cap), render.rs:26-106 (16-color), grep: zero alt-screen |
| C4 | libtui v0: Elm MVU runtime, diff renderer, input decoder, 3 components (viewport, textinput, list), minimal styling | active | edit/src/ has private primitives, no shared framework |
| C5 | Scriptable editor: upgrade edit → MicroPython plugin host with scoped capabilities | active | edit/ 3.8k LOC, MicroPython runs as container, capability model §2/§3 |
| C6 | App ecosystem: 15-20 usable userspace apps across shell/network/dev/TUI/system tiers | active | roadmap.md:201-206 lists 27 utils, no "real" apps |

## Open assumptions (announced defaults)

| assumption | adopted default | rationale | reversible? |
|---|---|---|---|
| TCP stack | smoltcp (no_std Rust) | user agreed in analysis; DIY TCP is 12+ wk vs 4-7 wk | yes — swap library |
| Terminal fixes timing | Fold into libtui v0 workstream (C4), not separate | same person touches same files; prerequisite | yes |
| Phase 3 scope | exactly as roadmap.md:202: "pipes, TTYs, /dev pseudo-files. Sockets deferred to Phase 5" | already defined | no |
| Testing strategy | Extend existing Python harness + rustc --test for pure logic; manual visual QA for TUI | existing pattern works; screenshot testing is a rabbit hole | yes |
| Kernel freeze | respected; only IRQ-line dispatch fix for virtio-net (pre-authorized roadmap.md:315) | freeze active through ~2026-10-21 | no |
| smoltcp integration | as userspace netd service, not kernel | kernel knows threads+caps+IPC only (§1); network is userspace | yes |

## Findings (cited - path:lines)

- **virtio-core exists**: `userspace/virtio-core/` ~863 LOC, device-class-neutral, proven by virtio-blk. Driver template.
- **Socket-fd precedent**: pipes = rights-scoped IPC endpoints in libcluu `posix/pipe.rs`. Sockets copy pattern.
- **ANSI parser limits**: `libcluu/src/ansi/state.rs:13` — `params: [u16; 4]`, 16-color only. Truecolor `38;2;r;g;b` needs 5 params.
- **No alt-screen**: grep-wide zero matches for `1049|AltScreen|alt_screen` in userspace.
- **cluuterm renders 16 colors**: `cluuterm/src/render.rs:26-106` — quantizes to PALETTE_16, advertises TERM=xterm-256color.
- **Bold/underline/reverse silently consumed**: `render.rs:156` packs attrs=0.
- **Mouse not forwarded to apps**: `compositor/src/window_mgr.rs:533-591` — compositor-internal only.
- **edit is 3.8k LOC**: piece-table, vim keymap, raw-mode, CSI output. Good seed for scriptable editor.
- **MicroPython runs as container**: proven, reads/writes files, REPL works.
- **Kernel non-preemptible invariant**: `gotchas/cluu/cluu-kernel-non-preemptible-invariant.md` — single-CPU, structural.
- **Phase 3 unfinished**: roadmap.md:202-209 — poll/select NOT done, warnings ~30, soak test not run.
- **Elf loader static-only**: `kernel/src/elf.rs` — no PT_DYNAMIC, no PLT/GOT. Dynamic linking rejected for v1.
- **QEMU -netdev user passthrough**: `python/cluu_harness/qemu.py:204` — QEMU_EXTRA_ARGS works, zero harness changes.

## Decisions (with rationale)

- **smoltcp over DIY TCP**: user values finishing; smoltcp is battle-tested no_std. Keep 3-week pivot trigger (→ UDP-only) anyway.
- **Reject dynamic linking for v1**: 8.4 MB total binaries, no memory pressure, contradicts capability model. Static = design.
- **Scriptable editor as linchpin**: simultaneously validates libtui (first consumer) + scoped MicroPython plugins (first host). If this ships, everything after is downhill.
- **libtui v0 scoped, not bubbletea-class**: 3-5 wk core, not 10-16 wk full parity. Rule: first consumer (editor) must exist before the crate does.

## Scope IN

- Phase 3: poll()/select() for pipes, TTYs, /dev pseudo-files
- Phase 5: virtio-net driver, netd service (smoltcp), BSD socket API (TCP+UDP), DHCP, ARP, DNS, wget
- Terminal fixes: 256-color SGR, alt-screen buffer, CSI param cap lift, bold/underline/reverse rendering
- libtui v0: Elm MVU runtime, cell-diff renderer, input decoder, viewport+textinput+list components, minimal styling
- Scriptable editor: upgrade edit → MicroPython plugin host, scoped capabilities, piece-table core preserved
- App ecosystem: 15-20 apps (exact list TBD per user fork)

## Scope OUT (Must NOT have)

- Dynamic linking / shared libraries / dlopen (rejected for v1)
- SMP / multi-CPU (post-v1, 2027)
- GUI / compositor windowing beyond existing TUI (post-2026)
- Full bubbletea parity (Kitty keyboard protocol, OSC52 clipboard, glamour markdown, tea.Exec subprocess, snapshot testing)
- Mouse-to-apps delivery (deferred sub-project per cluu-mouse-minimal-then-full decision)
- New kernel syscalls (use InvokeOp on existing path per §2)
- Runtime ACL / permission checks (capability-scoped at spawn per §3)
- Network filesystem (NFS/SMB)
- Kernel preemption changes (non-preemptible invariant holds)

## Open questions (RESOLVED)

1. **libtui rendering target** → **Hybrid: ANSI first, SHM later**. libtui v0 targets ANSI/CSI through cluuterm/pts. Portable, editor upgrade incremental. SHM backend deferred to post-v0.
2. **MicroPython plugin scoping** → **Child process + pipe IPC, editor brokers**. One MicroPython child per editor session. Editor exposes scoped IPC API. Protocol IS the scope. No runtime ACL (§3 honored).
3. **App list and priority** → **Tiered: essentials → network → dev → TUI → system**. ~20 apps across 5 tiers, sequenced by dependency on networking/libtui.

## Approval gate
status: awaiting-approval
pending-action: write .omo/plans/cluu-next-phase.md
approach: 6-component plan (C1-C6). Phase 3 poll/select → Phase 5 networking (smoltcp) → terminal fixes (256-color, alt-screen, CSI cap, bold/underline/reverse) → libtui v0 (ANSI-path, Elm runtime, diff renderer, viewport+textinput+list) → scriptable editor (MicroPython child process, pipe IPC, editor-brokered scoped API) → 20-app ecosystem (5 tiers: essentials, network, dev, TUI, system). Kernel freeze respected. No dynamic linking. No new syscalls.
