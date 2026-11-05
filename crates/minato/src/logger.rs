//! Minimal logger: appends to `%APPDATA%\MinatoWM\minato.log` (and stderr
//! in debug builds, where there is a console).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use log::{LevelFilter, Log, Metadata, Record};

struct Logger {
    file: Option<Mutex<File>>,
    level: LevelFilter,
}

impl Log for Logger {
    fn enabled(&self, m: &Metadata) -> bool {
        m.level() <= self.level && m.target().starts_with("minato")
    }

    fn log(&self, r: &Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        let line = format!("{secs:.3} {:<5} {}: {}\n", r.level(), r.target(), r.args());
        if cfg!(debug_assertions) {
            eprint!("{line}");
        }
        if let Some(f) = &self.file {
            if let Ok(mut f) = f.lock() {
                let _ = f.write_all(line.as_bytes());
            }
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    let dir = minato_config::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("minato.log");
    // Keep the log from growing without bound.
    if std::fs::metadata(&path).map(|m| m.len() > 4 * 1024 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("minato.old.log"));
    }
    let file = OpenOptions::new().create(true).append(true).open(path).ok().map(Mutex::new);
    let level = match std::env::var("MINATO_LOG").as_deref() {
        Ok("debug") => LevelFilter::Debug,
        Ok("trace") => LevelFilter::Trace,
        _ => LevelFilter::Info,
    };
    let _ = log::set_boxed_logger(Box::new(Logger { file, level }));
    log::set_max_level(level);
}
