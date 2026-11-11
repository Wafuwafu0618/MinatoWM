//! Client side of MinatoWM's control pipe (newline-delimited JSON).

use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::time::Instant;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

pub const PIPE_NAME: &str = r"\\.\pipe\MinatoWM.Control";

pub struct Client {
    reader: BufReader<File>,
    writer: File,
    /// WM perf clock minus our clock (ms), from [`Client::sync_clock`].
    offset_ms: f64,
    epoch: Instant,
}

impl Client {
    pub fn connect(epoch: Instant) -> Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(PIPE_NAME)
            .with_context(|| {
                format!("could not open {PIPE_NAME}: is MinatoWM running with MINATO_CONTROL=1?")
            })?;
        let writer = file.try_clone()?;
        let mut c = Self {
            reader: BufReader::new(file),
            writer,
            offset_ms: 0.0,
            epoch,
        };
        c.sync_clock()?;
        Ok(c)
    }

    pub fn request(&mut self, req: &Value) -> Result<Value> {
        let mut line = req.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        self.writer.flush()?;
        let mut resp = String::new();
        if self.reader.read_line(&mut resp)? == 0 {
            bail!("control pipe closed");
        }
        Ok(serde_json::from_str(&resp)?)
    }

    /// Like [`Self::request`], but an `"ok": false` answer is an error.
    pub fn call(&mut self, req: Value) -> Result<Value> {
        let resp = self.request(&req)?;
        if resp["ok"] != true {
            bail!("{req} failed: {}", resp["error"]);
        }
        Ok(resp)
    }

    fn now(&self) -> f64 {
        self.epoch.elapsed().as_secs_f64() * 1000.0
    }

    /// Measures the offset between the WM's perf clock and ours, taking the
    /// fastest of a few pings.
    fn sync_clock(&mut self) -> Result<()> {
        let mut best = f64::MAX;
        for _ in 0..8 {
            let t0 = self.now();
            let resp = self.call(json!({"cmd": "ping"}))?;
            let t1 = self.now();
            let rtt = t1 - t0;
            if rtt < best {
                best = rtt;
                self.offset_ms = resp["now_ms"].as_f64().unwrap_or(0.0) - (t0 + t1) / 2.0;
            }
        }
        Ok(())
    }

    /// Converts a WM perf timestamp into our clock.
    pub fn to_local(&self, wm_ms: f64) -> f64 {
        wm_ms - self.offset_ms
    }
}
