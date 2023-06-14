pub use self::serial::*;

pub mod serial;

pub unsafe fn init_noncpu_perif() {
    serial::init();
}