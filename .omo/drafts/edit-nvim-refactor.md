# Draft: edit-nvim-refactor

## Status: approved
## Pending action: write .omo/plans/edit-nvim-refactor.md

## Findings

### Issue 1: Keyboard layout
- `userspace/kbd/src/layout.rs` only has `UsLayout` — no Hungarian layout
- USB-HID sends scancodes → kbd translates via UsLayout → cluuterm forwards ASCII to PTS
- Arrow keys: should work (ANSI escape sequences), but may be broken by cluuterm input forwarding
- Fix: Add `HuLayout` to kbd/src/layout.rs, make it selectable

### Issue 2: Slow load (6.7s)
- `EditModel::init()` calls `PluginRegistry::load_all()` which spawns 4 MicroPython children serially
- First MicroPython ELF load = 5s (7.3MB binary from disk, uncached)
- Subsequent loads = 0.4s each (cached)
- ALL plugins fail with `InvalidArgument` — edit-plugin container spawn is broken
- Fix: Remove plugin loading from init(). Load lazily after first render.

### Issue 3: Double 'i' + resize status bar
- `KeyEvent::Char('i')` in normal.rs:99 sets mode to Insert — should work with one press
- Likely cause: keyboard layout issue (Issue 1) means 'i' key doesn't produce 'i' ASCII
- Resize: diff renderer skips unchanged cells. After resize, prev_buffer=0x0, empty cells compared against Cell::new(' ') match → skipped. Old screen content (white bg from status bar) stays visible.
- Fix: clear screen + reset prev_buffer when viewport dimensions change

### Issue 4: White background on text
- Status bar uses `bg(COLOR_WHITE)` (intentional)
- Content area uses `Cell::new()` with `bg=COLOR_DEFAULT=0` (no bg)
- After resize, old status bar cells (white bg) stay on screen because diff renderer skips matching cells
- Fix: same as Issue 3 — clear screen on resize

### Issue 5: Ctrl+C not cleaning up
- `enter_raw()` in `libcluu/src/posix/tty.rs` clears ICANON + ECHO but NOT ISIG
- Ctrl+C (0x03) generates SIGINT → process killed → cleanup() never runs
- Alt screen never exited, TTY never restored → shell broken, white screen
- Fix: also clear ISIG flag in enter_raw()

### Issue 6: edit-plugin spawn fails with InvalidArgument
- `plugin.rs:try_load()` spawns with `FdSource::EndpointCap` for fd 0/1
- `edit-plugin` Cluufile has `PROFILE ipc` only — no `spawn` capability
- The spawn likely fails because the envelope is malformed or the container lacks capabilities
- Fix: investigate spawn path, or remove plugin system entirely for now

## Approach
1. Fix enter_raw() to clear ISIG (fixes Ctrl+C)
2. Remove plugin loading from init() (fixes slow load)
3. Add viewport change detection in Program::run() — clear screen + reset prev_buffer on dimension change (fixes white bg + resize status)
4. Add Hungarian keyboard layout to kbd (fixes keyboard issues)
5. Fix arrow key forwarding in cluuterm (if broken)
6. Clean up edit code: remove dead plugin code, simplify init()
