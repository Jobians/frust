#![cfg(all(feature = "frust-api", feature = "hub"))]

mod support;

use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
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

/// Every state a tracker published, recorded by its `on_state` observer, so no state can be
/// missed the way a sampled signal read can miss one.
struct StateLog {
    states: Arc<Mutex<Vec<DownloadState>>>,
    terminals: Receiver<DownloadState>,
}

impl StateLog {
    /// Waits for the next terminal state and returns every state recorded up to it, clearing
    /// the log for the next download.
    fn next_download(&self) -> Vec<DownloadState> {
        let terminal = self
            .terminals
            .recv_timeout(Duration::from_secs(30))
            .expect("a terminal state");
        let states = std::mem::take(&mut *self.states.lock().unwrap());
        assert_eq!(states.last(), Some(&terminal), "{states:?}");
        states
    }
}

fn logged(tracker: DownloadTracker) -> (DownloadTracker, StateLog) {
    let states: Arc<Mutex<Vec<DownloadState>>> = Arc::default();
    let (tx, terminals) = mpsc::channel();
    let record = states.clone();
    let tx = Mutex::new(tx);
    let tracker = tracker.on_state(move |state| {
        record.lock().unwrap().push(state.clone());
        if state.is_terminal() {
            let _ = tx.lock().unwrap().send(state.clone());
        }
    });
    (tracker, StateLog { states, terminals })
}

/// The order `DownloadState` documents for one download of `files` files: `Running` first,
/// then `Verifying` and `Running` alternating with at most one `Verifying` per file, then
/// exactly one terminal state, last.
fn assert_contract(states: &[DownloadState], files: usize) {
    assert_eq!(states.first(), Some(&DownloadState::Running), "{states:?}");
    let (terminal, before) = states.split_last().unwrap();
    assert!(terminal.is_terminal(), "{states:?}");
    assert!(
        before
            .iter()
            .all(|s| matches!(s, DownloadState::Running | DownloadState::Verifying)),
        "only Running and Verifying precede the terminal state: {states:?}"
    );
    assert!(
        states.windows(2).all(|w| w[0] != w[1]),
        "no state is published twice in a row: {states:?}"
    );
    let verifying = before
        .iter()
        .filter(|s| **s == DownloadState::Verifying)
        .count();
    assert!(verifying <= files, "{states:?}");
}

#[test]
fn a_hub_download_runs_to_done_and_refreshes_the_linked_list() {
    runtime();
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let list = ModelList::new(store.clone());
    let (tracker, log) = logged(DownloadTracker::new().with_list(list.clone()));
    assert_eq!(tracker.state().get_untracked(), DownloadState::Idle);
    assert!(list.models().get_untracked().is_empty());

    tracker
        .start_hub(&client, &store, REPO, "main", &[BLOB, TOKENIZER])
        .unwrap();
    let states = log.next_download();
    assert_contract(&states, 2);
    assert_eq!(states.last(), Some(&DownloadState::Done));
    assert_eq!(tracker.state().get_untracked(), DownloadState::Done);
    let progress = tracker.progress().get_untracked();
    assert_eq!(progress.files, 2);
    let total = 3 * 1024 * 1024 + fx.file(TOKENIZER).bytes.len() as u64;
    assert_eq!(progress.bytes, total);
    assert_eq!(progress.total, Some(total));

    // The list was refreshed before `Done` was published.
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
    let (tracker, log) = logged(DownloadTracker::new());

    let id = ModelId::new("mirror", "tiny", "model.gguf").unwrap();
    let url = format!("{}/{REPO}/resolve/main/{BLOB}", fx.hub);
    let wrong = "0".repeat(64);
    let request = DownloadRequest::from_url(url, id, Some(wrong), None).unwrap();
    tracker.start(&client, &store, request).unwrap();
    let states = log.next_download();
    assert_contract(&states, 1);
    assert!(
        matches!(states.last(), Some(DownloadState::Failed(msg)) if !msg.is_empty()),
        "{states:?}"
    );

    // The tracker is reusable once a download has ended.
    tracker
        .start_hub(&client, &store, REPO, "main", &[TOKENIZER])
        .unwrap();
    let states = log.next_download();
    assert_contract(&states, 1);
    assert_eq!(states.last(), Some(&DownloadState::Done));
}

#[test]
fn a_hub_request_that_cannot_be_built_ends_in_failed() {
    runtime();
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let (tracker, log) = logged(DownloadTracker::new());

    for (repo, path) in [("acme/missing", BLOB), (REPO, "absent.bin")] {
        tracker
            .start_hub(&client, &store, repo, "main", &[path])
            .unwrap();
        let states = log.next_download();
        assert_contract(&states, 1);
        assert_eq!(
            states,
            [
                DownloadState::Running,
                DownloadState::Failed("hub error: not found".into())
            ],
            "{repo}/{path}"
        );
    }
}

#[test]
fn cancel_mid_transfer_ends_in_cancelled_and_a_second_start_is_busy_meanwhile() {
    runtime();
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let (tracker, log) = logged(DownloadTracker::new());

    fx.hold_next_body();
    tracker
        .start_hub(&client, &store, REPO, "main", &[BLOB])
        .unwrap();
    // The file's body is parked after its first bytes, so the download cannot end meanwhile.
    fx.wait_held();
    assert_eq!(tracker.state().get_untracked(), DownloadState::Running);
    assert!(matches!(
        tracker.start_hub(&client, &store, REPO, "main", &[BLOB]),
        Err(ModelsError::Busy)
    ));
    let request = DownloadRequest::from_url(
        format!("{}/blobs/{TOKENIZER}", fx.cdn),
        ModelId::new("mirror", "tiny", "tokenizer.json").unwrap(),
        None,
        None,
    )
    .unwrap();
    assert!(matches!(
        tracker.start(&client, &store, request),
        Err(ModelsError::Busy)
    ));
    tracker.cancel();
    fx.release();

    let states = log.next_download();
    assert_contract(&states, 1);
    assert_eq!(states.last(), Some(&DownloadState::Cancelled));
    let id = ModelId::hub(REPO, BLOB).unwrap();
    assert!(store.get(&id).is_none());
    assert!(
        store
            .in_progress()
            .iter()
            .all(|(part, bytes)| *part == id && *bytes < 3 * 1024 * 1024),
        "{:?}",
        store.in_progress()
    );
}

#[test]
fn cancel_while_the_hub_request_is_built_ends_in_cancelled() {
    runtime();
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let (tracker, log) = logged(DownloadTracker::new());

    fx.hold_next_listing();
    tracker
        .start_hub(&client, &store, REPO, "main", &[TOKENIZER])
        .unwrap();
    // The listing the request is built from is parked, so no download has started yet.
    fx.wait_held();
    assert_eq!(tracker.state().get_untracked(), DownloadState::Running);
    tracker.cancel();
    fx.release();
    let states = log.next_download();
    assert_eq!(
        states,
        [DownloadState::Running, DownloadState::Cancelled],
        "{states:?}"
    );
    let log = fx.log();
    assert_eq!(log.len(), 1, "no file was requested: {log:?}");
    assert!(log[0].target.contains("/tree/main"));
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
