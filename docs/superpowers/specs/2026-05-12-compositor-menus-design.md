# Compositor Menus + App Registry — Design Spec

**Date:** 2026-05-12
**Status:** Draft, pre-plan
**Supersedes:** Status bar from `docs/superpowers/specs/2026-05-10-tui-compositor-design.md` §9 → replaced.
**Sibling specs:** `2026-05-10-tui-compositor-design.md` (base), `2026-05-12-login-flow-design.md`.

## 1. Goal

Replace the compositor's static status bar with a Mac-style menu bar:

- Leftmost: a system menu (Apple-menu equivalent) that opens on F1.
- Rest of row 0: menu items declared by the currently focused window.
- Menu items are hierarchical (submenu support, arbitrary depth, but v1 caps at 3 levels).
- Keyboard navigation only (Mouse deferred to compositor sub-project C).
- The system menu's `Apps→` submenu is populated from a build-time-generated app manifest, sourced from each binary's `Cluufile` via a new `APP` directive.

Net effect: the compositor row 0 stops being a clock+window-count strip and becomes the OS's primary discoverability surface.

## 2. Non-goals

- Mouse interaction (deferred until sub-project C).
- Themed/coloured menus beyond focus highlight.
- Per-app menu i18n; English-only v1.
- Dynamic menu items (e.g., recently-opened files). Items are static per app.
- Pinned favourites, search-in-menu.
- Animations.

## 3. Constraints

- Cell-grid only; menus are rows of cells, no pixel-level fancy.
- Compositor freeze on protocol additions stays soft: a new IPC label is fine,
  but the existing WIN_REGISTER reply layout must stay backwards-compatible.
- Menu rendering must not slow `b_compositor_blit` beyond its current ratchet
  (re-bench in test plan).
- App manifest is read by compositor exactly once at startup. No live reload v1.

## 4. Menu data model

```rust
struct Menu {
    label: ArrayString<31>,     // shown in bar or submenu row
    accel: Option<u32>,         // keycode for "underlined letter"
    children: MenuKind,
}

enum MenuKind {
    Leaf { action: MenuAction },
    Submenu { entries: Vec<Menu> },
    Separator,                  // renders as a horizontal rule in a submenu
}

enum MenuAction {
    System(SystemAction),       // hardcoded set
    SpawnApp { container: String },  // procmgr spawn by container name
    WindowCommand { cmd_id: u32 },   // forwarded to focused window via INPUT_FORWARD kind=menu
}

enum SystemAction {
    About,
    Lock,                       // future; v1 no-op
    Reboot,
    Poweroff,
}
```

The Vec inside `Submenu` is loaded at startup (system menu) or at WIN_REGISTER
time (window menu). Both are immutable once set in v1.

## 5. Row 0 layout

```
[CLUU]  [Window-Label]  [File]  [View]  ...                     [12:34:56]
   ^         ^             ^      ^                                ^
system    focused-window's top-level entries                    clock
```

- The system menu's label is always `CLUU` (4 cells + 2 padding = 6 cells).
- Focused window's top-level entries follow, each padded by 1 cell of gap.
- Clock is right-aligned, fixed 8 cells (`HH:MM:SS` + space).
- Between last menu and clock: empty.
- When no window is focused, only `CLUU` + clock appear.

Row 0 is not part of the window-placement area (matches base compositor spec).

## 6. Keyboard interaction

Global hotkeys (compositor consumes, never forwards):

| Chord            | Action                                                          |
|------------------|-----------------------------------------------------------------|
| `F1`             | Toggle system menu (open / close).                              |
| `F10`            | Open focused-window's first menu entry. (Standard X11 convention.) |
| `Esc`            | Close any open menu.                                            |
| `←` / `→`        | Move horizontally between top-level entries (when a menu is open). |
| `↑` / `↓`        | Move vertically within a submenu.                               |
| `Enter`          | Activate highlighted entry (leaf → run action; submenu → open). |

When no menu is open, all keys forward to the focused window as today.

When a menu is open, no key event reaches windows. The compositor handles
every key locally until the menu closes (action fires OR Esc).

## 7. System menu (hardcoded)

```
CLUU
├── About                       (System(About))
├── ────                        (Separator)
├── Lock                        (System(Lock))            v1: no-op, displays "TODO"
├── Apps                        (Submenu)
│   ├── Terminal                (SpawnApp "cluuterm")
│   ├── Editor                  (SpawnApp "edit")
│   ├── MicroPython             (SpawnApp "mp")
│   └── …                       (built from app manifest, sorted alphabetically)
├── ────                        (Separator)
├── Reboot                      (System(Reboot))
└── Poweroff                    (System(Poweroff))
```

The `Apps` submenu is populated at compositor startup from `/var/manifests/apps.toml` (§9). Entries before/after `Apps` are hardcoded.

### About dialog (v1)

A leaf `About` action opens a 40×8 modal window owned by the compositor (no client, drawn directly). Shows:

```
CLUU
build: <git short hash>
kernel: <build date>

Press Esc to close.
```

Modal eats Esc to dismiss.

## 8. Focused-window menu

Window declares its menu tree at `WIN_REGISTER` time via a new IPC:

### `WIN_MENU_SET` (app → compositor)

- Sent right after the WIN_REGISTER reply, before the first WIN_DAMAGE.
- Payload: serialised menu tree (CBOR-like or hand-rolled — pick at plan).
- Maximum tree depth: 3.
- Maximum total entries (incl. submenus): 64.
- Maximum payload size: 4 KiB (1 page).
- Reply: `{ ok: u32, error: u32 }`. On invalid tree (oversize / too deep / malformed) → no menu set, return error; window still renders.

### Action dispatch

- `SpawnApp` → compositor IPCs procmgr `PROCMGR_CONTAINER_RUN_LABEL` with the
  container name. Same path the existing `Super+N` hotkey uses.
- `WindowCommand { cmd_id }` → compositor sends new `INPUT_FORWARD` variant with
  `kind = menu, codepoint = cmd_id` to the focused window's input endpoint.
  Window interprets `cmd_id` per its own internal menu definition.
- `System(...)` → compositor handles internally.

### Focus change

When focus moves to a different window, compositor replaces row 0's
window-menu portion with the new focused window's declared menu. If that
window never sent `WIN_MENU_SET`, only `CLUU` + clock are shown.

When focus returns to no window (last destroyed), same.

## 9. App manifest — `Cluufile APP` directive

### Cluufile extension

Add a new directive valid in any container's Cluufile:

```
APP <category-path> "<label>"
```

Examples:

```
# containers/cluuterm/Cluufile
APP Apps "Terminal"

# containers/edit/Cluufile
APP Apps "Editor"

# containers/top/Cluufile
APP Apps/System "Process Monitor"
```

Semantics:

- `<category-path>` is a slash-separated path. The first segment (`Apps`)
  matches a top-level submenu in the system menu. Future segments allow
  nested submenus.
- `<label>` is the leaf entry text shown in the menu.
- If a Cluufile declares no `APP`, the binary is not user-facing and does
  not appear in any menu (this is the default, matching today's
  infrastructure containers).
- The container name (Cluufile's directory under `containers/`) becomes the
  `SpawnApp.container` argument.
- A Cluufile may declare at most one `APP` directive in v1. Multi-launch is
  out of scope.

### Build pipeline

`xtask` already walks `containers/*/Cluufile`. Extend it to:

1. Parse every Cluufile for `APP`.
2. Collect `{container, category, label}` tuples.
3. Sort by `(category, label)`.
4. Emit `target/initrd/var/manifests/apps.toml`:

```toml
# Auto-generated by xtask. Do not edit.

[[app]]
container = "cluuterm"
category  = "Apps"
label     = "Terminal"

[[app]]
container = "edit"
category  = "Apps"
label     = "Editor"

[[app]]
container = "top"
category  = "Apps/System"
label     = "Process Monitor"
```

Compositor reads `/var/manifests/apps.toml` at boot (via VFS open from the
already-mounted ext2/initrd). Stores entries in memory; builds the
hierarchical submenu from category paths.

### "Install"-time semantics

Mentally: build = install. A Cluufile's APP directive is the install
manifest; rebuilding the initrd is the equivalent of `apt install`. Future
work (post-freeze): a true `pkgadd` that registers a Cluufile post-boot.
Out of scope for v1; the build-time pipeline is sufficient.

## 10. Rendering details

### Row 0 idle (no menu open)

Compose: render `[CLUU]` left, focused window's top-level entries from
column 7 right, clock right-aligned. Background = `palette[0]` (default
black), foreground = `palette[7]` (white). On focus highlight (menu open),
highlighted entry inverts fg/bg.

### Open submenu

Compositor draws a rectangular cell region starting at row 1, anchored to
the top-level entry's x column. Width = max(child label widths) + 4 (for
chrome). Height = number of children. Borders use the Tier-3 corner
glyphs already in the compositor.

Submenu cells overlap window cells; this is the first case where
non-window content overlays window content. Compositor's compose pipeline
already supports priority layering (status bar wins row 0); extend the
same priority arbitration to a "menu overlay" layer above all windows.
Closing the menu marks the covered region dirty and lets normal compose
restore window cells.

### Highlighting

The currently-highlighted entry uses palette inversion (fg ↔ bg). No
animation, no underline.

## 11. IPC summary

| Label                              | Direction       | Status        |
|------------------------------------|-----------------|---------------|
| `WIN_MENU_SET`                     | client→comp     | new           |
| `INPUT_FORWARD kind=menu`          | comp→client     | extend existing label with a new kind value |
| `PROCMGR_CONTAINER_RUN_LABEL`      | comp→procmgr    | reused        |
| `PROCMGR_SHUTDOWN_LABEL`           | comp→procmgr    | reused (Reboot/Poweroff dispatch) |

## 12. Test plan

L1 (unit):
- Cluufile `APP` directive parser (valid / invalid / missing / multiple).
- xtask manifest emitter: feed 3 fixture Cluufiles, expect deterministic sorted output.
- Menu tree validator: depth>3 rejected, oversize rejected, malformed rejected.
- Hotkey table: F1 toggles open/close idempotently.

L2 (harness):
- `l2_menu_open_close` — boot, send F1, expect compositor log `menu: system opened`, send Esc, expect `menu: system closed`.
- `l2_menu_apps_lists_cluuterm` — boot, F1, ↓ to Apps, →, assert child entries include "Terminal" line in compositor's debug print of the open submenu.
- `l2_menu_spawns_app` — F1, navigate to Apps→Terminal, Enter, expect new cluuterm window registers within 2 s.
- `l2_menu_focused_window` — register a window that sends WIN_MENU_SET with one entry "Custom"; assert "Custom" appears in row 0 once focused.
- `l2_menu_reboot` — F1, ↓ to Reboot, Enter, expect `procmgr: reboot requested` marker (don't actually reboot in harness — use a stub).

Bench:
- `b_compositor_blit` re-run; menu code must not push cycles_per_full_screen past 1.5× the post-atlas baseline (matches base spec gate).

## 13. Files touched

New:
- `userspace/compositor/src/menu.rs` — Menu types, render, hotkey state machine.
- `xtask/src/apps_manifest.rs` — Cluufile APP parser + emitter.
- `target/initrd/var/manifests/apps.toml` (build output, gitignored).
- `docs/superpowers/plans/2026-05-…-compositor-menus.md` (later).

Modified:
- `userspace/compositor/src/main.rs` — boot-time apps.toml load, menu state hookup.
- `userspace/compositor/src/state.rs` — `Window.menu: Option<Menu>` field.
- `userspace/compositor/src/protocol.rs` — `WIN_MENU_SET` parse/encode.
- `userspace/libcluu/src/ipc.rs` — `WIN_MENU_SET` label constant + new `INPUT_FORWARD kind=menu` enum value.
- `xtask/src/main.rs` — call into `apps_manifest` after the existing Cluufile walk.
- All user-facing containers' Cluufiles (start with `containers/cluuterm/Cluufile`) — add `APP` directive.
- `docs/superpowers/specs/2026-05-10-tui-compositor-design.md` §9 status bar — mark superseded, link to this spec.

## 14. Implementation order

This whole feature lands AFTER the login-flow chunks 1–7 are confirmed
green (per user direction). Internal order, when it lands:

1. xtask `APP` parser + apps.toml emitter (no compositor changes; verify
   the manifest is correct in initrd).
2. `WIN_MENU_SET` IPC + Window.menu plumbing (no rendering; verify recv
   path via debug print).
3. Menu render + F1 hotkey + system menu (no Apps spawn — just navigation).
4. SpawnApp action (Apps submenu starts launching).
5. WindowCommand action (focused-window menus drive INPUT_FORWARD kind=menu).
6. About dialog (compositor-owned modal).

Each step ships its own per-step plan.

## 15. Open questions

- Submenu rendering when the submenu would extend past screen edge:
  clamp left, or open upward? v1: clamp right edge to last column.
- Sub-3-character labels for top-level entries (e.g., "OK") — keep
  1-cell gap or 2? v1: always 2 cells.
- Whether `WIN_MENU_SET` can be re-sent (e.g., when a window's menu
  changes). v1: rejected after first call, returns error. Multi-set =
  later.
- Menu accelerator keys (Alt+letter) — visual underline of accel char
  in label. v1: parse the `&` convention (`F&ile` → File w/ "i"
  underlined), no underline rendering yet, key dispatch only.

## 16. References

- Base compositor spec: `docs/superpowers/specs/2026-05-10-tui-compositor-design.md`.
- Login-flow spec: `docs/superpowers/specs/2026-05-12-login-flow-design.md`.
- Cluufile schema (de-facto, no formal doc yet): see any `containers/*/Cluufile`.
- procmgr container-run path: `userspace/procmgr/src/main.rs` PROCMGR_CONTAINER_RUN_LABEL handler.
