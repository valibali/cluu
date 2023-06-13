use core::fmt::Write;

use log::{Record, Level, Metadata, LevelFilter};

struct CluuLogger;

impl log::Log for CluuLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            serial_println!("[{}] {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

static LOGGER: CluuLogger = CluuLogger;

pub fn init(clearscr: bool) {
    if clearscr {
        _ = crate::utils::writer::Writer::new().write_str("\u{001B}[2J\u{001B}[H"); //clear screen
    };

    let logger_init_result = 
        log::set_logger(&LOGGER).map(|()| log::set_max_level(LevelFilter::Info)); //clearscr: true

    match logger_init_result {
        Ok(_) => serial_println!("Logger initialized correctly"),
        Err(err) => panic!("Error with initializing logger: {}", err),
    }

}