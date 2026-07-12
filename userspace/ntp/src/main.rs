//! /bin/ntp -- minimal NTP client.
//!
//! Sends a 48-byte NTPv3 client packet via UDP to 10.0.2.2:123,
//! parses the transmit timestamp from the reply, prints Unix seconds.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::debug_print;
use libcluu::posix::socket;

const NTP_HOST: u32 = (10 << 24) | (0 << 16) | (2 << 8) | 2;
const NTP_PORT: u16 = 123;
const NTP_EPOCH_OFFSET: u32 = 2208988800;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let fd = socket::socket(socket::AF_INET, socket::SOCK_DGRAM, 0);
    if fd < 0 {
        let _ = debug_print("NTP_FAIL: socket\n");
        return 1;
    }

    let mut packet = [0u8; 48];
    packet[0] = 0x1B;

    let sent = socket::sendto(fd, packet.as_ptr(), packet.len(), 0, NTP_HOST, NTP_PORT);
    if sent != 48 {
        let _ = debug_print("NTP_FAIL: sendto\n");
        let _ = socket::close_socket(fd);
        return 1;
    }

    let mut reply = [0u8; 48];
    let mut src_addr: u32 = 0;
    let mut src_port: u16 = 0;
    let n = socket::recvfrom(
        fd,
        reply.as_mut_ptr(),
        reply.len(),
        0,
        &mut src_addr,
        &mut src_port,
    );
    let _ = socket::close_socket(fd);

    if n < 48 {
        let _ = debug_print(&format!("NTP_FAIL: short reply ({})\n", n));
        return 1;
    }

    let seconds = u32::from_be_bytes([reply[40], reply[41], reply[42], reply[43]]);
    let unix_secs = seconds.saturating_sub(NTP_EPOCH_OFFSET);
    let _ = debug_print(&format!("NTP_TIME_OK unix={}\n", unix_secs));
    0
}
