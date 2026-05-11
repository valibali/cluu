use super::event::{Attr, Event};

#[derive(Clone, Copy, PartialEq)]
enum EscState {
    Normal,
    Escape,
    Csi,
}

pub struct Parser {
    state: EscState,
    params: [u16; 4],
    param_count: usize,
    current: u16,
    attr: Attr,
}

impl Parser {
    pub fn new() -> Self {
        Self {
            state: EscState::Normal,
            params: [0; 4],
            param_count: 0,
            current: 0,
            attr: Attr::default_attr(),
        }
    }

    pub fn feed<F: FnMut(Event)>(&mut self, _bytes: &[u8], mut _emit: F) {
        // intentionally empty: filled in Task 2 once tests are written
    }
}
