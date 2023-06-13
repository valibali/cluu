/// Prints formatted text to the console using the `Writer` struct.
///
/// This macro is similar to the standard `println!` macro, but it uses the `Writer` struct
/// from the `utils::writer` module to write the formatted text to the console.
///
/// # Syntax
///
/// The syntax for using the `print` macro is the same as the `println!` macro:
///
/// ```rust
/// print!(format_string, arg1, arg2, ...);
/// ```
///
/// Where `format_string` is a string literal with optional format specifiers, and `arg1`, `arg2`, ...
/// are the values to be formatted and printed.
///
/// # Examples
///
/// Basic usage:
///
/// ```rust
/// print!("The answer is {}", 42);
/// ```
///
/// This will print the formatted string "The answer is 42" to the console.
///
/// # Panics
///
/// If writing to the console fails, a panic will occur with the message "Printing fmt failed".
///
/// # Notes
///
/// - This macro internally uses the `core::fmt::Write` trait to write formatted text to the console.
/// - The `Writer` struct from the `utils::writer` module is used to perform the actual writing.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ({
        use core::fmt::Write;
        let _ = $crate::utils::writer::Writer::new().write_fmt(format_args!($($arg)*)).expect("Printing fmt failed");
    });
}

/// Prints a formatted string followed by a new line to the console.
///
/// This macro is similar to the standard `println!` macro, but it uses the `print!` macro
/// to print the formatted string followed by a new line character (`\n`).
///
/// # Examples
///
/// ```rust
/// serial_println!("Hello, World!");
/// ```
#[macro_export]
macro_rules! serial_println {
    () => (print!("\n"));
    ($fmt:expr) => (print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => (print!(concat!($fmt, "\n"), $($arg)*));
}

/// Clears the console screen.
///
/// This macro sends the escape sequences `"\u{001B}[2J\u{001B}[H"` to the console,
/// which clear the screen and move the cursor to the top-left position.
///
/// # Examples
///
/// ```rust
/// serial_clearcls!();
/// ```
#[macro_export]
macro_rules! serial_clearcls {
    () => (print!("\u{001B}[2J\u{001B}[H"));
}
