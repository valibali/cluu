//! :help command opens a read-only buffer with the cheat sheet.

extern crate alloc;
use alloc::string::String;

use crate::buffer::EditBuffer;
use crate::mode::Editor;

const CHEAT_SHEET: &str = "\
edit — vi-flavored modal editor.

MODES:
  NORMAL     default. press i to insert, : for ex, / for search, v for visual.
  INSERT     enter via i, a, o, O, I, A. Esc or Ctrl-[ exits.
  VISUAL     v (char), V (line), gv (last range), o (toggle anchor).

NORMAL MOTIONS:
  h j k l    one cell (Arrow keys also work)
  w b e      word forward, back, end
  0 ^ $      line start / first non-ws / line end
  gg G       first / last line
  %          match bracket
  Ctrl-F/B   page down / up (Page-Down/Up keys also work)

NORMAL EDITS:
  i a o      insert at, after, line below
  I A O      insert at line start, end, line above
  x          delete char
  dd yy      delete / yank line
  d{motion}  delete by motion
  y{motion}  yank by motion
  c{motion}  change by motion
  p P        paste after / before
  u Ctrl-R   undo / redo
  > <        indent / dedent (with motion or visual)

EX COMMANDS:
  :w [path]  save (atomic write)
  :q         quit
  :q!        quit without saving
  :wq        save and quit
  :e path    open path
  :N         goto line N
  :s/old/new/[g]
  :%s/old/new/[g]
  :set       see/change settings (et, ts=N, ic, hls, nu, wrap)
  :help      this screen

SEARCH:
  /pat       search forward
  ?pat       search backward
  n N        next / previous match
  *  #       search word under cursor

SHORTCUTS:
  Ctrl-S     :w
  Ctrl-Q     quit
  Tab        indent (literal \\t or expandtab spaces)
  Shift-Tab  dedent

Press :q to close help and return.
";

pub fn open(state: &mut Editor) {
    state.buf = EditBuffer::new(CHEAT_SHEET.as_bytes().to_vec(), Some(":help".into()));
    state.buf.mark_clean();
}
