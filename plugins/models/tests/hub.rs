#![cfg(feature = "hub")]

mod support;

use std::fs;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use frust_models::{
    DownloadEvent, DownloadItem, DownloadRequest, DownloadSink, Downloader, Gated, HubClient,
    HubError, ModelId, ModelSource, ModelStore, ModelsError, SearchOptions,
};
use support::Temp;
use support::http_fixture::{
    BLOB, FOREIGN_PAGE_REPO, Fixture, NESTED, OFF_LOOPBACK_HOST, PAGED_REPO, REPO, TOKENIZER,
};

const MIB: u64 = 1024 * 1024;

fn client(fx: &Fixture) -> HubClient {
    HubClient::new().unwrap().with_base_url(fx.hub.clone())
}

type Events = Arc<Mutex<Vec<DownloadEvent>>>;

fn recorder() -> (DownloadSink, Events) {
    let events: Events = Arc::default();
    let sink_events = events.clone();
    let sink: DownloadSink = Arc::new(move |e| sink_events.lock().unwrap().push(e));
    (sink, events)
}

fn is_terminal(e: &DownloadEvent) -> bool {
    matches!(
        e,
        DownloadEvent::Done(_) | DownloadEvent::Cancelled { .. } | DownloadEvent::Error(_)
    )
}

/// Checks the event stream shape: `Started` first, one terminal event last.
fn assert_stream(events: &[DownloadEvent]) {
    assert!(
        matches!(events.first(), Some(DownloadEvent::Started { .. })),
        "{events:?}"
    );
    assert_eq!(
        events.iter().filter(|e| is_terminal(e)).count(),
        1,
        "{events:?}"
    );
    assert!(is_terminal(events.last().unwrap()));
}

fn progress_bytes(events: &[DownloadEvent]) -> Vec<u64> {
    events
        .iter()
        .filter_map(|e| match e {
            DownloadEvent::Progress { bytes, .. } => Some(*bytes),
            _ => None,
        })
        .collect()
}

fn run(
    client: &HubClient,
    store: &ModelStore,
    req: DownloadRequest,
) -> (
    Result<Vec<frust_models::LocalModel>, ModelsError>,
    Vec<DownloadEvent>,
) {
    let (sink, events) = recorder();
    let result = Downloader::start(client, store, req, sink).unwrap().wait();
    let events = std::mem::take(&mut *events.lock().unwrap());
    (result, events)
}

#[test]
fn full_download_verifies_and_writes_manifest() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let req = client.request_for_main(REPO, &[BLOB]).unwrap();
    let (result, events) = run(&client, &store, req);
    let models = result.unwrap();
    assert_stream(&events);
    assert!(matches!(
        events[0],
        DownloadEvent::Started { total: Some(t), resumed_from: 0, files: 1 } if t == 3 * MIB
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, DownloadEvent::Verifying { file_index: 0 }))
    );
    assert_eq!(progress_bytes(&events).last(), Some(&(3 * MIB)));

    let expected = fx.file(BLOB);
    assert_eq!(models.len(), 1);
    let manifest = models[0].manifest.clone().unwrap();
    assert_eq!(manifest.sha256.as_deref(), Some(expected.sha256.as_str()));
    assert!(manifest.verified);
    assert_eq!(
        manifest.source,
        ModelSource::HuggingFace {
            repo: REPO.into(),
            revision: "main".into()
        }
    );
    assert_eq!(fs::read(&models[0].path).unwrap(), expected.bytes);
    let stored = store.get(&ModelId::hub(REPO, BLOB).unwrap()).unwrap();
    assert_eq!(stored.manifest, Some(manifest));
    assert!(store.in_progress().is_empty());
    let log = fx.log();
    assert!(
        log.iter()
            .any(|l| l.server == "cdn" && l.target == "/blobs/model.gguf")
    );
}

#[test]
fn finished_file_with_matching_digest_is_skipped() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let req = client.request_for_main(REPO, &[BLOB]).unwrap();
    run(&client, &store, req.clone()).0.unwrap();
    fx.clear_log();
    let (result, events) = run(&client, &store, req);
    assert_eq!(result.unwrap().len(), 1);
    assert!(matches!(
        events[0],
        DownloadEvent::Started { resumed_from, .. } if resumed_from == 3 * MIB
    ));
    assert!(fx.log().is_empty(), "{:?}", fx.log());
}

#[test]
fn cancel_keeps_part_and_second_start_resumes_with_range() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let req = client.request_for_main(REPO, &[BLOB]).unwrap();

    let (progress_tx, progress_rx) = mpsc::channel::<u64>();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let gate = Mutex::new(Some((progress_tx, gate_rx)));
    let (record, events) = recorder();
    let sink: DownloadSink = Arc::new(move |e| {
        let first = matches!(e, DownloadEvent::Progress { .. });
        let bytes = match &e {
            DownloadEvent::Progress { bytes, .. } => *bytes,
            _ => 0,
        };
        record(e);
        if first && let Some((tx, rx)) = gate.lock().unwrap().take() {
            tx.send(bytes).unwrap();
            rx.recv().unwrap();
        }
    });
    let handle = Downloader::start(&client, &store, req.clone(), sink).unwrap();
    let at = progress_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    handle.cancel();
    gate_tx.send(()).unwrap();
    assert!(matches!(handle.wait(), Err(ModelsError::Cancelled)));
    let events = std::mem::take(&mut *events.lock().unwrap());
    assert_stream(&events);
    let kept = match events.last() {
        Some(DownloadEvent::Cancelled { bytes_kept }) => *bytes_kept,
        other => panic!("{other:?}"),
    };
    assert!(kept >= at && kept > 0 && kept < 3 * MIB, "kept {kept}");
    let id = ModelId::hub(REPO, BLOB).unwrap();
    assert_eq!(store.in_progress(), vec![(id.clone(), kept)]);
    assert!(store.get(&id).is_none());

    fx.clear_log();
    let (result, events) = run(&client, &store, req);
    let models = result.unwrap();
    assert!(matches!(
        events[0],
        DownloadEvent::Started { resumed_from, .. } if resumed_from == kept
    ));
    let range = format!("bytes={kept}-");
    let cdn = fx
        .log()
        .into_iter()
        .find(|l| l.server == "cdn")
        .expect("cdn request");
    assert_eq!(cdn.range.as_deref(), Some(range.as_str()));
    let expected = fx.file(BLOB);
    assert_eq!(
        models[0].manifest.as_ref().unwrap().sha256.as_deref(),
        Some(expected.sha256.as_str())
    );
    assert_eq!(fs::read(&models[0].path).unwrap(), expected.bytes);
}

#[test]
fn ignored_range_restarts_from_zero_and_completes() {
    let fx = Fixture::start();
    fx.set_ignore_range(true);
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let expected = fx.file(BLOB);
    let id = ModelId::new("mirror", "tiny", "model.gguf").unwrap();
    let part = store.part_path_for(&id);
    fs::create_dir_all(part.parent().unwrap()).unwrap();
    fs::write(&part, &expected.bytes[..MIB as usize]).unwrap();
    let url = format!("{}/blobs/{BLOB}", fx.cdn);
    let req = DownloadRequest::from_url(
        url.clone(),
        id.clone(),
        Some(expected.sha256.clone()),
        Some(expected.bytes.len() as u64),
    )
    .unwrap();
    let (result, events) = run(&client, &store, req);
    let models = result.unwrap();
    assert!(matches!(
        events[0],
        DownloadEvent::Started { resumed_from, .. } if resumed_from == MIB
    ));
    assert_eq!(fx.log()[0].range.as_deref(), Some("bytes=1048576-"));
    assert_eq!(fs::read(&models[0].path).unwrap(), expected.bytes);
    let manifest = models[0].manifest.as_ref().unwrap();
    assert_eq!(manifest.source, ModelSource::Url { url });
    assert!(manifest.verified);
}

#[test]
fn wrong_digest_deletes_part_and_reports_integrity() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let id = ModelId::new("mirror", "tiny", "model.gguf").unwrap();
    let wrong = "00".repeat(32);
    let req = DownloadRequest::from_url(
        format!("{}/blobs/{BLOB}", fx.cdn),
        id.clone(),
        Some(wrong.clone()),
        None,
    )
    .unwrap();
    let (result, events) = run(&client, &store, req);
    assert_stream(&events);
    match result {
        Err(ModelsError::Hub(HubError::Integrity { expected, actual })) => {
            assert_eq!(expected, wrong);
            assert_eq!(actual, fx.file(BLOB).sha256);
        }
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        events.last(),
        Some(DownloadEvent::Error(ModelsError::Hub(
            HubError::Integrity { .. }
        )))
    ));
    assert!(!store.part_path_for(&id).exists());
    assert!(store.get(&id).is_none());
}

#[test]
fn max_bytes_stops_before_writing_past_the_limit() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    for route in ["blobs", "nolength"] {
        let id = ModelId::new("mirror", route, "model.gguf").unwrap();
        let mut req =
            DownloadRequest::from_url(format!("{}/{route}/{BLOB}", fx.cdn), id.clone(), None, None)
                .unwrap();
        req.max_bytes = Some(MIB);
        let (result, events) = run(&client, &store, req);
        assert_stream(&events);
        match result {
            Err(ModelsError::Hub(HubError::TooLarge { limit, actual })) => {
                assert_eq!(limit, MIB);
                assert!(actual > MIB);
            }
            other => panic!("{route}: {other:?}"),
        }
        assert!(progress_bytes(&events).iter().all(|b| *b <= MIB), "{route}");
        assert!(!store.part_path_for(&id).exists(), "{route}");
    }

    let id = ModelId::new("mirror", "declared", "model.gguf").unwrap();
    let mut req =
        DownloadRequest::from_url(format!("{}/blobs/{BLOB}", fx.cdn), id, None, Some(3 * MIB))
            .unwrap();
    req.max_bytes = Some(MIB);
    fx.clear_log();
    let (result, _) = run(&client, &store, req);
    assert!(matches!(
        result,
        Err(ModelsError::Hub(HubError::TooLarge { actual, .. })) if actual == 3 * MIB
    ));
    assert!(
        fx.log().is_empty(),
        "declared size is checked before any request"
    );
}

#[test]
fn status_codes_map_to_hub_errors() {
    let fx = Fixture::start();
    let client = client(&fx);
    let err = |repo: &str| match client.files(repo, "main") {
        Err(ModelsError::Hub(e)) => e,
        other => panic!("{repo}: {other:?}"),
    };
    assert_eq!(err("acme/gated"), HubError::Unauthorized { gated: true });
    assert_eq!(err("acme/private"), HubError::Unauthorized { gated: false });
    assert_eq!(err("acme/forbidden"), HubError::Forbidden);
    assert_eq!(
        err("acme/ratelimit"),
        HubError::RateLimited {
            retry_after: Some(Duration::from_secs(7))
        }
    );
    assert_eq!(err("acme/missing"), HubError::NotFound);
    match err("acme/teapot") {
        HubError::Status { code, body_excerpt } => {
            assert_eq!(code, 418);
            assert_eq!(body_excerpt.len(), 256);
        }
        other => panic!("{other:?}"),
    }

    let tmp = Temp::new();
    let store = tmp.store();
    let id = ModelId::new("mirror", "gated", "model.gguf").unwrap();
    let req = DownloadRequest::from_url(format!("{}/gated", fx.hub), id, None, None).unwrap();
    let (result, events) = run(&client, &store, req);
    assert_stream(&events);
    assert!(matches!(
        result,
        Err(ModelsError::Hub(HubError::Unauthorized { gated: true }))
    ));
}

#[test]
fn bundle_with_nested_path_completes_with_aggregate_progress() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let req = client.request_for_main(REPO, &[NESTED, TOKENIZER]).unwrap();
    assert_eq!(req.items[1].sha256, None, "small files have no LFS digest");
    let total = (fx.file(NESTED).bytes.len() + fx.file(TOKENIZER).bytes.len()) as u64;
    let (result, events) = run(&client, &store, req);
    assert_stream(&events);
    let models = result.unwrap();
    assert!(matches!(
        &events[0],
        DownloadEvent::Started { total: Some(t), files: 2, .. } if *t == total
    ));
    let bytes = progress_bytes(&events);
    assert!(bytes.windows(2).all(|w| w[0] <= w[1]), "{bytes:?}");
    assert_eq!(bytes.last(), Some(&total));
    assert!(events.iter().any(|e| matches!(
        e,
        DownloadEvent::Progress { file_index: 1, bytes, .. } if *bytes == total
    )));
    let Some(DownloadEvent::Done(done)) = events.last() else {
        panic!("{events:?}")
    };
    assert_eq!(done.len(), 2);
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id.path(), NESTED);
    assert_eq!(models[1].id.path(), TOKENIZER);
    assert_eq!(models[1].id.extension().as_deref(), Some("json"));
    assert_eq!(fs::read(&models[1].path).unwrap(), fx.file(TOKENIZER).bytes);
    let tokenizer = models[1].manifest.as_ref().unwrap();
    assert!(!tokenizer.verified);
    assert_eq!(
        tokenizer.sha256.as_deref(),
        Some(fx.file(TOKENIZER).sha256.as_str())
    );
    assert!(models[0].manifest.as_ref().unwrap().verified);
    assert_eq!(store.list().len(), 2);
}

#[test]
fn bundle_whose_second_file_fails_keeps_the_first() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let mut req = client.request_for_main(REPO, &[NESTED]).unwrap();
    req.items.push(DownloadItem {
        id: ModelId::new("acme", "tiny", "missing.bin").unwrap(),
        url: format!("{}/blobs/missing.bin", fx.cdn),
        source: ModelSource::Url {
            url: "missing".into(),
        },
        size: None,
        sha256: None,
    });
    let (result, events) = run(&client, &store, req);
    assert_stream(&events);
    assert!(matches!(result, Err(ModelsError::Hub(HubError::NotFound))));
    assert!(matches!(
        events.last(),
        Some(DownloadEvent::Error(ModelsError::Hub(HubError::NotFound)))
    ));
    assert!(store.get(&ModelId::hub(REPO, NESTED).unwrap()).is_some());
    assert_eq!(store.list().len(), 1);
}

#[test]
fn token_reaches_the_hub_and_never_the_redirect_target() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx).with_token(Some("hf_fixture_token".into()));
    let bearer = Some("Bearer hf_fixture_token".to_owned());

    let req = client.request_for_main(REPO, &[BLOB]).unwrap();
    run(&client, &store, req).0.unwrap();
    let log = fx.log();
    let hub: Vec<_> = log.iter().filter(|l| l.server == "hub").collect();
    assert_eq!(hub.len(), 2, "{log:?}");
    assert!(hub.iter().all(|l| l.authorization == bearer), "{log:?}");
    let cdn: Vec<_> = log.iter().filter(|l| l.server == "cdn").collect();
    assert_eq!(cdn.len(), 1, "{log:?}");
    assert_eq!(cdn[0].authorization, None);

    fx.clear_log();
    let id = ModelId::new("mirror", "redirect", "model.gguf").unwrap();
    let req =
        DownloadRequest::from_url(format!("{}/redirect/{BLOB}", fx.hub), id, None, None).unwrap();
    run(&client, &store, req).0.unwrap();
    let log = fx.log();
    assert_eq!(log[0].server, "hub");
    assert_eq!(log[0].authorization, bearer);
    assert_eq!(log[1].server, "cdn");
    assert_eq!(log[1].authorization, None);

    fx.clear_log();
    let id = ModelId::new("mirror", "direct", "model.gguf").unwrap();
    let req =
        DownloadRequest::from_url(format!("{}/blobs/{BLOB}", fx.cdn), id, None, None).unwrap();
    run(&client, &store, req).0.unwrap();
    assert_eq!(
        fx.log()[0].authorization,
        None,
        "a non-Hub origin never gets the token"
    );
}

#[test]
fn a_request_that_starts_off_the_hub_never_carries_the_token() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx).with_token(Some("hf_fixture_token".into()));
    let id = ModelId::new("mirror", "cdn-to-hub", "model.gguf").unwrap();
    let req =
        DownloadRequest::from_url(format!("{}/cdn-to-hub/{BLOB}", fx.cdn), id, None, None).unwrap();
    let models = run(&client, &store, req).0.unwrap();
    assert_eq!(fs::read(&models[0].path).unwrap(), fx.file(BLOB).bytes);
    let log = fx.log();
    let cdn: Vec<_> = log.iter().filter(|l| l.server == "cdn").collect();
    let hub: Vec<_> = log.iter().filter(|l| l.server == "hub").collect();
    assert_eq!(cdn.len(), 1, "{log:?}");
    assert_eq!(hub.len(), 1, "the redirect reached the Hub: {log:?}");
    assert_eq!(cdn[0].authorization, None, "{log:?}");
    assert_eq!(
        hub[0].authorization, None,
        "a request that started off the Hub never carries the token: {log:?}"
    );
}

#[test]
fn plain_http_is_refused_unless_loopback() {
    let id = ModelId::new("mirror", "tiny", "model.gguf").unwrap();
    assert!(matches!(
        DownloadRequest::from_url("http://example.com/model.gguf", id.clone(), None, None),
        Err(ModelsError::Hub(HubError::InsecureUrl))
    ));
    assert!(
        DownloadRequest::from_url("https://example.com/model.gguf", id.clone(), None, None).is_ok()
    );
    assert!(
        DownloadRequest::from_url("http://127.0.0.1:1/model.gguf", id.clone(), None, None).is_ok()
    );

    let tmp = Temp::new();
    let store = tmp.store();
    let mut req = DownloadRequest::from_url("https://example.com/m", id, None, None).unwrap();
    req.items[0].url = "http://example.com/m".into();
    let (sink, _) = recorder();
    assert!(matches!(
        Downloader::start(&HubClient::new().unwrap(), &store, req, sink),
        Err(ModelsError::Hub(HubError::InsecureUrl))
    ));
}

#[test]
fn redirect_to_plain_http_off_loopback_is_refused_before_connecting() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let id = ModelId::new("mirror", "insecure", "model.gguf").unwrap();
    let req = DownloadRequest::from_url(
        format!("{}/insecure/{BLOB}", fx.hub),
        id.clone(),
        None,
        None,
    )
    .unwrap();
    let (result, events) = run(&client, &store, req);
    assert_stream(&events);
    assert!(
        matches!(result, Err(ModelsError::Hub(HubError::InsecureUrl))),
        "{result:?}"
    );
    let part_len = fs::metadata(store.part_path_for(&id)).map_or(0, |m| m.len());
    assert_eq!(part_len, 0, "nothing was written");
    assert!(progress_bytes(&events).iter().all(|b| *b == 0));
    let log = fx.log();
    assert_eq!(log.len(), 1, "only the first hop was requested: {log:?}");
    assert!(log[0].target.starts_with("/insecure/"));
    assert!(
        store.get(&id).is_none(),
        "{OFF_LOOPBACK_HOST} served nothing"
    );
}

#[test]
fn five_redirects_are_followed_and_a_sixth_fails() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx).with_token(Some("hf_fixture_token".into()));
    let bearer = Some("Bearer hf_fixture_token".to_owned());

    let id = ModelId::new("mirror", "five", "tokenizer.json").unwrap();
    let req = DownloadRequest::from_url(format!("{}/hops/5/{TOKENIZER}", fx.hub), id, None, None)
        .unwrap();
    let models = run(&client, &store, req).0.unwrap();
    assert_eq!(fs::read(&models[0].path).unwrap(), fx.file(TOKENIZER).bytes);
    let log = fx.log();
    assert_eq!(log.len(), 6, "{log:?}");
    assert!(
        log.iter().all(|l| l.authorization == bearer),
        "every hop stays on the Hub origin, so each carries the token: {log:?}"
    );

    fx.clear_log();
    let id = ModelId::new("mirror", "six", "tokenizer.json").unwrap();
    let req = DownloadRequest::from_url(
        format!("{}/hops/6/{TOKENIZER}", fx.hub),
        id.clone(),
        None,
        None,
    )
    .unwrap();
    let (result, events) = run(&client, &store, req);
    assert_stream(&events);
    assert!(
        matches!(result, Err(ModelsError::Hub(HubError::TooManyRedirects))),
        "{result:?}"
    );
    let log = fx.log();
    assert_eq!(log.len(), 6, "{log:?}");
    assert!(log.iter().all(|l| !l.target.starts_with("/hops/0/")));
    assert!(store.get(&id).is_none());
}

#[test]
fn listing_pages_must_stay_on_the_base_origin() {
    let fx = Fixture::start();
    let client = client(&fx);
    let files = client.files(PAGED_REPO, "main").unwrap();
    let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, [BLOB, TOKENIZER]);
    assert_eq!(fx.log().len(), 2);

    fx.clear_log();
    assert!(matches!(
        client.files(FOREIGN_PAGE_REPO, "main"),
        Err(ModelsError::Hub(HubError::CrossOriginPage))
    ));
    let log = fx.log();
    assert_eq!(log.len(), 1, "the foreign page is never requested: {log:?}");
    assert_eq!(log[0].server, "hub");
}

#[test]
fn a_plain_http_base_off_loopback_is_refused_with_or_without_a_token() {
    let base = format!("http://{OFF_LOOPBACK_HOST}");
    for token in [None, Some("hf_secret".to_owned())] {
        let client = HubClient::new()
            .unwrap()
            .with_base_url(base.clone())
            .with_token(token);
        assert!(matches!(
            client.search(&SearchOptions::default()),
            Err(ModelsError::Hub(HubError::InsecureUrl))
        ));
        assert!(matches!(
            client.files(REPO, "main"),
            Err(ModelsError::Hub(HubError::InsecureUrl))
        ));
    }
}

#[test]
fn second_download_of_the_same_file_is_busy() {
    let fx = Fixture::start();
    let tmp = Temp::new();
    let store = tmp.store();
    let client = client(&fx);
    let req = client.request_for_main(REPO, &[BLOB]).unwrap();
    let (started_tx, started_rx) = mpsc::channel::<()>();
    let (gate_tx, gate_rx) = mpsc::channel::<()>();
    let gate = Mutex::new(Some((started_tx, gate_rx)));
    let sink: DownloadSink = Arc::new(move |e| {
        if matches!(e, DownloadEvent::Started { .. })
            && let Some((tx, rx)) = gate.lock().unwrap().take()
        {
            tx.send(()).unwrap();
            rx.recv().unwrap();
        }
    });
    let handle = Downloader::start(&client, &store, req.clone(), sink).unwrap();
    started_rx.recv_timeout(Duration::from_secs(30)).unwrap();
    let (other, _) = recorder();
    assert!(matches!(
        Downloader::start(&client, &store, req.clone(), other),
        Err(ModelsError::Busy)
    ));
    gate_tx.send(()).unwrap();
    handle.wait().unwrap();
    assert!(run(&client, &store, req).0.is_ok());

    let mut twice = client.request_for_main(REPO, &[BLOB]).unwrap();
    twice.items.push(twice.items[0].clone());
    let (sink, _) = recorder();
    assert!(matches!(
        Downloader::start(&client, &store, twice, sink),
        Err(ModelsError::Busy)
    ));
}

#[test]
fn search_parses_live_shapes_and_sends_the_query() {
    let fx = Fixture::start();
    let client = client(&fx);
    let models = client
        .search(&SearchOptions {
            query: Some("tiny".into()),
            filter: Some("gguf".into()),
            ..SearchOptions::default()
        })
        .unwrap();
    assert_eq!(models.len(), 3);
    assert_eq!(models[0].id, "acme/tiny");
    assert_eq!(models[0].gated, Gated::No);
    assert_eq!(models[0].downloads, 3400);
    assert_eq!(models[0].likes, 12);
    assert_eq!(
        models[0].last_modified.as_deref(),
        Some("2026-10-01T12:00:00.000Z")
    );
    assert!(models[0].files.contains(&NESTED.to_owned()));
    assert_eq!(models[1].gated, Gated::Manual);
    assert_eq!(models[2].gated, Gated::Auto);
    assert_eq!(models[2].author.as_deref(), Some("other"));
    assert!(models[2].files.is_empty());
    let target = &fx.log()[0].target;
    for part in [
        "search=tiny",
        "filter=gguf",
        "sort=downloads",
        "direction=-1",
        "limit=20",
        "full=true",
    ] {
        assert!(target.contains(part), "{target} lacks {part}");
    }
}

#[test]
fn tree_lists_files_with_sizes_and_lfs_digests() {
    let fx = Fixture::start();
    let files = client(&fx).files(REPO, "main").unwrap();
    let paths: Vec<_> = files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, [".gitattributes", BLOB, NESTED, TOKENIZER]);
    let blob = &files[1];
    assert_eq!(blob.size, 3 * MIB);
    assert_eq!(blob.sha256.as_deref(), Some(fx.file(BLOB).sha256.as_str()));
    assert_eq!(files[3].sha256, None);
    assert!(fx.log()[0].target.contains("recursive=true"));
    assert!(matches!(
        client(&fx).request_for_main(REPO, &["absent.bin"]),
        Err(ModelsError::Hub(HubError::NotFound))
    ));
}

#[test]
#[ignore = "downloads 1.2 MB from huggingface.co; run with `cargo test -p frust-models -- --ignored` on a networked host"]
fn live_hub_download_matches_the_tree_digest() {
    const LIVE_REPO: &str = "ggml-org/models";
    const LIVE_FILE: &str = "tinyllamas/stories260K.gguf";
    let tmp = Temp::new();
    let store = tmp.store();
    let client = HubClient::new().unwrap();
    let listed = client
        .files(LIVE_REPO, "main")
        .unwrap()
        .into_iter()
        .find(|f| f.path == LIVE_FILE)
        .expect("file listed");
    let sha = listed.sha256.clone().expect("lfs digest");
    let req = client.request_for_main(LIVE_REPO, &[LIVE_FILE]).unwrap();
    let (result, events) = run(&client, &store, req);
    assert_stream(&events);
    let models = result.unwrap();
    let manifest = models[0].manifest.as_ref().unwrap();
    assert_eq!(manifest.sha256.as_deref(), Some(sha.as_str()));
    assert_eq!(models[0].size_bytes, listed.size);
    assert!(manifest.verified);
}
