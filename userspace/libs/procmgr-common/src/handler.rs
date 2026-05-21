//! Handler dispatch trait. Each IPC handler is one type implementing `MsgHandler`.
//! Dispatcher = static `label → fn ptr` table. Future async migration: trait
//! method becomes `async fn`, dispatcher becomes executor poll.

extern crate alloc;
use alloc::vec::Vec;

#[derive(Debug)]
pub struct Reply {
    pub words: [usize; 6],
    pub payload: Vec<u8>,
    pub label: u32,
}

impl Reply {
    pub fn ok(label: u32) -> Self {
        Self { words: [0; 6], payload: Vec::new(), label }
    }
    pub fn with_word(mut self, idx: usize, val: usize) -> Self {
        self.words[idx] = val;
        self
    }
    pub fn with_payload(mut self, p: Vec<u8>) -> Self {
        self.payload = p;
        self
    }
}

#[derive(Debug)]
pub enum HandlerError {
    BadCap,
    BadLabel,
    BadPayload,
    Internal(&'static str),
    Eagain,
    NotFound,
}

pub struct InboundMsg<'a> {
    pub label: u32,
    pub words: [usize; 6],
    pub payload: &'a [u8],
    pub sender_tid: usize,
}

pub trait MsgHandler {
    const LABEL: u32;
    type State;
    fn handle(state: &mut Self::State, msg: &InboundMsg<'_>) -> Result<Reply, HandlerError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;
    impl MsgHandler for Echo {
        const LABEL: u32 = 0xE000;
        type State = ();
        fn handle(_: &mut (), msg: &InboundMsg<'_>) -> Result<Reply, HandlerError> {
            Ok(Reply::ok(Self::LABEL).with_word(0, msg.words[0]))
        }
    }

    #[test]
    fn echo_handler() {
        let msg = InboundMsg { label: 0xE000, words: [42, 0, 0, 0, 0, 0], payload: &[], sender_tid: 1 };
        let r = Echo::handle(&mut (), &msg).unwrap();
        assert_eq!(r.words[0], 42);
        assert_eq!(r.label, 0xE000);
    }
}
