const PAGE_SIZE: usize = 0x1000;

pub(crate) const fn pages_for_window(width: usize, height: usize, header_bytes: usize) -> usize {
    let cells_bytes = width * height * core::mem::size_of::<u64>();
    let total_bytes = header_bytes + cells_bytes;
    (total_bytes + PAGE_SIZE - 1) / PAGE_SIZE
}

#[cfg(test)]
mod tests {
    #[test]
    fn pages_for_window_use_returned_geometry() {
        let pages = super::pages_for_window(82, 27, 32);

        assert_eq!(pages, 5);
        assert_ne!(pages, 33);
    }
}
