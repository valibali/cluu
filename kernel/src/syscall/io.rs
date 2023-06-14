use core::cmp::PartialEq;
use core::ops::{BitAnd, BitOr, Not};

/// Represents an I/O interface.
pub trait Io {
    /// The value type used for I/O operations.
    type Value: Copy + PartialEq + BitAnd<Output = Self::Value> + BitOr<Output = Self::Value> + Not<Output = Self::Value>;

    /// Reads the value from the I/O interface.
    fn read(&self) -> Self::Value;

    /// Writes the value to the I/O interface.
    fn write(&mut self, value: Self::Value);

    /// Reads the value from the I/O interface and checks if the specified flags are set.
    ///
    /// # Arguments
    ///
    /// * `flags` - The flags to check.
    ///
    /// # Returns
    ///
    /// Returns `true` if all the specified flags are set, `false` otherwise.
    #[inline(always)]
    fn readf(&self, flags: Self::Value) -> bool  {
        (self.read() & flags) as Self::Value == flags
    }

    /// Writes the value to the I/O interface based on the specified flags and value.
    ///
    /// # Arguments
    ///
    /// * `flags` - The flags to modify.
    /// * `value` - The value indicating whether to set (`true`) or clear (`false`) the flags.
    #[inline(always)]
    fn writef(&mut self, flags: Self::Value, value: bool) {
        let tmp: Self::Value = match value {
            true => self.read() | flags,
            false => self.read() & !flags,
        };
        self.write(tmp);
    }
}

/// Wrapper for an I/O interface providing read-only access.
pub struct ReadOnly<I> {
    inner: I
}

impl<I> ReadOnly<I> {
    /// Creates a new `ReadOnly` wrapper instance.
    ///
    /// # Arguments
    ///
    /// * `inner` - The inner I/O interface.
    pub const fn new(inner: I) -> ReadOnly<I> {
        ReadOnly {
            inner: inner
        }
    }
}

impl<I: Io> ReadOnly<I> {
    /// Reads the value from the I/O interface.
    #[inline(always)]
    pub fn read(&self) -> I::Value {
        self.inner.read()
    }

    /// Reads the value from the I/O interface and checks if the specified flags are set.
    ///
    /// # Arguments
    ///
    /// * `flags` - The flags to check.
    ///
    /// # Returns
    ///
    /// Returns `true` if all the specified flags are set, `false` otherwise.
    #[allow(dead_code)]
    #[inline(always)]
    pub fn readf(&self, flags: I::Value) -> bool {
        self.inner.readf(flags)
    }
}

/// Wrapper for an I/O interface providing write-only access.
pub struct WriteOnly<I> {
    inner: I
}

impl<I> WriteOnly<I> {
    /// Creates a new `WriteOnly` wrapper instance.
    ///
    /// # Arguments
    ///
    /// * `inner` - The inner I/O interface.
    #[allow(dead_code)]
    pub const fn new(inner: I) -> WriteOnly<I> {
        WriteOnly {
            inner: inner
        }
    }
}

impl<I: Io> WriteOnly<I> {
    /// Writes the value to the I/O interface.
    ///
    /// # Arguments
    ///
    /// * `value` - The value to write.
    #[allow(dead_code)]
    #[inline(always)]
    pub fn write(&mut self, value: I::Value) {
        self.inner.write(value)
    }

    /// Writes the value to the I/O interface based on the specified flags and value.
    ///
    /// # Arguments
    ///
    /// * `flags` - The flags to modify.
    /// * `value` - The value indicating whether to set (`true`) or clear (`false`) the flags.
    #[allow(dead_code)]
    #[inline(always)]
    pub fn writef(&mut self, flags: I::Value, value: bool) {
        self.inner.writef(flags, value)
    }
}
