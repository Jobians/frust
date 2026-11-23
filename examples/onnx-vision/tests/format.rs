//! The download progress line and the other status text, pinned for each tracker state.

use frust_models::api::{DownloadProgress, DownloadState};
use onnxvision::format::{
    LOW_MEMORY_BYTES, low_memory_note, partial_line, progress_line, storage_line,
};

fn progress(bytes: u64, total: Option<u64>, bytes_per_sec: u64) -> DownloadProgress {
    DownloadProgress {
        bytes,
        total,
        bytes_per_sec,
        file_index: 0,
        files: 1,
    }
}

#[test]
fn an_idle_tracker_has_no_line() {
    assert_eq!(
        progress_line(&DownloadState::Idle, &DownloadProgress::default()),
        None
    );
}

#[test]
fn a_running_download_shows_bytes_percent_and_speed() {
    let line = progress_line(
        &DownloadState::Running,
        &progress(1_143_716, Some(4_574_861), 850_000),
    );
    assert_eq!(
        line.as_deref(),
        Some("1.1 MB of 4.6 MB (25%) at 850.0 KB/s")
    );
}

#[test]
fn a_download_without_a_total_shows_bytes_and_speed() {
    let line = progress_line(&DownloadState::Running, &progress(2_500, None, 1_200));
    assert_eq!(line.as_deref(), Some("2.5 KB at 1.2 KB/s"));
}

#[test]
fn a_download_that_has_not_received_bytes_is_starting() {
    let line = progress_line(&DownloadState::Running, &progress(0, Some(232_589), 0));
    assert_eq!(line.as_deref(), Some("Starting..."));
}

#[test]
fn the_terminal_states_say_what_happened() {
    let p = progress(13_629_400, Some(27_258_801), 0);
    assert_eq!(
        progress_line(&DownloadState::Verifying, &p).as_deref(),
        Some("Checking the file...")
    );
    assert_eq!(
        progress_line(&DownloadState::Done, &p).as_deref(),
        Some("Downloaded and verified")
    );
    assert_eq!(
        progress_line(&DownloadState::Cancelled, &p).as_deref(),
        Some("Cancelled at 13.6 MB of 27.3 MB (49%); Resume continues")
    );
    assert_eq!(
        progress_line(&DownloadState::Failed("timed out".into()), &p).as_deref(),
        Some("Failed: timed out")
    );
}

#[test]
fn a_kept_partial_file_offers_a_resume() {
    assert_eq!(
        partial_line(2_287_430, 4_574_861),
        "2.3 MB of 4.6 MB kept (49%); Resume continues"
    );
}

#[test]
fn the_storage_line_counts_finished_and_partial_files() {
    assert_eq!(storage_line(None, 0), "Measuring storage...");
    assert_eq!(storage_line(Some(0), 0), "Models use 0 B on this device");
    assert_eq!(
        storage_line(Some(4_807_450), 1_000),
        "Models use 4.8 MB on this device, plus 1.0 KB of partial downloads"
    );
}

#[test]
fn the_low_memory_note_appears_under_4_gb_only() {
    assert!(low_memory_note(LOW_MEMORY_BYTES).is_none());
    assert!(low_memory_note(8_000_000_000).is_none());
    let note = low_memory_note(3_700_000_000).expect("a 3.7 GB device gets the note");
    assert!(
        note.starts_with("This device has 3.7 GB of memory."),
        "{note}"
    );
}
