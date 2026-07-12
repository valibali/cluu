//! BSD socket API smoke test: loopback TCP echo.
//!
//! Creates a server socket, binds to 127.0.0.1:8080, listens, then
//! connects a client socket, sends "hello", receives the echo, and
//! verifies the data matches.

#![no_std]
#![no_main]

extern crate alloc;

#[allow(unused_imports)]
use libcluu::runtime as _;

use alloc::format;
use libcluu::posix::socket;

const LOOPBACK: u32 = 0x7F000001;
const PORT: u16 = 8080;

#[no_mangle]
pub extern "C" fn main() -> i32 {
    let server_fd = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0);
    if server_fd < 0 {
        let _ = libcluu::debug_print("l2_socket_basic: FAIL socket(server)\n");
        return 1;
    }

    if socket::bind(server_fd, LOOPBACK, PORT) != 0 {
        let _ = libcluu::debug_print("l2_socket_basic: FAIL bind\n");
        return 1;
    }

    if socket::listen(server_fd, 1) != 0 {
        let _ = libcluu::debug_print("l2_socket_basic: FAIL listen\n");
        return 1;
    }

    let client_fd = socket::socket(socket::AF_INET, socket::SOCK_STREAM, 0);
    if client_fd < 0 {
        let _ = libcluu::debug_print("l2_socket_basic: FAIL socket(client)\n");
        return 1;
    }

    if socket::connect(client_fd, LOOPBACK, PORT) != 0 {
        let _ = libcluu::debug_print("l2_socket_basic: FAIL connect\n");
        return 1;
    }

    let accepted_fd = socket::accept(server_fd);
    if accepted_fd < 0 {
        let _ = libcluu::debug_print("l2_socket_basic: FAIL accept\n");
        return 1;
    }

    let msg = b"hello";
    let sent = socket::send(client_fd, msg.as_ptr(), msg.len(), 0);
    if sent != msg.len() as isize {
        let _ = libcluu::debug_print(&format!(
            "l2_socket_basic: FAIL send sent={}\n",
            sent
        ));
        return 1;
    }

    let mut buf = [0u8; 64];
    let n = socket::recv(accepted_fd, buf.as_mut_ptr(), buf.len(), 0);
    if n != msg.len() as isize {
        let _ = libcluu::debug_print(&format!(
            "l2_socket_basic: FAIL recv n={}\n",
            n
        ));
        return 1;
    }

    let echo_sent = socket::send(accepted_fd, buf.as_ptr(), n as usize, 0);
    if echo_sent != n {
        let _ = libcluu::debug_print("l2_socket_basic: FAIL echo send\n");
        return 1;
    }

    let mut echo_buf = [0u8; 64];
    let echo_n = socket::recv(client_fd, echo_buf.as_mut_ptr(), echo_buf.len(), 0);
    if echo_n != msg.len() as isize {
        let _ = libcluu::debug_print(&format!(
            "l2_socket_basic: FAIL echo recv n={}\n",
            echo_n
        ));
        return 1;
    }

    if &echo_buf[..echo_n as usize] != msg {
        let _ = libcluu::debug_print("l2_socket_basic: FAIL echo mismatch\n");
        return 1;
    }

    let _ = socket::close_socket(accepted_fd);
    let _ = socket::close_socket(client_fd);
    let _ = socket::close_socket(server_fd);

    let _ = libcluu::debug_print("l2_socket_basic: PASS\n");
    0
}
