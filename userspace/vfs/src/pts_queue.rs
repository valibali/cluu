extern crate alloc;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub fn prepend_delivered_data(pending: &mut Vec<u8>, delivered: &[u8]) {
    pending.splice(0..0, delivered.iter().copied());
}

pub fn park_read<T>(reads: &mut VecDeque<T>, read: T) {
    reads.push_back(read);
}

pub fn take_parked_read<T>(reads: &mut VecDeque<T>) -> Option<T> {
    reads.pop_front()
}

#[cfg(test)]
mod tests {
    extern crate std;

    use alloc::collections::VecDeque;
    use alloc::vec;

    #[test]
    fn prepends_failed_delivery_before_newer_buffered_bytes() {
        // Given: newer bytes buffered while an earlier delivery is in flight.
        let mut pending = vec![b'c', b'd'];

        // When: every parked reply for the earlier delivery fails.
        super::prepend_delivered_data(&mut pending, b"ab");

        // Then: readers observe FIFO byte order.
        assert_eq!(pending, b"abcd");
    }

    #[test]
    fn parks_new_reads_without_discarding_existing_readers() {
        // Given: an older parked reader.
        let mut reads = VecDeque::from([1usize]);

        // When: a new ordinary read parks.
        super::park_read(&mut reads, 2);

        // Then: both readers remain in FIFO order.
        assert_eq!(reads, VecDeque::from([1, 2]));
    }

    #[test]
    fn consumes_failed_readers_before_first_successful_reader() {
        // Given: three parked readers.
        let mut reads = VecDeque::from([1usize, 2, 3]);

        // When: the first reply fails and the second succeeds.
        let failed = super::take_parked_read(&mut reads);
        let successful = super::take_parked_read(&mut reads);

        // Then: the successful reader receives once and later readers remain parked.
        assert_eq!(
            (failed, successful, reads),
            (Some(1), Some(2), VecDeque::from([3]))
        );
    }
}
