//! Model-View-Update (MVU) core — the Elm Architecture trait + command type.
//!
//! SRP split from the original lib.rs. Provides:
//! - `Model` trait: the init/update/view contract for TUI applications
//! - `Cmd` enum: lazily-evaluated side-effect descriptor
//!
//! no_std + alloc. Pure state + transition — no I/O.

extern crate alloc;
use alloc::vec::Vec;

use crate::input::KeyEvent;
use crate::View;

// =========================================================================
// Model trait — Elm MVU
// =========================================================================

/// Elm-style Model trait. Each application implements this with its own
/// `Msg` associated type. The runtime calls `init` once, then loops
/// `update -> view -> render`.
pub trait Model: Sized {
    /// Application-specific message type (typically an enum).
    type Msg;

    /// Initial model state and startup command.
    fn init() -> (Self, Cmd);

    /// Process a message, mutating state. Returns a `Cmd` describing
    /// follow-up effects.
    fn update(&mut self, msg: Self::Msg) -> Cmd;

    /// Render the current state as a `View`.
    fn view(&self) -> View;

    /// Convert a key event into a message. Return `None` to ignore.
    fn from_key(key: KeyEvent) -> Option<Self::Msg>;

    /// Optional terminal cursor position after rendering (0-indexed row,
    /// col). The Program emits `CSI row+1;col+1 H` after the diff render.
    /// Default: `None` (no cursor positioning).
    fn cursor_position(&self) -> Option<(usize, usize)> {
        None
    }

    fn on_resize(&mut self) {}
}

// =========================================================================
// Cmd — side-effect descriptor
// =========================================================================

/// A lazily-evaluated side-effect descriptor.
///
/// For v0, `None` and `Quit` are the only variants the `Program` runtime
/// acts on. `Batch` and `Sequence` are structural — they compose cmds for
/// future execution but the v0 runtime flattens them without performing
/// I/O or async work. Cmd is non-generic because v0 cmds do not carry
/// Msg-producing effects; a future revision may parameterize it.
pub enum Cmd {
    /// No side-effect.
    None,
    /// Run all cmds concurrently (structural — v0 flattens to no-op).
    Batch(Vec<Cmd>),
    /// Run cmds in order, stopping on first that produces a msg
    /// (structural — v0 flattens to no-op).
    Sequence(Vec<Cmd>),
    /// Signal the program loop to exit.
    Quit,
}

impl Cmd {
    pub fn none() -> Self {
        Cmd::None
    }

    pub fn quit() -> Self {
        Cmd::Quit
    }

    pub fn batch(cmds: Vec<Cmd>) -> Self {
        Cmd::Batch(cmds)
    }

    pub fn sequence(cmds: Vec<Cmd>) -> Self {
        Cmd::Sequence(cmds)
    }

    /// Returns true if this cmd (or any nested cmd) signals quit.
    pub fn should_quit(&self) -> bool {
        match self {
            Cmd::Quit => true,
            Cmd::Batch(cmds) | Cmd::Sequence(cmds) => cmds.iter().any(|c| c.should_quit()),
            Cmd::None => false,
        }
    }
}

impl Default for Cmd {
    fn default() -> Self {
        Cmd::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::KeyEvent;
    use crate::View;
    use alloc::string::String;
    use alloc::format;
    use alloc::vec;

    // --- Cmd tests ---

    #[test]
    fn cmd_none_should_not_quit() {
        let cmd = Cmd::none();
        assert!(!cmd.should_quit());
    }

    #[test]
    fn cmd_quit_should_quit() {
        let cmd = Cmd::quit();
        assert!(cmd.should_quit());
    }

    #[test]
    fn cmd_batch_with_quit_should_quit() {
        let cmd = Cmd::batch(vec![Cmd::none(), Cmd::quit(), Cmd::none()]);
        assert!(cmd.should_quit());
    }

    #[test]
    fn cmd_batch_without_quit_should_not_quit() {
        let cmd = Cmd::batch(vec![Cmd::none(), Cmd::none()]);
        assert!(!cmd.should_quit());
    }

    #[test]
    fn cmd_sequence_with_quit_should_quit() {
        let cmd = Cmd::sequence(vec![Cmd::none(), Cmd::quit()]);
        assert!(cmd.should_quit());
    }

    #[test]
    fn cmd_nested_batch_quit_should_quit() {
        let inner = Cmd::batch(vec![Cmd::quit()]);
        let outer = Cmd::batch(vec![Cmd::none(), inner]);
        assert!(outer.should_quit());
    }

    #[test]
    fn cmd_default_is_none() {
        let cmd = Cmd::default();
        assert!(!cmd.should_quit());
    }

    // --- Minimal Model test ---

    struct CounterModel {
        count: u32,
    }

    #[allow(dead_code)]
    enum CounterMsg {
        Key(KeyEvent),
        Quit,
    }

    impl Model for CounterModel {
        type Msg = CounterMsg;

        fn init() -> (Self, Cmd) {
            (CounterModel { count: 0 }, Cmd::none())
        }

        fn update(&mut self, msg: CounterMsg) -> Cmd {
            match msg {
                CounterMsg::Key(_) => {
                    self.count += 1;
                    Cmd::none()
                }
                CounterMsg::Quit => Cmd::quit(),
            }
        }

        fn view(&self) -> View {
            let mut v = View::new(20, 1);
            let s = format!("count: {}", self.count);
            v.write_str(0, 0, &s);
            v
        }

        fn from_key(key: KeyEvent) -> Option<CounterMsg> {
            match key {
                KeyEvent::Ctrl('c') => Some(CounterMsg::Quit),
                other => Some(CounterMsg::Key(other)),
            }
        }
    }

    #[test]
    fn counter_model_init_and_update() {
        let (mut model, cmd) = CounterModel::init();
        assert!(!cmd.should_quit());
        assert_eq!(model.count, 0);

        let key = KeyEvent::Char('a');
        let msg = CounterModel::from_key(key).unwrap();
        let cmd = model.update(msg);
        assert!(!cmd.should_quit());
        assert_eq!(model.count, 1);

        let msg = CounterModel::from_key(KeyEvent::Char('b')).unwrap();
        model.update(msg);
        assert_eq!(model.count, 2);
    }

    #[test]
    fn counter_model_quit_on_ctrl_c() {
        let (mut model, _) = CounterModel::init();
        let msg = CounterModel::from_key(KeyEvent::Ctrl('c')).unwrap();
        let cmd = model.update(msg);
        assert!(cmd.should_quit());
    }

    #[test]
    fn counter_model_view_shows_count() {
        let (mut model, _) = CounterModel::init();
        model.update(CounterModel::from_key(KeyEvent::Char('x')).unwrap());
        model.update(CounterModel::from_key(KeyEvent::Char('y')).unwrap());
        let v = model.view();
        assert_eq!(v.get(0, 0).map(|c| c.ch), Some('c'));
        let cell_str: String = v.cells.iter().take(9).map(|c| c.ch).collect();
        assert_eq!(cell_str, "count: 2 ");
    }
}
