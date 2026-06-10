//! `weaveffi watch` — regenerate whenever the IDL file changes, with a
//! debounce window so editor save bursts trigger a single regeneration.

use super::generate::cmd_generate;
use camino::Utf8Path;
use miette::{bail, miette, IntoDiagnostic, Result, WrapErr};
use std::process::Command;

/// Returns `true` when enough time has elapsed since the most recent file
/// system event for the debounce window to have closed and the generator to
/// fire. Pure function so the watch loop can be unit-tested without real
/// timers or `notify` events.
fn debounce_should_fire(
    last_event: std::time::Instant,
    now: std::time::Instant,
    debounce: std::time::Duration,
) -> bool {
    now.saturating_duration_since(last_event) >= debounce
}

pub(crate) fn cmd_watch(
    input: &str,
    out: &str,
    targets: Option<&str>,
    config_path: Option<&str>,
    quiet: bool,
) -> Result<()> {
    use notify::{EventKind, RecursiveMode, Watcher};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let in_path = Utf8Path::new(input);
    let abs_input = std::fs::canonicalize(in_path.as_std_path())
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to resolve input file: {}", input))?;
    let parent = abs_input
        .parent()
        .ok_or_else(|| miette!("input file has no parent directory: {}", input))?
        .to_path_buf();

    let (tx, rx) = mpsc::channel::<notify::Result<notify::Event>>();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .into_diagnostic()
    .wrap_err("failed to create file watcher")?;
    watcher
        .watch(&parent, RecursiveMode::NonRecursive)
        .into_diagnostic()
        .wrap_err_with(|| format!("failed to watch directory: {}", parent.display()))?;

    if let Err(e) = cmd_generate(
        input,
        out,
        targets,
        false,
        config_path,
        false,
        false,
        false,
        quiet,
    ) {
        eprintln!("error: {e:?}");
    }
    if !quiet {
        println!("Watching...");
    }

    let debounce = Duration::from_millis(500);
    let mut pending: Option<Instant> = None;
    loop {
        let timeout = match pending {
            Some(t) => debounce
                .saturating_sub(t.elapsed())
                .max(Duration::from_millis(10)),
            None => Duration::from_secs(60),
        };
        match rx.recv_timeout(timeout) {
            Ok(Ok(event)) => {
                if matches!(
                    event.kind,
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Any
                ) && event.paths.iter().any(|p| {
                    std::fs::canonicalize(p)
                        .map(|c| c == abs_input)
                        .unwrap_or(false)
                }) {
                    pending = Some(Instant::now());
                }
            }
            Ok(Err(e)) => eprintln!("watch error: {e}"),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("file watcher disconnected unexpectedly");
            }
        }
        if let Some(t) = pending {
            if debounce_should_fire(t, Instant::now(), debounce) {
                pending = None;
                if let Err(e) = cmd_generate(
                    input,
                    out,
                    targets,
                    false,
                    config_path,
                    false,
                    false,
                    false,
                    quiet,
                ) {
                    eprintln!("error: {e:?}");
                } else if !quiet {
                    let now = chrono_local_time_string();
                    println!("Regenerated at {now}");
                }
            }
        }
    }
}

/// Format a `HH:MM:SS` timestamp from the system clock without pulling in a
/// chrono dependency. Computes hours/minutes/seconds from the seconds-since-
/// epoch, applying the local UTC offset by reading `localtime`'s difference.
fn chrono_local_time_string() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let local_offset = local_utc_offset_seconds();
    let local = now as i64 + local_offset;
    let secs = local.rem_euclid(86_400);
    let h = secs / 3600;
    let m = (secs % 3600) / 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

/// Best-effort local timezone offset in seconds. Falls back to UTC (`0`) on
/// platforms without an obvious way to query the offset; the watch command
/// only uses this for the friendly "Regenerated at HH:MM:SS" line.
fn local_utc_offset_seconds() -> i64 {
    if let Ok(out) = Command::new("date").arg("+%z").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if s.len() == 5 {
                let sign = if s.starts_with('-') { -1 } else { 1 };
                if let (Ok(h), Ok(m)) = (s[1..3].parse::<i64>(), s[3..5].parse::<i64>()) {
                    return sign * (h * 3600 + m * 60);
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_should_fire_after_window_elapses() {
        use std::time::{Duration, Instant};
        let last = Instant::now();
        let debounce = Duration::from_millis(500);
        assert!(!debounce_should_fire(last, last, debounce));
        assert!(!debounce_should_fire(
            last,
            last + Duration::from_millis(499),
            debounce
        ));
        assert!(debounce_should_fire(
            last,
            last + Duration::from_millis(500),
            debounce
        ));
        assert!(debounce_should_fire(
            last,
            last + Duration::from_secs(1),
            debounce
        ));
    }

    #[test]
    fn debounce_handles_now_before_last_event() {
        use std::time::{Duration, Instant};
        let now = Instant::now();
        let later = now + Duration::from_secs(1);
        assert!(!debounce_should_fire(
            later,
            now,
            Duration::from_millis(500)
        ));
    }

    #[test]
    fn debounce_collapses_burst_to_single_fire() {
        use std::time::{Duration, Instant};
        let debounce = Duration::from_millis(500);
        let t0 = Instant::now();
        let burst = [
            t0,
            t0 + Duration::from_millis(50),
            t0 + Duration::from_millis(120),
            t0 + Duration::from_millis(200),
        ];
        let last = *burst.last().unwrap();
        for &t in &burst {
            assert!(
                !debounce_should_fire(last, t, debounce),
                "burst event at {:?} after last must not fire",
                t.duration_since(t0)
            );
        }
        assert!(debounce_should_fire(
            last,
            last + Duration::from_millis(500),
            debounce
        ));
    }
}
