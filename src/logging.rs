use log::{Log, Record, Level, Metadata};

pub struct SimpleLogger;

impl Log for SimpleLogger {
  fn enabled(&self, metadata: &Metadata) -> bool {
    metadata.level() <= Level::Info
  }

  fn log(&self, record: &Record) {
    if self.enabled(record.metadata()) {

      eprintln!("[{}] - {} - {}", record.level(), record.target(), record.args());
    }
  }

  fn flush(&self) {}
}
