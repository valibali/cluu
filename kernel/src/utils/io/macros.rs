/*
 * Kernel Utility Macros
 *
 * This module provides essential macros for kernel development, including
 * printing, logging, and console manipulation. These macros are kernel-specific
 * versions of standard library functionality adapted for bare-metal environment.
 *
 * Why this is important:
 * - Provides familiar print!/println! style macros for kernel development
 * - Enables formatted output to serial console for debugging
 * - Implements console control functions (clear screen, etc.)
 * - Essential for kernel debugging and development workflow
 * - Replaces standard library macros that aren't available in no_std
 *
 * Key macros:
 * - print!: Basic formatted output to serial console
 * - serial_println!: Print with newline to serial console
 * - serial_clearcls!: Clear console screen
 */

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
        let _ = $crate::utils::io::writer::Writer::new().write_fmt(format_args!($($arg)*)).expect("Printing fmt failed");
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
    () => ($crate::print!("\n"));
    ($fmt:expr) => ($crate::print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::print!(concat!($fmt, "\n"), $($arg)*));
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
    () => {
        $crate::print!("\u{001B}[2J\u{001B}[H")
    };
}
