use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context;
use notify::{RecursiveMode, Watcher};

use crate::Config;

/// `%APPDATA%\MinatoWM`, or `$MINATO_CONFIG_DIR` when set.
pub fn config_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("MINATO_CONFIG_DIR") {
        return PathBuf::from(dir);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("MinatoWM")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn load(path: &Path) -> anyhow::Result<Config> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text)
}

fn parse(text: &str) -> anyhow::Result<Config> {
    // Tolerate a UTF-8 BOM, which Notepad and PowerShell like to add.
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    serde_json::from_str(text).context("parsing config.json")
}

/// Loads the config, writing the defaults first if the file doesn't exist.
pub fn load_or_create(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        let cfg = Config::default();
        save(path, &cfg)?;
        return Ok(cfg);
    }
    load(path)
}

/// Writes the config atomically: to a temp file next to it, then renamed
/// over the original, so the watcher never sees a half-written file.
pub fn save(path: &Path, cfg: &Config) -> anyhow::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut json = serde_json::to_string_pretty(cfg)?;
    json.push('\n');
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).with_context(|| format!("writing {}", tmp.display()))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

fn hash(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

fn read_with_retry(path: &Path) -> std::io::Result<String> {
    let mut last = None;
    for _ in 0..5 {
        match fs::read_to_string(path) {
            Ok(s) => return Ok(s),
            // Editors may still hold the file open for a moment.
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(last.unwrap())
}

/// Watches `config.json` and reports each new valid or invalid version.
/// Dropping the watcher stops it.
pub struct ConfigWatcher {
    _watcher: notify::RecommendedWatcher,
}

impl ConfigWatcher {
    /// Starts watching. `on_change` runs on a background thread after the
    /// file settles, only when its contents actually changed.
    pub fn spawn(
        path: PathBuf,
        on_change: impl Fn(Result<Config, String>) + Send + 'static,
    ) -> anyhow::Result<Self> {
        let dir = path
            .parent()
            .context("config path has no parent directory")?
            .to_path_buf();
        fs::create_dir_all(&dir)?;
        let name = path.file_name().context("config path has no file name")?.to_owned();

        let (tx, rx) = mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(event) = res {
                if event.paths.iter().any(|p| p.file_name() == Some(name.as_os_str())) {
                    let _ = tx.send(());
                }
            }
        })?;
        // Watch the directory: atomic saves replace the file, which would
        // orphan a watch on the file itself.
        watcher.watch(&dir, RecursiveMode::NonRecursive)?;

        let mut last = fs::read_to_string(&path).map(|s| hash(&s)).unwrap_or(0);
        std::thread::Builder::new()
            .name("config-watch".into())
            .spawn(move || {
                while rx.recv().is_ok() {
                    // Debounce bursts of events from a single save.
                    while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}
                    let text = match read_with_retry(&path) {
                        Ok(t) => t,
                        Err(e) => {
                            // Deleted or unreadable: keep the current config.
                            log::warn!("config unreadable: {e}");
                            continue;
                        }
                    };
                    let h = hash(&text);
                    if h == last {
                        continue;
                    }
                    last = h;
                    on_change(parse(&text).map_err(|e| format!("{e:#}")));
                }
            })?;

        Ok(Self { _watcher: watcher })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn save_then_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = load_or_create(&path).unwrap();
        assert_eq!(cfg, Config::default());
        let mut changed = cfg.clone();
        changed.gaps.inner = 42;
        save(&path, &changed).unwrap();
        assert_eq!(load(&path).unwrap(), changed);
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn bom_is_accepted() {
        assert!(parse("\u{feff}{}").is_ok());
    }

    #[test]
    fn watcher_reports_changes_and_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        save(&path, &Config::default()).unwrap();

        let seen: Arc<Mutex<Vec<Result<Config, String>>>> = Arc::default();
        let sink = seen.clone();
        let _w = ConfigWatcher::spawn(path.clone(), move |r| sink.lock().unwrap().push(r)).unwrap();

        let wait_for = |n: usize| {
            for _ in 0..100 {
                if seen.lock().unwrap().len() >= n {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            panic!("watcher did not report {n} changes");
        };

        let mut cfg = Config::default();
        cfg.gaps.outer = 99;
        save(&path, &cfg).unwrap();
        wait_for(1);
        assert_eq!(seen.lock().unwrap()[0].as_ref().unwrap().gaps.outer, 99);

        fs::write(&path, "{ not json").unwrap();
        wait_for(2);
        assert!(seen.lock().unwrap()[1].is_err());

        // Rewriting identical content is not reported again.
        fs::write(&path, "{ not json").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(seen.lock().unwrap().len(), 2);
    }
}
