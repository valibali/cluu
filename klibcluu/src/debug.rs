//! Kernel debug output facilities

use core::fmt::{self, Write};
use spin::Mutex;

/// Debug output trait - implement for your output device
pub trait DebugOutput: Send {
    fn write_str(&mut self, s: &str);
    fn write_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.write_str(c.encode_utf8(&mut buf));
    }
}

/// Null output (discards everything)
pub struct NullOutput;

impl DebugOutput for NullOutput {
    fn write_str(&mut self, _s: &str) {}
}

/// Global debug output
static DEBUG_OUTPUT: Mutex<Option<&'static mut dyn DebugOutput>> = Mutex::new(None);

/// Set the global debug output
///
/// # Safety
/// Must only be called once during kernel initialization
pub unsafe fn set_debug_output(output: &'static mut dyn DebugOutput) {
    *DEBUG_OUTPUT.lock() = Some(output);
}

/// Writer that uses the global debug output
pub struct DebugWriter;

impl Write for DebugWriter {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if let Some(ref mut output) = *DEBUG_OUTPUT.lock() {
            output.write_str(s);
        }
        Ok(())
    }
}

/// Print to debug output
#[macro_export]
macro_rules! kprint {
    ($($arg:tt)*) => {
        {
            use core::fmt::Write;
            let _ = write!($crate::debug::DebugWriter, $($arg)*);
        }
    };
}

/// Print line to debug output
#[macro_export]
macro_rules! kprintln {
    () => { $crate::kprint!("\n") };
    ($($arg:tt)*) => {
        $crate::kprint!("{}\n", format_args!($($arg)*))
    };
}

/// Log levels
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// Log with level
#[macro_export]
macro_rules! klog {
    ($level:expr, $($arg:tt)*) => {
        $crate::kprintln!("[{:5}] {}",
            match $level {
                $crate::debug::LogLevel::Error => "ERROR",
                $crate::debug::LogLevel::Warn  => "WARN ",
                $crate::debug::LogLevel::Info  => "INFO ",
                $crate::debug::LogLevel::Debug => "DEBUG",
                $crate::debug::LogLevel::Trace => "TRACE",
            },
            format_args!($($arg)*)
        )
    };
}

#[macro_export]
macro_rules! kerror { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Error, $($arg)*) }; }
#[macro_export]
macro_rules! kwarn  { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Warn,  $($arg)*) }; }
#[macro_export]
macro_rules! kinfo  { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Info,  $($arg)*) }; }
#[macro_export]
macro_rules! kdebug { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Debug, $($arg)*) }; }
#[macro_export]
macro_rules! ktrace { ($($arg:tt)*) => { $crate::klog!($crate::debug::LogLevel::Trace, $($arg)*) }; }
