//! The text the screens show, as pure functions over plain values so the tests can pin it.

use frust_models::api::{DownloadProgress, DownloadState};
use frust_onnx::VisionRun;
use frust_onnx::catalog::VisionTask;

/// Devices with less total memory than this get the low-memory note (decimal, as phones are
/// sold: a "4 GB" phone usually reports a little under it and gets the note too).
pub const LOW_MEMORY_BYTES: u64 = 4_000_000_000;

/// A byte count in decimal units with one decimal place above a kilobyte: `512 B`, `4.6 MB`.
pub fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if n < 1000 {
        return format!("{n} B");
    }
    let mut value = n as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// Whole percent of `done` over `total`, rounded down and capped at 100; `None` without a total.
pub fn percent(done: u64, total: Option<u64>) -> Option<u64> {
    let total = total.filter(|&t| t > 0)?;
    Some((u128::from(done) * 100 / u128::from(total)).min(100) as u64)
}

/// A transfer rate: `850.0 KB/s`.
pub fn speed(bytes_per_sec: u64) -> String {
    format!("{}/s", bytes(bytes_per_sec))
}

/// The progress line under a catalog row, or `None` when the tracker has nothing to say.
pub fn progress_line(state: &DownloadState, progress: &DownloadProgress) -> Option<String> {
    let amount = || match (progress.total, percent(progress.bytes, progress.total)) {
        (Some(total), Some(pct)) => {
            format!("{} of {} ({pct}%)", bytes(progress.bytes), bytes(total))
        }
        _ => bytes(progress.bytes),
    };
    match state {
        DownloadState::Idle => None,
        DownloadState::Running if progress.bytes == 0 => Some("Starting...".to_owned()),
        DownloadState::Running => {
            Some(format!("{} at {}", amount(), speed(progress.bytes_per_sec)))
        }
        DownloadState::Verifying => Some("Checking the file...".to_owned()),
        DownloadState::Done => Some("Downloaded and verified".to_owned()),
        DownloadState::Cancelled => Some(format!("Cancelled at {}; Resume continues", amount())),
        DownloadState::Failed(message) => Some(format!("Failed: {message}")),
    }
}

/// The line under a row that is neither installed nor downloading but left a partial file.
pub fn partial_line(partial: u64, size: u64) -> String {
    let pct = percent(partial, Some(size)).unwrap_or(0);
    format!(
        "{} of {} kept ({pct}%); Resume continues",
        bytes(partial),
        bytes(size)
    )
}

/// The storage line at the top of the Models screen.
pub fn storage_line(total: Option<u64>, partial: u64) -> String {
    match (total, partial) {
        (None, _) => "Measuring storage...".to_owned(),
        (Some(total), 0) => format!("Models use {} on this device", bytes(total)),
        (Some(total), partial) => format!(
            "Models use {} on this device, plus {} of partial downloads",
            bytes(total),
            bytes(partial)
        ),
    }
}

/// The low-memory note, when `total_bytes` of RAM is under [`LOW_MEMORY_BYTES`].
pub fn low_memory_note(total_bytes: u64) -> Option<String> {
    (total_bytes < LOW_MEMORY_BYTES).then(|| {
        format!(
            "This device has {} of memory. One model session can peak near 380 MB: close other \
             apps before running a task, and prefer one task at a time.",
            bytes(total_bytes)
        )
    })
}

/// What a vision task is called in the UI.
pub fn task_label(task: VisionTask) -> &'static str {
    match task {
        VisionTask::Segmentation => "Subject mask",
        VisionTask::FaceDetection => "Faces",
        VisionTask::Depth => "Depth",
    }
}

/// What a catalog row's task column says.
pub fn task_kind(task: VisionTask) -> &'static str {
    match task {
        VisionTask::Segmentation => "Segmentation",
        VisionTask::FaceDetection => "Face detection",
        VisionTask::Depth => "Depth",
    }
}

/// The status line after `task`'s last run, as `VisionHandle::last_run` reports it:
/// `Depth: 412 ms on CPU (includes loading the model)`.
pub fn timing_line(task: VisionTask, run: &VisionRun) -> String {
    let provider = run
        .provider
        .map_or_else(|| "an unknown provider".to_owned(), |p| p.to_string());
    let loaded = if run.loaded_model {
        " (includes loading the model)"
    } else {
        ""
    };
    format!(
        "{}: {} ms on {provider}{loaded}",
        task_label(task),
        run.duration.as_millis()
    )
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use frust_onnx::Provider;

    use super::*;

    #[test]
    fn bytes_pick_a_readable_unit() {
        assert_eq!(bytes(0), "0 B");
        assert_eq!(bytes(999), "999 B");
        assert_eq!(bytes(1000), "1.0 KB");
        assert_eq!(bytes(232_589), "232.6 KB");
        assert_eq!(bytes(4_574_861), "4.6 MB");
        assert_eq!(bytes(27_258_801), "27.3 MB");
        assert_eq!(bytes(999_960), "1.0 MB");
        assert_eq!(bytes(3_700_000_000), "3.7 GB");
    }

    #[test]
    fn percent_needs_a_total_and_never_passes_100() {
        assert_eq!(percent(50, None), None);
        assert_eq!(percent(50, Some(0)), None);
        assert_eq!(percent(1, Some(3)), Some(33));
        assert_eq!(percent(7, Some(5)), Some(100));
    }

    #[test]
    fn timing_names_the_provider_and_a_run_that_loaded_the_model() {
        let run = VisionRun {
            provider: Some(Provider::Cpu),
            duration: Duration::from_micros(412_900),
            loaded_model: false,
        };
        assert_eq!(timing_line(VisionTask::Depth, &run), "Depth: 412 ms on CPU");
        let loading = VisionRun {
            provider: None,
            loaded_model: true,
            ..run
        };
        assert_eq!(
            timing_line(VisionTask::Depth, &loading),
            "Depth: 412 ms on an unknown provider (includes loading the model)"
        );
    }
}
