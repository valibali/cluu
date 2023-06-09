#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ({
        use core::fmt::Write;
        let _ = $crate::utils::writer::Writer::new().write_fmt(format_args!($($arg)*)).expect("Printing fmt failed");
    });
}

/// Print with new line to console
#[macro_export]
macro_rules! serial_println {
    () => (print!("\n"));
    ($fmt:expr) => (print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => (print!(concat!($fmt, "\n"), $($arg)*));
}

#[macro_export]
macro_rules! serial_clearcls {
    () => (print!("\u{001B}[2J\u{001B}[H"));
}
