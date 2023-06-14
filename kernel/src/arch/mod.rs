#[cfg(target_arch = "x86_64")]
#[macro_use]
pub mod x86_64;

#[cfg(target_arch = "x86_64")]
use self::x86_64::*;

use utils::logger;

pub fn kstart() -> !
{
    //Init devices
    peripheral::init_peripherals();

    logger::init(true); //clearscr: true


    if let Some(ref mut fb) = *peripheral::FB.lock(){
        fb.puts("hello")
    }
    

    loop {}
}
