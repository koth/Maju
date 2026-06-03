use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

static START: OnceLock<Instant> = OnceLock::new();
static LOG_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();

pub fn start_run(component: &str) {
    if cfg!(test) {
        return;
    }
    let started = START.set(Instant::now()).is_ok();
    if started {
        append_line("run/start", component);
    } else {
        mark("run/already_started", component);
    }
}

pub fn mark(stage: impl AsRef<str>, detail: impl AsRef<str>) {
    if cfg!(test) {
        return;
    }
    let _ = START.get_or_init(Instant::now);
    append_line(stage.as_ref(), detail.as_ref());
}

pub fn measure<T, F>(stage: impl AsRef<str>, detail: impl AsRef<str>, f: F) -> T
where
    F: FnOnce() -> T,
{
    if cfg!(test) {
        return f();
    }
    let stage = stage.as_ref().to_string();
    let detail = detail.as_ref().to_string();
    mark(format!("{stage}/start"), &detail);
    let started = Instant::now();
    let result = f();
    mark(
        format!("{stage}/end"),
        format!("{} duration_ms={}", detail, started.elapsed().as_millis()),
    );
    result
}

fn append_line(stage: &str, detail: &str) {
    let Some(log_path) = log_path() else {
        return;
    };
    let lock = LOG_LOCK.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = lock.lock() else {
        return;
    };
    if let Some(parent) = log_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let epoch_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let elapsed_ms = START
        .get()
        .map(|start| start.elapsed().as_millis())
        .unwrap_or(0);
    let safe_detail = detail.replace('\r', " ").replace('\n', " ");
    let line = format!(
        "[{epoch_ms} +{elapsed_ms}ms pid={}] {stage} {safe_detail}\n",
        std::process::id()
    );
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .and_then(|mut file| file.write_all(line.as_bytes()));
}

fn log_path() -> Option<&'static PathBuf> {
    LOG_PATH
        .get_or_init(|| {
            crate::paths::AppPaths::resolve()
                .ok()
                .map(|paths| paths.logs_dir().join("kodex-startup.log"))
        })
        .as_ref()
}
