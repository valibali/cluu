use arch::x86_64::peripheral::uart_16550::SerialPort;
use syscall::pio::Pio;
use spin::Mutex;

pub static COM2: Mutex<SerialPort<Pio<u8>>> = Mutex::new(SerialPort::<Pio<u8>>::new(0x2F8));


pub fn init_noncpu_perif() {
    COM2.lock().init();
}