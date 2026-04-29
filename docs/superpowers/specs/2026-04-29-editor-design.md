# `/bin/edit` — vi-flavored modal editor for CLUU

**Status:** Design (2026-04-29)
**Type:** New userspace binary (Rust, `no_std + alloc`)
**Replaces:** Nothing — first interactive editor in the system.
**Prerequisites:** None beyond what already ships (libcluu VFS, raw-mode TTY, kbd driver with arrow-key escape sequences, ANSI CSI in console renderer).

---

## 1. Goal

A vi-flavored modal text editor named `edit`, hand-rolled in Rust `no_std + alloc`, fitting in roughly 3,000 lines of code. Targets day-to-day editing of source code and config files inside CLUU — *not* a full vim replacement, *not* an `ed`-style line editor, but the smallest practical text editor that makes you actually want to use it.

User-visible payoff: typing `edit ~/.shellrc` opens the file, lets you change it, and saves it back via the same VFS path the shell already uses. Closes Phase 2's "Write code in CLUU" loop alongside MicroPython.

---

## 2. Locked design decisions (from brainstorm 2026-04-29)

| # | Decision | Choice |
|---|----------|--------|
| Q1 | Feature ceiling | **Practical** — modes, motion, edit, ex, `:s` substitute, `/` search, undo, visual mode |
| Q2 | Keymap style | **Vim-leaning** — Esc *or* `Ctrl-[`, arrows in NORMAL, `Ctrl-S`/`Ctrl-Q`, Page-Up/Down, Tab indent in INSERT, `*`/`%`, `gd` heuristic |
| Q3 | File size ceiling | **Medium** — up to ~1MB, piece-table buffer |
| Q4 | Visual mode | **Char + line + `gv` + `o`** — no block-visual |
| Q5 | UTF-8 | **Byte-safe** — load any bytes, navigate by codepoints, render non-ASCII as `?` |
| Q6 | Indent | **Smart** — `:set expandtab`/`tabstop`, autoindent, smart-indent for `{` / `:` |
| Q7 | Long lines | **Horizontal scroll default**, `:set wrap` toggles to soft-wrap |
| Q8 | Search | **Vim-comfortable** — `:set ic`, history, `:set hlsearch`, `*`/`#` |
| Q9 | Discovery | **Embedded `:help`** — one-page cheat sheet const string. Bare argv (`edit [file]`), no stdin, no multi-file |

---

## 3. Architecture

### 3.1 Crate shape

Single bin crate at `userspace/edit/`, `no_std + alloc`, no shared library extraction for v1.

```
userspace/edit/
├── Cargo.toml
└── src/
    ├── main.rs        # _start, argv parsing, top-level event loop
    ├── piece.rs       # piece table + edit operations
    ├── undo.rs        # undo/redo stack on top of piece table
    ├── buffer.rs      # buffer abstraction: file path, dirty flag, piece-table owner
    ├── mode.rs        # mode enum + top-level dispatch
    ├── normal.rs      # NORMAL-mode keymap + accumulator
    ├── insert.rs      # INSERT-mode keymap
    ├── visual.rs      # VISUAL-char/line keymaps + selection ops
    ├── op_pending.rs  # OperatorPending mode (after d/c/y/>/<)
    ├── prompt.rs      # ex/search prompt buffer + history
    ├── motion.rs      # h/j/k/l/w/b/e/0/$/gg/G/% + arrow keys + counted motion
    ├── ops.rs         # operators: d/y/c/x/p/r/i/a/o/dd/yy + smart-indent
    ├── search.rs      # /, ?, n, N, *, #, hlsearch, history
    ├── ex.rs          # : prompt, command parser, dispatch
    ├── settings.rs    # :set framework
    ├── render.rs      # frame builder, status line, gutter, hlsearch overlay, scroll math
    ├── input.rs       # raw-mode TTY input, escape-sequence decoder
    ├── help.rs        # const &str cheat sheet, :help command
    └── vfs_io.rs      # load/save via libcluu VFS; atomic write
```

### 3.2 Container packaging

`containers/edit/Cluufile`:

```
FROM minimal
PROFILE ipc vfs
BUILD "cargo build -p edit --target triplets/x86_64-cluu-user.json -Z build-std=core,alloc -Z build-std-features=compiler-builtins-mem" target/x86_64-cluu-user/debug/edit /bin/edit
ENTRYPOINT /bin/edit
```

No `MOUNT` directives — inherits the parent shell's view. Editing a file in `/etc` from a USER envelope will fail to save (envelope says ro:/etc); error surfaces as `:w` returning EACCES with a status-line message. Correct behavior; no special handling.

### 3.3 Boot sequence

1. `_start` (libcluu's existing crt0) calls `main` with argv decoded.
2. Argv parser handles `[-h]` (print usage + exit 0) and one optional file path.
3. Resolve VFS endpoint via registry. Load file if given (else empty buffer). Reject files > 1MB at load with a clear stderr message + exit 1.
4. Switch TTY to raw mode via libcluu's `set_mode` API. Disable line buffering, disable echo. Save the previous mode for restoration on exit.
5. Initial full-screen paint: clear screen (`CSI 2 J`), render frame, place cursor.
6. Event loop: blocking byte reads on stdin → escape-sequence decoder → mode dispatcher → buffer/undo mutation → re-render → loop.
7. On `:q` (clean) / `:q!` / `:wq`: restore TTY mode, free piece table, exit 0. On EOF / parent death: same cleanup + exit 1.

### 3.4 Dependencies

- `libcluu` — VFS client, registry, args, posix shim, TTY mode control.
- Nothing external. No `crossterm`, no `tui-rs`, no `regex`. All hand-rolled.

### 3.5 Top-level data flow

```
keystroke → input::decode → mode::dispatch → ops::apply → piece::insert/delete
                                                ↓                       ↓
                                           undo::record              render::redraw
```

---

## 4. Buffer & undo

### 4.1 Piece table

Two append-only byte buffers + an ordered list of pieces:

```rust
struct Buffer {
    original: Vec<u8>,        // file content as loaded — never mutated
    add:      Vec<u8>,        // append-only, grows on every insert
    pieces:   Vec<Piece>,     // logical content = concatenation of pieces
    line_idx: Vec<usize>,     // cache: byte offsets of line starts
    line_idx_dirty: bool,
    edit_seq: u64,            // incremented on every edit (for cache invalidation)
}

#[derive(Clone, Copy)]
struct Piece {
    source: Source,
    offset: usize,            // byte offset into chosen source
    length: usize,
}

enum Source { Original, Add }
```

Edits never touch text bytes, only the `pieces` list. Insert at logical offset `K`: find which piece covers `K`, split it into two, splice a new `Piece { source: Add, …, length: N }` between them. Delete is the same shape. The `add` buffer is referenced forever — we don't garbage-collect even if a piece is later "deleted", because undo might need it back.

### 4.2 Cursor / line lookup

Logical byte offset is the source of truth. The `line_idx` cache (Vec of byte offsets to line starts) gives O(1) `(line, col) → byte_offset` and O(1) `byte_offset → line` (via binary search). Rebuilt lazily after edits — `line_idx_dirty` flag, regenerate on first read after dirty.

### 4.3 UTF-8 motion (Q5)

- `h`/`l` advance by one *codepoint*, not one byte.
- Validate the slice as UTF-8 via `core::str::from_utf8`. On valid UTF-8: advance by `char_indices` boundaries.
- On invalid byte sequence: advance one byte (so navigation works on any binary-ish file). Render the bad byte as `?`.
- `w`/`b`/`e` operate on word boundaries; word-boundary detection uses a small character-class table (alphanumeric, `_`, ASCII-only for v1).

### 4.4 Undo stack

```rust
struct UndoEntry {
    cursor_before: usize,
    cursor_after:  usize,
    patch: PiecePatch,
}

struct PiecePatch {
    range:    Range<usize>,   // index range in `pieces` that was replaced
    removed:  Vec<Piece>,     // what was there before
    inserted: Vec<Piece>,     // what's there now
}

struct UndoStack {
    entries: Vec<UndoEntry>,
    head:    usize,           // ≤ entries.len(); redo possible if < len
    pending: Option<UndoBuilder>,  // accumulates an INSERT session
}
```

### 4.5 Undo grouping (vim-style coarse, Q1)

- One undo entry per NORMAL-mode command (`x`, `dd`, `dw`, `p`, …).
- One undo entry per INSERT-mode session (i/a/o … Esc collapses all keystrokes between to one entry).
- One undo entry per visual-mode operator (`vjd` is one undo).
- `:s` substitute is one entry, even if it modifies many lines.

### 4.6 Undo / redo operations

- **`u`**: pop entry from `entries[head-1]`, reverse-apply (delete `inserted` from `pieces`, splice `removed` back), `head -= 1`. If `head == 0`, no-op + status message "Already at oldest change".
- **`Ctrl-R`**: if `head < entries.len()`, forward-apply `entries[head]`, `head += 1`. Else "Already at newest change".
- Any new edit after undo truncates `entries` at `head` (drops redo history past current point).

### 4.7 Memory ceiling per session

- `original` ≤ 1MB
- `add` ≤ ~1MB realistic for heavy session (every insert-mode keystroke emits to `add`)
- `pieces`: ~24 bytes each, ~4k pieces worst case → ~100KB
- `undo.entries`: ~100 bytes each (text NOT duplicated, only piece refs), ~10k entries → ~1MB
- `line_idx`: 8 bytes per line, ~30k lines max → ~240KB

Total worst case: ~3.5MB. Within libcluu's per-process heap (multi-MB available).

### 4.8 No persistent undo

Undo lives in memory; closing the file drops it. Vim's `.un~` files are a v2 conversation.

---

## 5. Mode state machine

```rust
enum Mode {
    Normal,
    Insert,
    VisualChar,
    VisualLine,
    OperatorPending(Operator),
    ExPrompt(PromptKind),
}

enum Operator { Delete, Change, Yank, Indent, Dedent }
enum PromptKind { Ex, SearchFwd, SearchBwd }

struct PromptState {
    buf:         String,
    cursor:      usize,         // column within buf
    history_idx: Option<usize>, // for up-arrow recall
}
```

### 5.1 Mode transitions (the directed graph)

```
NORMAL ─── i / a / I / A / o / O ──→ INSERT
NORMAL ─── v ───────────────────────→ VISUAL_CHAR
NORMAL ─── V ───────────────────────→ VISUAL_LINE
NORMAL ─── d / c / y / > / < ──────→ OPERATOR_PENDING
NORMAL ─── : / / / ? ──────────────→ EX_PROMPT
INSERT ─── Esc / Ctrl-[ ───────────→ NORMAL  (commits undo)
VISUAL_* ── Esc / v / V (toggle) ──→ NORMAL
VISUAL_* ── d / y / c / > / < ─────→ NORMAL  (operator applied)
OP_PENDING ─ {motion} ─────────────→ NORMAL  (operator applied)
OP_PENDING ─ {same op letter} ─────→ NORMAL  (line operation: dd, yy)
OP_PENDING ─ Esc ──────────────────→ NORMAL  (cancel)
EX_PROMPT ─ Enter ─────────────────→ NORMAL  (dispatch)
EX_PROMPT ─ Esc ───────────────────→ NORMAL  (cancel)
```

---

## 6. Input decoder

### 6.1 KeyEvent

Raw-mode TTY delivers single bytes. Decoder buffers up to 6 bytes to disambiguate escape sequences. Output is a typed `KeyEvent`:

```rust
enum KeyEvent {
    Char(char),                  // includes Tab, Enter, Backspace as plain bytes
    Ctrl(char),                  // Ctrl-A..Ctrl-Z, Ctrl-[
    Esc,                         // bare Esc (after timeout)
    Arrow(Direction),
    PageUp, PageDown, Home, End,
    Delete,                      // CSI 3~, distinct from Backspace (0x08)
}
```

### 6.2 Esc-vs-escape-sequence problem

Esc starts a CSI sequence (`Esc [ A` for up-arrow). A bare Esc keypress is the user pressing Esc to exit a mode. Disambiguate with a **25ms read timeout**: after reading `0x1B`, attempt a non-blocking read; if a follow-up byte arrives within 25ms, decode as escape sequence; if not, emit `KeyEvent::Esc`.

The kbd driver already does this for the shell's arrow-key handling — copy the pattern via libcluu's existing `read_with_timeout`.

### 6.3 CSI sequences we recognize

| Sequence | KeyEvent |
|----------|----------|
| `CSI A` | Arrow(Up) |
| `CSI B` | Arrow(Down) |
| `CSI C` | Arrow(Right) |
| `CSI D` | Arrow(Left) |
| `CSI 5 ~` | PageUp |
| `CSI 6 ~` | PageDown |
| `CSI 1 ~` or `CSI H` | Home |
| `CSI 4 ~` or `CSI F` | End |
| `CSI 3 ~` | Delete |

Unknown sequences are silently dropped (don't panic on unexpected escape sequences).

---

## 7. Mode dispatch

### 7.1 Top-level

```rust
fn handle(state: &mut Editor, event: KeyEvent) -> StepResult {
    match state.mode {
        Mode::Normal             => normal::handle(state, event),
        Mode::Insert             => insert::handle(state, event),
        Mode::VisualChar         => visual::handle(state, event, Span::Char),
        Mode::VisualLine         => visual::handle(state, event, Span::Line),
        Mode::OperatorPending(op) => op_pending::handle(state, event, op),
        Mode::ExPrompt(kind)     => prompt::handle(state, event, kind),
    }
}

enum StepResult {
    Redraw(DirtyRegion),
    Quit(ExitCode),
    Continue,                    // accumulating count, etc.
}
```

### 7.2 NORMAL — accumulator

A small state machine inside the handler tracks transient context:

```rust
struct NormalAccum {
    count:      Option<usize>,   // numeric prefix: "5"+"j" → count=5
    pending_g:  bool,            // `g` pressed, waiting for `g`/`d`
}
```

Dispatch flow per keystroke:

1. Digit `1-9` (or `0` when count is non-empty): append to count, `Continue`.
2. `0` and count empty: motion `0` (start of line).
3. `g` and not pending_g: set pending_g, `Continue`.
4. pending_g + `g`: `gg` motion.
5. pending_g + `d`: `gd` heuristic def-jump.
6. Else: clear pending_g, look up key in NORMAL keymap.

### 7.3 NORMAL keymap (categories)

**Pure motions** (apply `count` times, redraw):

| Key | Motion |
|-----|--------|
| `h`, `Arrow(Left)` | one codepoint left |
| `l`, `Arrow(Right)` | one codepoint right |
| `j`, `Arrow(Down)` | one file line down |
| `k`, `Arrow(Up)` | one file line up |
| `0` | start of line |
| `$` | end of line |
| `^` | first non-whitespace on line |
| `w` | next word start |
| `b` | previous word start |
| `e` | next word end |
| `gg` | first line |
| `G` | last line |
| `%` | match bracket (`()`, `[]`, `{}`) |
| `Ctrl-F`, `PageDown` | one screen down |
| `Ctrl-B`, `PageUp` | one screen up |
| `Home` | start of line |
| `End` | end of line |

**Operators** (switch to `OperatorPending`):

| Key | Operator |
|-----|----------|
| `d` | Delete |
| `c` | Change |
| `y` | Yank |
| `>` | Indent |
| `<` | Dedent |

**Direct edits** (apply with count, record undo, redraw):

| Key | Action |
|-----|--------|
| `x` | delete char under cursor |
| `i` | enter INSERT at cursor |
| `a` | enter INSERT after cursor |
| `I` | enter INSERT at first non-whitespace |
| `A` | enter INSERT at end of line |
| `o` | new line below, enter INSERT |
| `O` | new line above, enter INSERT |
| `r{c}` | replace cursor char with `c` |
| `p` | paste yank-buffer after cursor |
| `P` | paste yank-buffer before cursor |
| `u` | undo |
| `Ctrl-R` | redo |
| `dd` | delete current line (when in op-pending after first `d`) |
| `yy` | yank current line |

**Mode switches:**

| Key | New mode |
|-----|----------|
| `v` | VisualChar |
| `V` | VisualLine |
| `:` | ExPrompt(Ex) |
| `/` | ExPrompt(SearchFwd) |
| `?` | ExPrompt(SearchBwd) |

**Search jumps:**

| Key | Action |
|-----|--------|
| `n` | next match in search direction |
| `N` | previous match in search direction |
| `*` | search word under cursor (forward) |
| `#` | search word under cursor (backward) |

**Modern conveniences (Q2):**

| Key | Action |
|-----|--------|
| `Ctrl-S` | `:w` |
| `Ctrl-Q` | `:q` |

### 7.4 INSERT keymap (small)

```rust
match event {
    KeyEvent::Esc | KeyEvent::Ctrl('[') => {
        state.commit_undo_session();
        state.mode = Mode::Normal;
    }
    KeyEvent::Char('\n')                 => state.insert_newline_with_autoindent(),
    KeyEvent::Char('\t')                 => state.insert_indent(),
    KeyEvent::Backspace                  => state.delete_back_one(),
    KeyEvent::Char(c)                    => state.insert_char(c),
    KeyEvent::Arrow(d) | KeyEvent::PageUp | KeyEvent::PageDown
                                          => state.move_cursor_in_insert(d),
    KeyEvent::Home                       => state.cursor_to_line_start(),
    KeyEvent::End                        => state.cursor_to_line_end(),
    KeyEvent::Delete                     => state.delete_forward_one(),
    _                                    => {}  // ignore Ctrl combos in v1
}
```

Tab in INSERT: with `:set expandtab`, insert `tabstop` spaces; without, insert one literal `\t`. `Shift-Tab` (decoded as `CSI Z`) deletes the previous indent unit.

### 7.5 VISUAL keymap

Movement keys extend the selection's "other end" (the anchor stays). Selection rendered with reverse video.

| Key | Action |
|-----|--------|
| `h j k l w b e gg G $ 0 %` etc. | extend selection by motion |
| `o` | toggle which end is the cursor inside the selection |
| `gv` (when entering visual) | restore last selection range |
| `d` | delete selection, exit to NORMAL |
| `y` | yank selection, exit to NORMAL |
| `c` | delete selection, enter INSERT |
| `>` / `<` | indent / dedent selection |
| `v` | toggle VisualChar off (back to NORMAL); switch to VisualLine if pressed in VisualLine |
| `V` | likewise for VisualLine |
| `Esc` | cancel, back to NORMAL |

`gv` requires storing `last_visual_range` on each visual-mode exit.

### 7.6 OperatorPending keymap

Reads the next key as a motion (or doubled operator letter for `dd`/`yy`):

```rust
match event {
    KeyEvent::Esc => Mode::Normal,                        // cancel
    {motion}      => apply_op_to_range(op, cursor..motion_endpoint),
    {same_op_letter} => apply_op_to_line(op),             // dd, yy, cc
    _             => Mode::Normal,                        // unknown, cancel
}
```

After applying: switch to `Normal` (or `Insert` for `Change`). Record undo.

### 7.7 Prompt handler

Keystrokes append to `prompt.buf`. Backspace deletes. Up/Down walk history (bounded ring buffer ~50 entries, separate per `:` and `/`). Enter dispatches: parser in `ex.rs` for `:`, search engine for `/` and `?`. Esc cancels and returns to NORMAL.

---

## 8. Render path

### 8.1 Screen layout (80×24 default)

```
[ row 1..h-2 ]    content area (with optional gutter)
[ row h-1   ]    status line (inverse video)
[ row h     ]    ex prompt / transient messages
```

Read terminal size from TTY at startup; fall back to 80×24 if the query fails.

### 8.2 ANSI escapes used

| Escape | Purpose |
|--------|---------|
| `CSI r ; c H` | cursor to (row, col) — 1-indexed |
| `CSI K` | clear from cursor to end of line |
| `CSI 2 J` | clear entire screen (startup only) |
| `CSI ?25 l` / `?25 h` | hide / show cursor |
| `CSI 7 m` / `CSI 0 m` | reverse video on / reset attrs |
| `CSI 33 m` | yellow (hlsearch) |
| `CSI 31 m` | red (errors) |
| `CSI 39 m` | default fg |

If the TTY doesn't honor an SGR, output is uglier but never broken.

### 8.3 Render entry point

```rust
fn render(state: &Editor, out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(b"\x1b[?25l");
    paint_content_rows(state, out);
    paint_status_line(state, out);
    paint_message_line(state, out);
    place_cursor(state, out);
    out.extend_from_slice(b"\x1b[?25h");
}
```

A single `TTY_WRITE_LABEL` IPC call sends `out`. ~6KB worst case for an 80×24 frame.

### 8.4 Dirty tracking

v1 ships with full-frame redraw on every state change. IPC cost is one write per frame; user input rate caps at ~30 frames/sec; ~180KB/sec to TTY. Trivial. Dirty-region rendering is a v2 optimization once we have a flicker complaint.

### 8.5 Viewport + scroll math (horizontal scroll, Q7 default)

```rust
struct Viewport {
    top_line:  usize,            // first file line shown at content row 1
    left_col:  usize,            // first column shown
    height:    u16,              // = screen_h - 2 (status + msg)
    width:     u16,              // = screen_w - gutter
}
```

After every cursor move, recenter into viewport with `scrolloff = 3` (vim default; configurable via `:set scrolloff=N`):

```rust
fn ensure_visible(vp: &mut Viewport, cur: CursorPos, scrolloff: usize) {
    if cur.line < vp.top_line + scrolloff {
        vp.top_line = cur.line.saturating_sub(scrolloff);
    } else if cur.line >= vp.top_line + vp.height as usize - scrolloff {
        vp.top_line = cur.line + scrolloff + 1 - vp.height as usize;
    }
    // Same for cur.col vs vp.left_col, vp.left_col + vp.width.
}
```

### 8.6 Soft-wrap mode (`:set wrap`)

Different render path: lines break at the right edge into multiple display rows. One file row may map to many display rows. `j`/`k` still move by file line by default; `gj`/`gk` move by display line. Wrap mode disables horizontal scroll (`vp.left_col` stays 0).

`split_by_width` is UTF-8-byte-safe (codepoint-aware): doesn't slice mid-codepoint.

### 8.7 Tab and unprintable rendering

- `\t` expands to spaces up to the next `tabstop` boundary (default 4). Bytes in the buffer remain `\t`; only display columns differ.
- Bytes that aren't ASCII printable and aren't valid UTF-8 codepoints render as `?` (Q5 — byte-safe but ASCII-only font). Valid non-ASCII codepoints also render as `?`. Both count as 1 display column.

### 8.8 Status line content

```
-- INSERT --   foo.rs [+]                              L 2:C 16  3%
```

| Region | Content |
|--------|---------|
| Mode tag | `--NORMAL--`, `--INSERT--`, `--VISUAL--`, `--V·LINE--`. NORMAL gets a blank tag (vim convention). |
| Filename | Relative to `pwd` if under it, else absolute. `[No Name]` if buffer has no path. |
| Dirty marker | `[+]` if buffer has unsaved changes |
| Cursor | `L line:C col` |
| Position | `top%` / `bot%` / `nn%` |

Whole row is rendered with reverse video.

### 8.9 Message line

The bottom row:

- During `:` / `/` / `?`: literal prompt + buffer + visible cursor inside the prompt.
- Otherwise: most recent message, sticky for ~2 seconds then fades to blank. Errors render in red; save confirmations in default fg.

### 8.10 hlsearch overlay (Q8)

When `:set hlsearch` is on AND a search pattern is active, maintain `Vec<Range<usize>>` of byte-range matches. Render walks the list per visible row to decide which cells get the highlight color.

Lazy recompute: `buf.edit_seq` is incremented on every edit. `search.matches_seq` records the seq when matches were last computed. Render checks if they differ before reading; recompute on miss.

### 8.11 Cursor placement

After painting all rows, emit `CSI cursor_screen_row ; cursor_screen_col H` to position the actual hardware cursor. Cursor screen position is computed from `(cursor_file_line - vp.top_line)` and `(cursor_file_col - vp.left_col)`, accounting for gutter width.

---

## 9. Ex commands

### 9.1 Grammar (parser in `ex.rs`)

```
ex_command  := range? command args?
range       := address (',' address)?
address     := '%' | '$' | '.' | digit+
command     := identifier
args        := raw rest of input
```

For v1: `%`, `$`, `.`, and digit-only addresses. Pattern-based ranges (`/pat/,/pat/`) deferred to v2.

### 9.2 Recognized commands

| Command | Args | Behavior |
|---------|------|----------|
| `:w [path]` | optional path | save (atomic write) to `path` or current path |
| `:q` | none | quit if not dirty; error otherwise |
| `:q!` | none | quit unconditionally |
| `:wq [path]` | optional path | `:w [path]` + `:q` |
| `:e path` | path | open path; refuse if current buffer dirty |
| `:e! path` | path | open path, drop current edits |
| `:s/old/new/[g]` | substitute | literal replace, optional global |
| `:%s/old/new/[g]` | range subst | substitute over whole file |
| `:N1,N2 s/old/new/[g]` | range subst | substitute over lines N1..N2 |
| `:N` | digit only | jump to line N |
| `:set option[=value]` | option | see §10 |
| `:help` | none | open the embedded help buffer (read-only) |

### 9.3 Substitute (`:s`) — literal only

- Find each occurrence of `old` in the addressed range.
- Replace with `new`. No backref groups, no `&`, no `\1`.
- Without `g`: replace first match per line.
- With `g`: replace all matches per line.
- One undo entry for the whole substitute.

### 9.4 Errors

Parse errors and runtime errors flow to the message line as a single human-readable string. No panics. No prints to stdout (would corrupt rendering).

---

## 10. `:set` framework

### 10.1 Settings struct

```rust
struct Settings {
    expandtab:  bool,        // default false (insert literal \t on Tab)
    tabstop:    u8,          // default 4
    smartindent: bool,       // default true
    ignorecase: bool,        // default false
    hlsearch:   bool,        // default true
    wrap:       bool,        // default false
    number:     bool,        // default false
    scrolloff:  u8,          // default 3
}
```

### 10.2 SetOp parser output

```rust
enum SetOp {
    Toggle(SettingKey),         // :set wrap         → toggle on
    NoToggle(SettingKey),       // :set nowrap       → force off
    Assign(SettingKey, Value),  // :set tabstop=2    → assign
    Show(Option<SettingKey>),   // :set / :set wrap?
}
```

### 10.3 Aliases

| Short | Long |
|-------|------|
| `et` / `noet` | `expandtab` / `noexpandtab` |
| `ts` | `tabstop` |
| `ic` / `noic` | `ignorecase` / `noignorecase` |
| `nu` / `nonu` | `number` / `nonumber` |
| `hls` / `nohls` | `hlsearch` / `nohlsearch` |
| `si` / `nosi` | `smartindent` / `nosmartindent` |
| `so` | `scrolloff` |

Unknown options return `E518: Unknown option: foo` to the message line.

### 10.4 Persistence

Settings are session-only in v1. Restart loses changes. Persistent settings (`~/.editrc` sourced on startup) are a v2 conversation.

---

## 11. Search & substitute

### 11.1 Forward / backward search

- `/pattern Enter`: scan forward from cursor; if no match by EOF, wrap to start; if still no match, status message "Pattern not found".
- `?pattern Enter`: same, backward.
- `n`: next match in current direction.
- `N`: previous match in current direction.

### 11.2 hlsearch + match list

When a pattern is active and `:set hlsearch` is on, maintain `search.matches: Vec<Range<usize>>` of all byte-range matches in the buffer. Recompute lazily when `buf.edit_seq` changes (cache key in `search.matches_seq`).

### 11.3 Word-under-cursor (`*`/`#`)

`*` extracts the word at the cursor (using the same word-class rules as `w`/`b`), wraps it in `\b…\b`-equivalent boundary check (whole-word match for v1: prev/next char is non-word), sets it as the active pattern, jumps to next match. `#` is the backward variant.

### 11.4 Case sensitivity

Default: case-sensitive. With `:set ic` (or `:set ignorecase`): both `/` search and `:s` substitute compare bytes case-insensitively (ASCII case fold only — UTF-8 case folding is out of scope).

### 11.5 History

Up-arrow at the prompt walks the history ring (~50 entries each, separate for `:` and `/`+`?` combined). Down-arrow walks forward. Esc cancels. History is session-only.

---

## 12. File I/O

### 12.1 Load

```rust
fn load_file(vfs: &mut VfsClient, path: &str) -> Result<Vec<u8>, EditError> {
    let stat = vfs.stat(path)?;
    if stat.size > MAX_FILE_BYTES {
        return Err(EditError::TooLarge(stat.size));
    }
    // Mirror shellrc.rs's read pattern (UE18): one chunk of grant memory,
    // loop read_grant in 4KB chunks, accumulate into Vec.
    ...
}
```

`MAX_FILE_BYTES = 1 << 20`. Files larger error at load with a clear stderr message + exit 1.

### 12.2 Atomic save

```rust
fn save_file_atomic(vfs: &mut VfsClient, path: &str, bytes: &[u8]) -> Result<(), EditError> {
    let tmp = format!("{}.edit~", path);
    vfs_write_all(vfs, &tmp, bytes)?;       // open O_WRONLY|O_CREAT|O_TRUNC, write, close
    vfs.rename(&tmp, path)?;                // atomic swap
    Ok(())
}
```

The `.edit~` temp file is in the same directory so the rename is intra-dir (always atomic in ext2 + MemFs). On failure, the original file is untouched. We do not honor `:set noatomic` — atomic is the only mode for v1.

### 12.3 Read-only mounts

UE work means VFS already returns `EACCES` on `open(O_WRONLY)` against a read-only mount. `:w` propagates that as a status line error: `Cannot save (read-only mount)`. Buffer stays dirty; user can `:w other-path` somewhere writable.

---

## 13. Error handling

```rust
enum EditError {
    Io(libcluu::Error),         // wraps EACCES, ENOENT, etc.
    TooLarge(usize),            // file > MAX_FILE_BYTES
    NotUtf8Path(Vec<u8>),       // path arg is not UTF-8
    DirtyBuffer,                // refused destructive op on unsaved changes
    PatternNotFound(String),    // /pat or :s with no match
    ParseError(String),         // ex parser said no
    Unknown(String),            // catch-all
}
```

All errors flow to the message line as a single human-readable string. No panics. No prints to stdout. On unrecoverable error during cleanup (e.g. failure to restore TTY mode): emit a warning via `debug_print` and exit anyway.

---

## 14. Testing strategy

### 14.1 Unit tests

In each module (run via `cargo test -p edit` once the host-runnable test setup is sorted; current libcluu host-test SIGSEGV is a known limitation):

- `piece.rs`: insert at start/middle/end, delete spanning piece boundaries, line-index correctness.
- `undo.rs`: insert-then-undo round-trips bytes; redo after undo restores; new edit after undo truncates redo stack.
- `motion.rs`: `w`/`b` on tabs/punctuation/UTF-8 bytes, `gg`/`G` clamps, `%` matches `()` `[]` `{}`.
- `search.rs`: `n`/`N` wraps, `*` finds word-under-cursor, hlsearch range list.
- `ex.rs`: parse `:%s/foo/bar/g`, `:1,5d`, `:set tabstop=8`.
- `settings.rs`: alias resolution, `:set ic` toggles, `:set noic` forces off.

### 14.2 Harness cases

| Case | What it proves |
|------|----------------|
| `l2_edit_smoke` | open file, no-op, save, exit. Binary boots, TTY mode works. |
| `l2_edit_insert` | open file, INSERT, type `hello`, Esc, `:wq`. File contains `hello`. |
| `l2_edit_undo` | INSERT, type, Esc, `u`, `:wq`. File unchanged. |
| `l2_edit_search` | open known content, `/pattern`, `n`, verify cursor position via debug_print. |
| `l2_edit_substitute` | open file, `:%s/old/new/g`, `:wq`, read back, verify substitution. |
| `l2_edit_eacces` | open file in read-only mount (`/etc/motd` from user envelope), `:w`, expect "read-only" status; buffer stays dirty. |

Keystroke injection follows the existing `KEYSTROKE_COMMANDS` pattern in `harness_run.sh`.

### 14.3 Manual / interactive

Some things can't be automated and need eyeball verification on the framebuffer:

- Visual mode `v jjj d` correctness.
- Soft-wrap rendering with long Markdown.
- Esc-vs-arrow-key timing under load.
- hlsearch coloring on a syntax-highlighted-looking file.

---

## 15. Acceptance criteria for v1 ship

- All harness cases above pass.
- A 30-minute "edit a real config file" session by the user (open `/home/root/.shellrc`, change PATH, save, restart shell, verify) without:
  - Crashes
  - Visual corruption
  - Lost data on `:w`
  - Surprising key bindings
- Editor handles a 500KB Rust source file (the kernel's `mod.rs` if convenient) for opening, navigation, single-line edits, save.
- Hard-fault modes report cleanly without leaving the TTY in raw mode:
  - EACCES on save
  - File-not-found on `:e`
  - EPIPE on parent-shell death

---

## 16. Out of scope for v1

Explicitly punted:

- Multiple buffers / `:bn` / `:bp` / `:ls`
- Window splits (`:vs`, `:sp`)
- Persistent undo across sessions
- `~/.editrc` config file
- Color schemes / syntax highlighting
- Regex search/substitute (literal only)
- Block visual mode (`Ctrl-V`)
- `:tutor` interactive tutorial
- Stdin as input source
- Macros (`q` / `@`)
- Marks (`m` / `'`)
- Registers beyond the unnamed one (no `"a y` etc. — yank/paste uses one shared clipboard)
- `gd` does *real* def-jump (we ship a pattern-match heuristic for `fn|struct|let|const|enum|impl|mod|trait|type` only)

---

## 17. References

- Piece table primer: Charles Crowley, "Data Structures for Text Sequences" (1998).
- Vim modeling: `:help` topics motion, change, undo, visual.
- CLUU shell line editor (`userspace/shell/src/editor.rs` if it exists, else inlined in commands.rs): existing precedent for raw-mode TTY handling, escape-sequence decoding, and history.
- libcluu's existing VFS read pattern: `userspace/shell/src/shellrc.rs::read_file_via_vfs` (UE18).
- libcluu's existing atomic write pattern: none yet — we'll be the first; the rename + write ordering is the standard POSIX recipe.

## 18. LOC budget tally

| Module | Estimate |
|--------|----------|
| `piece.rs` + `buffer.rs` | 500 |
| `undo.rs` | 200 |
| `mode.rs` + `normal.rs` + `insert.rs` + `visual.rs` + `op_pending.rs` | 600 |
| `motion.rs` + `ops.rs` | 400 |
| `search.rs` | 250 |
| `ex.rs` + `prompt.rs` | 350 |
| `settings.rs` | 150 |
| `render.rs` | 350 |
| `input.rs` | 200 |
| `vfs_io.rs` | 150 |
| `help.rs` (~3KB const + thin viewer) | 100 |
| `main.rs` (boot, argv, top loop) | 150 |

**Total: ~3,400 LOC.** Slightly over the 3k target; reasonable for the breadth.
