#![cfg(all(feature = "frust-api", feature = "hub"))]

mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use frust::GetUntracked;
use frust_models::api::{DownloadState, DownloadTracker, ModelList};
use frust_models::{DownloadRequest, HubClient, ModelId, ModelsError};
use support::Temp;
use support::http_fixture::{BLOB, Fixture, REPO, TOKENIZER};

fn runtime() {
    frust_reactive::ReactiveRuntime::init(Arc::new(|| {}));
}

fn client(fx: &Fixture) -> HubClient {
    HubClient::new().unwrap().with_base_url(fx.hub.clone())
}

/// Polls `cond` until it holds, failing after ten seconds.
fn until(what: &str, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Records each distinct state seen while polling until a terminal one.
fn watch(tracker: &DownloadTracker) -> Vec<DownloadState> {
    let state = tracker.state();
    let mut seen = vec![state.get_untracked()];
    until("a terminal state", || {
        let now = state.get_untracked();
        if seen.last() != Some(&now) {
            seen.push(now);
        }
        !matches!(
            seen.last(),
            Some(DownloadState::Running | DownloadState::Verifying | DownloadState::Idle)
        )
    });
    seen
}

fn rank(state: &DownloadState) -> u8 {
    match state {
        DownloadState::Idle => 0,
        DownloadState::Running => 1,
        DownloadState::Verifying => 2,
        _ => 3,
    }
}

#[test]
fn a_download_runs_to_done_and_refreshes_the_linked_list() {
    runtime();
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let list = ModelList::new(store.clone());
    let tracker = DownloadTracker::new().with_list(list.clone());
    assert_eq!(tracker.state().get_untracked(), DownloadState::Idle);
    assert!(list.models().get_untracked().is_empty());

    let request = client.request_for_main(REPO, &[BLOB, TOKENIZER]).unwrap();
    tracker.start(&client, &store, request).unwrap();
    // `start` publishes `Running` before it returns; the thread can only move on from there.
    let seen = watch(&tracker);

    assert_eq!(seen.last(), Some(&DownloadState::Done), "{seen:?}");
    assert!(
        seen.windows(2).all(|w| rank(&w[0]) < rank(&w[1])),
        "states only move forward: {seen:?}"
    );
    let progress = tracker.progress().get_untracked();
    assert_eq!(progress.files, 2);
    let total = 3 * 1024 * 1024 + fx.file(TOKENIZER).bytes.len() as u64;
    assert_eq!(progress.bytes, total);
    assert_eq!(progress.total, Some(total));

    // The list was refreshed before `Done` became visible.
    let models = list.models().get_untracked();
    let mut ids: Vec<String> = models.iter().map(|m| m.id.to_string()).collect();
    ids.sort();
    assert_eq!(
        ids,
        vec![format!("{REPO}/{BLOB}"), format!("{REPO}/{TOKENIZER}")]
    );
}

#[test]
fn a_failed_download_reports_the_error_and_allows_a_new_start() {
    runtime();
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let tracker = DownloadTracker::new();

    let id = ModelId::new("mirror", "tiny", "model.gguf").unwrap();
    let url = format!("{}/{REPO}/resolve/main/{BLOB}", fx.hub);
    let wrong = "0".repeat(64);
    let request = DownloadRequest::from_url(url, id, Some(wrong), None).unwrap();
    tracker.start(&client, &store, request).unwrap();
    let seen = watch(&tracker);
    assert!(
        matches!(seen.last(), Some(DownloadState::Failed(msg)) if !msg.is_empty()),
        "{seen:?}"
    );

    // The tracker is reusable once a download has ended.
    let request = client.request_for_main(REPO, &[TOKENIZER]).unwrap();
    tracker.start(&client, &store, request).unwrap();
    assert_eq!(watch(&tracker).last(), Some(&DownloadState::Done));
}

#[test]
fn cancel_ends_in_cancelled_or_done_and_a_second_start_is_busy_meanwhile() {
    runtime();
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let tracker = DownloadTracker::new();

    let request = client.request_for_main(REPO, &[BLOB]).unwrap();
    tracker.start(&client, &store, request.clone()).unwrap();
    // Unless the download already ended, a second start is refused.
    if let Err(err) = tracker.start(&client, &store, request) {
        assert!(matches!(err, ModelsError::Busy), "{err}");
    }
    tracker.cancel();
    // A cancel that arrives after the last chunk finds a finished download.
    let seen = watch(&tracker);
    assert!(
        matches!(
            seen.last(),
            Some(DownloadState::Cancelled | DownloadState::Done)
        ),
        "{seen:?}"
    );
}

#[test]
fn list_refresh_and_remove_run_off_the_calling_thread() {
    runtime();
    let tmp = Temp::new();
    let store = tmp.store();
    let id = ModelId::new("local", "tiny", "config.json").unwrap();
    let source = tmp.0.join("config.json");
    std::fs::write(&source, b"{}").unwrap();
    store.import(&source, &id, None).unwrap();
    let list = ModelList::new(store);
    assert!(list.models().get_untracked().is_empty());

    list.refresh();
    let models = list.models();
    until("the list to fill", || models.get_untracked().len() == 1);
    assert_eq!(models.get_untracked()[0].id, id);

    list.remove(id);
    until("the list to empty", || models.get_untracked().is_empty());
}
