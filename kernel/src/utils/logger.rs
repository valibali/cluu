use log::{Record, Level, Metadata, SetLoggerError, LevelFilter};

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            println!("{} - {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}



static LOGGER: SimpleLogger = SimpleLogger;

pub fn init(clearscr: bool) -> Result<(), SetLoggerError> {
    if clearscr {
        crate::utils::writer::Writer::new().write("\u{001B}[2J\u{001B}[H".as_bytes()); //clear screen
    };

    log::set_logger(&LOGGER)
        .map(|()| log::set_max_level(LevelFilter::Info))
}