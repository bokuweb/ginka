//! Crash reports: what a panic leaves behind in the logs directory.
//!
//! A panic in the daemon or the window used to leave only whatever the log
//! had flushed. The hook installed here writes one file per crash —
//! `crash-<process>-<unix seconds>.log` beside the rolling logs — with the
//! message, where it happened, the build, and a backtrace, before the default
//! hook runs. Nothing is sent anywhere (rule 7): the file is for the reader,
//! and for the issue they may choose to open.

use std::path::{Path, PathBuf};

/// Install the hook for `process` (`daemon`, `app`), writing into `logs`.
///
/// The hook that was there before still runs afterwards, so a panic still
/// prints where it always did.
pub fn install(logs: PathBuf, process: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|text| (*text).to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "a panic with no message".to_string());
        let location = info
            .location()
            .map(|at| format!("{}:{}:{}", at.file(), at.line(), at.column()));
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or_default();
        let text = report(process, &message, location.as_deref(), &backtrace);
        match write(&logs, process, at, &text) {
            Ok(path) => tracing::error!(path = %path.display(), "crashed; report written"),
            Err(error) => tracing::error!(%error, "crashed, and the report could not be written"),
        }
        previous(info);
    }));
}

/// The text of one report.
pub fn report(process: &str, message: &str, location: Option<&str>, backtrace: &str) -> String {
    format!(
        "ginka {process} {version} ({os} {arch}) panicked\n\
         \n\
         {message}\n\
         at {location}\n\
         \n\
         {backtrace}\n",
        version = env!("CARGO_PKG_VERSION"),
        os = std::env::consts::OS,
        arch = std::env::consts::ARCH,
        location = location.unwrap_or("an unknown place"),
    )
}

/// Write a report and say where it went.
pub fn write(logs: &Path, process: &str, at: i64, text: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(logs)?;
    let path = logs.join(format!("crash-{process}-{at}.log"));
    std::fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_says_what_where_which_build_and_how_it_got_there() {
        let text = report(
            "daemon",
            "index out of bounds",
            Some("src/x.rs:3:9"),
            "0: main",
        );
        assert!(text.contains("ginka daemon"), "{text}");
        assert!(text.contains(env!("CARGO_PKG_VERSION")), "{text}");
        assert!(text.contains("index out of bounds"), "{text}");
        assert!(text.contains("src/x.rs:3:9"), "{text}");
        assert!(text.contains("0: main"), "{text}");
    }

    #[test]
    fn a_report_is_one_file_per_crash_named_for_its_process_and_time() {
        let dir = tempfile::tempdir().unwrap();
        let logs = dir.path().join("logs");
        let path = write(&logs, "app", 1_790_000_000, "boom").unwrap();
        assert_eq!(path, logs.join("crash-app-1790000000.log"));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "boom");
    }
}
