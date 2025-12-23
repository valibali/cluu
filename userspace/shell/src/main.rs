#![no_std]
#![no_main]

// FFI bindings to C syscall library
mod syscalls {
    pub const STDIN: i32 = 0;
    pub const STDOUT: i32 = 1;
    pub const STDERR: i32 = 2;

    pub const O_RDONLY: i32 = 0x0000;
    pub const O_WRONLY: i32 = 0x0001;
    pub const O_RDWR: i32 = 0x0002;

    unsafe extern "C" {
        pub fn syscall_write(fd: i32, buf: *const u8, count: usize) -> isize;
        pub fn syscall_read(fd: i32, buf: *mut u8, count: usize) -> isize;
        pub fn syscall_open(path: *const u8, flags: i32) -> i32;
        pub fn syscall_close(fd: i32) -> i32;
        pub fn syscall_exit(code: i32) -> !;
    }
}

// Helper functions for I/O
fn print(s: &str) {
    unsafe {
        syscalls::syscall_write(syscalls::STDOUT, s.as_ptr(), s.len());
    }
}

fn print_byte(c: u8) {
    unsafe {
        syscalls::syscall_write(syscalls::STDOUT, &c as *const u8, 1);
    }
}

fn print_dec(mut n: i32) {
    if n < 0 {
        print_byte(b'-');
        n = -n;
    }

    if n == 0 {
        print_byte(b'0');
        return;
    }

    let mut buf = [0u8; 12];
    let mut i = 0;
    while n > 0 {
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        i += 1;
    }

    while i > 0 {
        i -= 1;
        print_byte(buf[i]);
    }
}

fn read_char() -> Option<u8> {
    let mut buf = [0u8; 1];
    let n = unsafe {
        syscalls::syscall_read(syscalls::STDIN, buf.as_mut_ptr(), 1)
    };

    if n > 0 {
        Some(buf[0])
    } else {
        None
    }
}

// Read a line of input into a fixed-size buffer
// Returns number of bytes read (excluding newline)
fn read_line(buf: &mut [u8]) -> usize {
    let mut pos = 0;

    loop {
        if pos >= buf.len() - 1 {
            break; // Buffer full
        }

        if let Some(ch) = read_char() {
            if ch == b'\n' || ch == b'\r' {
                print("\n");
                break;
            } else if ch == 127 || ch == 8 { // Backspace/DEL
                if pos > 0 {
                    pos -= 1;
                    print("\x08 \x08"); // Move back, print space, move back
                }
            } else if ch >= 32 && ch < 127 { // Printable ASCII
                buf[pos] = ch;
                pos += 1;
            }
        } else {
            break;
        }
    }

    buf[pos] = 0; // Null terminate
    pos
}

// Parse command line into command and argument
fn parse_command(line: &[u8], len: usize) -> Option<(&[u8], &[u8])> {
    if len == 0 {
        return None;
    }

    // Find first space
    let mut space_pos = None;
    for i in 0..len {
        if line[i] == b' ' {
            space_pos = Some(i);
            break;
        }
    }

    if let Some(pos) = space_pos {
        // Command with argument
        let cmd = &line[0..pos];
        // Skip spaces after command
        let mut arg_start = pos + 1;
        while arg_start < len && line[arg_start] == b' ' {
            arg_start += 1;
        }
        let arg = &line[arg_start..len];
        Some((cmd, arg))
    } else {
        // Command only, no argument
        Some((&line[0..len], &[]))
    }
}

// Compare byte slice with string literal
fn bytes_eq(a: &[u8], b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    for i in 0..a.len() {
        if a[i] != b.as_bytes()[i] {
            return false;
        }
    }
    true
}

// Ensure path has null terminator
fn make_null_terminated<'a>(path: &[u8], buf: &'a mut [u8]) -> &'a [u8] {
    let len = path.len().min(buf.len() - 1);
    buf[..len].copy_from_slice(&path[..len]);
    buf[len] = 0;
    &buf[..len + 1]
}

// Built-in command: cat
fn cmd_cat(path: &[u8]) {
    if path.is_empty() {
        print("cat: missing file operand\n");
        return;
    }

    // Open file
    let mut path_buf = [0u8; 256];
    let path_cstr = make_null_terminated(path, &mut path_buf);

    let fd = unsafe {
        syscalls::syscall_open(path_cstr.as_ptr(), syscalls::O_RDONLY)
    };

    if fd < 0 {
        print("cat: ");
        print(core::str::from_utf8(path).unwrap_or("???"));
        print(": No such file or directory\n");
        return;
    }

    // Read and print file contents
    let mut buf = [0u8; 1024];
    loop {
        let n = unsafe {
            syscalls::syscall_read(fd, buf.as_mut_ptr(), buf.len())
        };

        if n <= 0 {
            break;
        }

        unsafe {
            syscalls::syscall_write(syscalls::STDOUT, buf.as_ptr(), n as usize);
        }
    }

    // Close file
    unsafe {
        syscalls::syscall_close(fd);
    }
}

// Built-in command: ls
fn cmd_ls(path: &[u8]) {
    let _default_path = b"/";
    let _actual_path = if path.is_empty() { _default_path } else { path };

    // For now, ls just tries to read a directory listing file
    // In a real implementation, this would use a readdir syscall
    // For the initrd, we'll try to read a special .listing file or just report not implemented

    print("ls: directory listing not yet implemented\n");
    print("    (VFS server needs to implement directory reading)\n");
    print("    Try: cat /path/to/file\n");
}

// Execute command
fn execute_command(cmd: &[u8], arg: &[u8]) {
    if bytes_eq(cmd, "cat") {
        cmd_cat(arg);
    } else if bytes_eq(cmd, "ls") {
        cmd_ls(arg);
    } else if bytes_eq(cmd, "exit") {
        print("Goodbye!\n");
        unsafe { syscalls::syscall_exit(0); }
    } else if bytes_eq(cmd, "help") {
        print("Available commands:\n");
        print("  cat <file>  - Display file contents\n");
        print("  ls [dir]    - List directory (not yet implemented)\n");
        print("  help        - Show this help\n");
        print("  exit        - Exit shell\n");
    } else if !cmd.is_empty() {
        print("Unknown command: ");
        if let Ok(s) = core::str::from_utf8(cmd) {
            print(s);
        }
        print("\n");
        print("Type 'help' for available commands\n");
    }
}

// Main shell loop
#[unsafe(no_mangle)]
pub extern "C" fn main(_argc: i32, _argv: *const *const u8) -> i32 {
    print("\n");
    print("CLUU Shell v0.1\n");
    print("Type 'help' for available commands\n");
    print("\n");

    let mut line_buf = [0u8; 256];

    loop {
        // Print prompt
        print("root@cluu:~# ");

        // Read command line
        let len = read_line(&mut line_buf);

        // Parse and execute
        if let Some((cmd, arg)) = parse_command(&line_buf, len) {
            execute_command(cmd, arg);
        }
    }
}

// Entry point - calls main with argc/argv from stack
#[unsafe(naked)]
#[unsafe(no_mangle)]
#[unsafe(link_section = ".text.entry")]
pub unsafe extern "C" fn _start() -> ! {
    core::arch::naked_asm!(
        // Stack layout: [argc] [argv pointers...] [NULL] [arg strings...]
        "mov rdi, [rsp]",        // argc
        "lea rsi, [rsp + 8]",    // argv
        "call main",

        // Exit with return value from main
        "mov rdi, rax",
        "mov rax, 60",           // SYS_EXIT
        "syscall",

        "ud2",                   // Should never reach
    );
}

// Panic handler
#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    print("\nPANIC in shell!\n");
    unsafe { syscalls::syscall_exit(1); }
}
