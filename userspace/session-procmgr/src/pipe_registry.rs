extern crate alloc;
use alloc::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pipe {
    pub id: u64,
    pub read_cap: u64,
    pub write_cap: u64,
    pub buffer_cap: u64,
}

pub struct PipeRegistry {
    next_id: u64,
    pipes: BTreeMap<u64, Pipe>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum PipeError {
    NotFound,
}

impl PipeRegistry {
    pub fn new() -> Self {
        Self { next_id: 1, pipes: BTreeMap::new() }
    }

    pub fn create(&mut self, read_cap: u64, write_cap: u64, buffer_cap: u64) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.pipes.insert(id, Pipe { id, read_cap, write_cap, buffer_cap });
        id
    }

    pub fn lookup(&self, id: u64) -> Option<&Pipe> {
        self.pipes.get(&id)
    }

    pub fn close(&mut self, id: u64) -> Result<Pipe, PipeError> {
        self.pipes.remove(&id).ok_or(PipeError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_returns_distinct_ids() {
        let mut r = PipeRegistry::new();
        let a = r.create(0xA0, 0xA1, 0xA2);
        let b = r.create(0xB0, 0xB1, 0xB2);
        assert_ne!(a, b);
    }

    #[test]
    fn close_known() {
        let mut r = PipeRegistry::new();
        let id = r.create(1, 2, 3);
        let p = r.close(id).unwrap();
        assert_eq!(p.read_cap, 1);
    }

    #[test]
    fn close_unknown() {
        let mut r = PipeRegistry::new();
        assert_eq!(r.close(999), Err(PipeError::NotFound));
    }
}
