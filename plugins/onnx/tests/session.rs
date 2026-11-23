//! Session options, accelerator fall back, worker runs and the session cache, over the committed
//! `add_f32` fixture on the host's ONNX Runtime.

mod support;

use std::sync::mpsc;
use std::sync::{Arc, Barrier, Mutex};
use std::thread::{self, ThreadId};

use frust_onnx::{
    Accelerator, CoreMlUnits, OnnxError, Optimization, Provider, RunEvent, RunHandle, RunSink,
    Session, SessionCache, SessionOptions,
};
use support::{add_f32_path, add_inputs, row};

fn load(options: &SessionOptions) -> Result<Session, OnnxError> {
    Session::from_file_with(add_f32_path(), options)
}

fn sum_of(session: &Session) -> Vec<f32> {
    let outputs = session
        .run_blocking(&[("a", row([1.0, 2.0, 3.0])), ("b", row([10.0, 20.0, 30.0]))])
        .expect("the add model runs");
    outputs[0].1.as_f32().expect("sum is f32").to_vec()
}

/// What a sink saw: the thread it ran on and the event.
type Seen = Arc<Mutex<Vec<(Option<String>, ThreadId, RunEvent)>>>;

fn recording_sink() -> (RunSink, Seen) {
    let seen: Seen = Arc::default();
    let record = Arc::clone(&seen);
    let sink: RunSink = Arc::new(move |event| {
        let current = thread::current();
        let name = current.name().map(str::to_owned);
        record.lock().unwrap().push((name, current.id(), event));
    });
    (sink, seen)
}

fn only_event(seen: &Seen) -> RunEvent {
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "exactly one event: {seen:?}");
    seen[0].2.clone()
}

#[test]
fn options_round_trip() {
    let defaults = SessionOptions::default();
    assert_eq!(defaults.accelerator, Accelerator::Auto);
    assert!(!defaults.strict_accelerator);
    assert_eq!(defaults.intra_threads, None);
    assert_eq!(defaults.optimization, Optimization::All);
    assert!(!defaults.cpu_arena && !defaults.memory_pattern && !defaults.arena_shrinkage);

    let options = SessionOptions::default()
        .with_accelerator(Accelerator::CoreMl(CoreMlUnits::CpuAndGpu))
        .with_strict_accelerator(true)
        .with_intra_threads(2)
        .with_optimization(Optimization::Basic)
        .with_cpu_arena(true)
        .with_memory_pattern(true)
        .with_arena_shrinkage(true);
    assert_eq!(
        options.accelerator,
        Accelerator::CoreMl(CoreMlUnits::CpuAndGpu)
    );
    assert!(options.strict_accelerator);
    assert_eq!(options.intra_threads, Some(2));
    assert_eq!(options.optimization, Optimization::Basic);
    assert!(options.cpu_arena && options.memory_pattern && options.arena_shrinkage);
    assert_eq!(options.clone(), options);
    assert_ne!(options, defaults);

    let parallelism = thread::available_parallelism().map_or(1, |n| n.get());
    assert_eq!(defaults.resolved_intra_threads(), parallelism.min(4));
    assert_eq!(options.resolved_intra_threads(), 2);
    assert_eq!(
        SessionOptions::default()
            .with_intra_threads(0)
            .resolved_intra_threads(),
        1
    );

    let loaded_with = SessionOptions::default()
        .with_accelerator(Accelerator::Cpu)
        .with_intra_threads(1)
        .with_optimization(Optimization::Extended)
        .with_cpu_arena(true)
        .with_arena_shrinkage(true);
    let session = load(&loaded_with).expect("the fixture loads with explicit options");
    assert_eq!(session.options(), &loaded_with);
    assert_eq!(session.provider(), Provider::Cpu);
    assert_eq!(sum_of(&session), [11.0, 22.0, 33.0]);
}

#[test]
fn every_memory_combination_loads_and_runs() {
    for bits in 0..8u8 {
        let options = SessionOptions::default()
            .with_accelerator(Accelerator::Cpu)
            .with_cpu_arena(bits & 1 != 0)
            .with_memory_pattern(bits & 2 != 0)
            .with_arena_shrinkage(bits & 4 != 0);
        let session = load(&options).unwrap_or_else(|err| panic!("{options:?}: {err}"));
        for _ in 0..2 {
            assert_eq!(sum_of(&session), [11.0, 22.0, 33.0], "{options:?}");
        }
    }
}

#[test]
fn auto_reports_a_provider_and_runs() {
    let session = load(&SessionOptions::default()).expect("the fixture loads on Auto");
    let provider = session.provider();
    if cfg!(target_vendor = "apple") {
        assert!(matches!(provider, Provider::CoreMl | Provider::Cpu));
    } else {
        assert_eq!(provider, Provider::Cpu);
        assert_eq!(Accelerator::Auto.resolve(), Accelerator::Cpu);
    }
    assert_eq!(sum_of(&session), [11.0, 22.0, 33.0]);

    let bytes = Session::from_bytes_with(&support::add_f32_bytes(), &SessionOptions::default())
        .expect("the fixture loads from bytes on Auto");
    assert_eq!(sum_of(&bytes), [11.0, 22.0, 33.0]);
}

#[test]
fn an_unavailable_provider_falls_back_to_the_cpu() {
    assert!(!Provider::Xnnpack.is_compiled_in());
    let options = SessionOptions::default().with_accelerator(Accelerator::Xnnpack);
    let session = load(&options).expect("XNNPACK falls back to the CPU");
    assert_eq!(session.provider(), Provider::Cpu);
    assert_eq!(sum_of(&session), [11.0, 22.0, 33.0]);

    if !cfg!(target_vendor = "apple") {
        let coreml =
            SessionOptions::default().with_accelerator(Accelerator::CoreMl(CoreMlUnits::All));
        let session = load(&coreml).expect("CoreML falls back to the CPU off Apple");
        assert_eq!(session.provider(), Provider::Cpu);
    }
}

#[test]
fn strict_accelerator_turns_the_fall_back_into_unsupported() {
    let strict = SessionOptions::default()
        .with_accelerator(Accelerator::Xnnpack)
        .with_strict_accelerator(true);
    match load(&strict) {
        Err(OnnxError::Unsupported(message)) => assert!(message.contains("XNNPACK"), "{message}"),
        other => panic!("expected Unsupported, got {other:?}"),
    }

    let missing = add_f32_path().with_file_name("no_such_model.onnx");
    match Session::from_file_with(&missing, &strict) {
        Err(OnnxError::Load {
            path: Some(path), ..
        }) => assert_eq!(path, missing),
        other => panic!("a model the CPU cannot load either is a Load error, got {other:?}"),
    }

    let strict_auto = SessionOptions::default().with_strict_accelerator(true);
    let session = load(&strict_auto).expect("Auto never fails for want of a provider");
    assert_eq!(sum_of(&session), [11.0, 22.0, 33.0]);
}

#[test]
fn run_delivers_one_done_off_the_calling_thread() {
    let session = Arc::new(support::add_f32_session());
    let (sink, seen) = recording_sink();
    let handle = session.run(add_inputs([1.0, 2.0, 3.0], [0.5, 0.5, 0.5]), sink);
    handle.wait();
    assert!(handle.is_finished());
    let seen_on = seen.lock().unwrap()[0].clone();
    assert_eq!(seen_on.0.as_deref(), Some("frust-onnx-run"));
    assert_ne!(seen_on.1, thread::current().id());
    match only_event(&seen) {
        RunEvent::Done(outputs) => {
            assert_eq!(outputs.len(), 1);
            assert_eq!(outputs[0].0, "sum");
            assert_eq!(outputs[0].1.as_f32(), Some(&[1.5, 2.5, 3.5][..]));
        }
        other => panic!("expected Done, got {other:?}"),
    }

    handle.cancel();
    assert!(
        matches!(only_event(&seen), RunEvent::Done(_)),
        "a cancel after the outcome changes nothing"
    );
}

#[test]
fn refused_inputs_arrive_as_one_error() {
    let session = Arc::new(support::add_f32_session());
    let (sink, seen) = recording_sink();
    let inputs = vec![("a".to_owned(), row([0.0; 3]))];
    session.run(inputs, sink).wait();
    match only_event(&seen) {
        RunEvent::Error(OnnxError::InvalidInput(message)) => {
            assert!(message.contains("`b`"), "{message}");
        }
        other => panic!("expected an InvalidInput error, got {other:?}"),
    }
}

#[test]
fn cancel_before_start_delivers_cancelled() {
    let session = Arc::new(support::add_f32_session());
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let (first_sink, first_seen) = recording_sink();
    let blocking_sink: RunSink = Arc::new(move |event| {
        released.lock().unwrap().recv().ok();
        first_sink(event);
    });
    let first = session.run(add_inputs([1.0; 3], [1.0; 3]), blocking_sink);

    let (second_sink, second_seen) = recording_sink();
    let second = session.run(add_inputs([2.0; 3], [2.0; 3]), second_sink);
    second.cancel();
    assert!(!second.is_finished());
    release.send(()).unwrap();

    second.wait();
    first.wait();
    assert_eq!(only_event(&second_seen), RunEvent::Cancelled);
    assert!(matches!(only_event(&first_seen), RunEvent::Done(_)));
}

#[test]
fn runs_queue_in_order_and_keep_the_session_alive() {
    let session = Arc::new(support::add_f32_session());
    let order = Arc::new(Mutex::new(Vec::new()));
    let handles: Vec<RunHandle> = (0..5)
        .map(|step| {
            let order = Arc::clone(&order);
            let sink: RunSink = Arc::new(move |event| {
                assert!(matches!(event, RunEvent::Done(_)), "{event:?}");
                order.lock().unwrap().push(step);
            });
            session.run(add_inputs([step as f32; 3], [0.0; 3]), sink)
        })
        .collect();
    drop(session);
    for handle in &handles {
        handle.wait();
    }
    assert_eq!(*order.lock().unwrap(), vec![0, 1, 2, 3, 4]);
}

#[test]
fn a_panicking_sink_does_not_stop_the_worker() {
    let session = Arc::new(support::add_f32_session());
    let panicking: RunSink = Arc::new(|_| panic!("sink failure"));
    session
        .run(add_inputs([0.0; 3], [0.0; 3]), panicking)
        .wait();
    let (sink, seen) = recording_sink();
    session.run(add_inputs([1.0; 3], [1.0; 3]), sink).wait();
    assert!(matches!(only_event(&seen), RunEvent::Done(_)));
}

#[test]
fn run_handle_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RunHandle>();
    assert_send_sync::<SessionCache>();
}

/// Distinct cache keys over the one fixture file.
fn keyed(threads: usize) -> SessionOptions {
    SessionOptions::default()
        .with_accelerator(Accelerator::Cpu)
        .with_intra_threads(threads)
}

#[test]
fn a_cache_hit_returns_the_same_session() {
    let cache = SessionCache::default();
    assert_eq!(cache.max_sessions(), SessionCache::DEFAULT_MAX_SESSIONS);
    let first = cache.get_or_load(add_f32_path(), &keyed(1)).unwrap();
    let relative = add_f32_path()
        .parent()
        .unwrap()
        .join("../fixtures/add_f32.onnx");
    let second = cache.get_or_load(&relative, &keyed(1)).unwrap();
    assert!(Arc::ptr_eq(&first, &second), "a canonical path hits");
    let other = cache.get_or_load(add_f32_path(), &keyed(2)).unwrap();
    assert!(
        !Arc::ptr_eq(&first, &other),
        "other options are another key"
    );
    assert_eq!((cache.len(), cache.load_count()), (2, 2));
    assert_eq!(sum_of(&first), [11.0, 22.0, 33.0]);
}

#[test]
fn the_cache_evicts_the_least_recently_used() {
    let cache = SessionCache::new(2);
    let one = cache.get_or_load(add_f32_path(), &keyed(1)).unwrap();
    cache.get_or_load(add_f32_path(), &keyed(2)).unwrap();
    let one_again = cache.get_or_load(add_f32_path(), &keyed(1)).unwrap();
    assert!(Arc::ptr_eq(&one, &one_again));
    cache.get_or_load(add_f32_path(), &keyed(3)).unwrap();
    assert_eq!((cache.len(), cache.load_count()), (2, 3));

    cache.get_or_load(add_f32_path(), &keyed(1)).unwrap();
    assert_eq!(cache.load_count(), 3, "1 was used last, so 2 went");
    cache.get_or_load(add_f32_path(), &keyed(2)).unwrap();
    assert_eq!(cache.load_count(), 4, "2 was evicted and loads again");

    assert_eq!(cache.evict(add_f32_path()), 2);
    assert!(cache.is_empty());
    cache.get_or_load(add_f32_path(), &keyed(1)).unwrap();
    cache.clear();
    assert!(cache.is_empty());
    assert_eq!(
        sum_of(&one),
        [11.0, 22.0, 33.0],
        "an evicted session stays usable"
    );
}

#[test]
fn concurrent_get_or_load_loads_once() {
    let cache = Arc::new(SessionCache::default());
    let start = Arc::new(Barrier::new(8));
    let sessions: Vec<Arc<Session>> = (0..8)
        .map(|_| {
            let cache = Arc::clone(&cache);
            let start = Arc::clone(&start);
            thread::spawn(move || {
                start.wait();
                cache.get_or_load(add_f32_path(), &keyed(1)).unwrap()
            })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect();
    assert_eq!(cache.load_count(), 1);
    assert!(sessions.iter().all(|s| Arc::ptr_eq(s, &sessions[0])));
}

#[test]
fn a_failed_load_is_not_cached() {
    let cache = SessionCache::default();
    let missing = add_f32_path().with_file_name("no_such_model.onnx");
    assert!(matches!(
        cache.get_or_load(&missing, &keyed(1)),
        Err(OnnxError::Load { .. })
    ));
    let not_a_model = add_f32_path().with_file_name("README.md");
    assert!(matches!(
        cache.get_or_load(&not_a_model, &keyed(1)),
        Err(OnnxError::Load { .. })
    ));
    assert_eq!((cache.len(), cache.load_count()), (0, 0));
}

/// The ignored host measurement behind the README's memory table.
#[cfg(target_os = "linux")]
mod measure {
    use std::time::{Duration, Instant};

    use frust_onnx::{Optimization, Session, SessionOptions, Tensor};

    /// A `/proc/self/status` size of this process, in bytes: `VmHWM` (peak resident) or `VmRSS`.
    fn status_bytes(key: &str) -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").expect("/proc/self/status");
        let kib: u64 = status
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|kib| kib.parse().ok())
            .unwrap_or_else(|| panic!("a {key} line in kB"));
        kib * 1024
    }

    fn peak_rss() -> u64 {
        status_bytes("VmHWM:")
    }

    fn env_flag(name: &str) -> Option<bool> {
        std::env::var(name).ok().map(|value| value == "1")
    }

    /// One measurement per process: `FRUST_ONNX_MODEL` names a model with one f32 input
    /// (dynamic dimensions taken as 320); `FRUST_ONNX_ARENA`, `_PATTERN`, `_SHRINK` (`0`/`1`),
    /// `_THREADS`, `_OPT` (`basic`/`extended`/`all`), `_SESSIONS` and `_RUNS` override the
    /// defaults. Prints peak RSS before loading, after loading and after the runs, the resident
    /// size at the end, and the median ms per run.
    #[test]
    #[ignore = "needs a model file in FRUST_ONNX_MODEL; run one process per option combination \
                with `cargo test --release -p frust-onnx --test session -- --ignored --exact \
                measure::measure_model`"]
    fn measure_model() {
        let model = std::env::var_os("FRUST_ONNX_MODEL").expect("FRUST_ONNX_MODEL is set");
        let mut options = SessionOptions::default();
        if let Some(enable) = env_flag("FRUST_ONNX_ARENA") {
            options.cpu_arena = enable;
        }
        if let Some(enable) = env_flag("FRUST_ONNX_PATTERN") {
            options.memory_pattern = enable;
        }
        if let Some(enable) = env_flag("FRUST_ONNX_SHRINK") {
            options.arena_shrinkage = enable;
        }
        let count = |name: &str, default: usize| {
            std::env::var(name)
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(default)
        };
        if let Ok(level) = std::env::var("FRUST_ONNX_OPT") {
            options.optimization = match level.as_str() {
                "basic" => Optimization::Basic,
                "extended" => Optimization::Extended,
                _ => Optimization::All,
            };
        }
        if let Ok(threads) = std::env::var("FRUST_ONNX_THREADS") {
            options.intra_threads = Some(threads.parse().expect("FRUST_ONNX_THREADS is a number"));
        }
        let sessions = count("FRUST_ONNX_SESSIONS", 1);
        let runs = count("FRUST_ONNX_RUNS", 10);

        let before = peak_rss();
        let loaded: Vec<Session> = (0..sessions)
            .map(|_| Session::from_file_with(&model, &options).expect("the model loads"))
            .collect();
        let after_load = peak_rss();
        let info = loaded[0].inputs()[0].clone();
        let shape: Vec<usize> = info.shape.iter().map(|dim| dim.unwrap_or(320)).collect();
        let len = shape.iter().product();
        let pixels: Vec<f32> = (0..len).map(|i| (i % 255) as f32 / 255.0).collect();
        let input = Tensor::from_f32(shape, pixels).unwrap();
        let mut times = Vec::new();
        for session in &loaded {
            session
                .run_blocking(&[(info.name.as_str(), input.clone())])
                .expect("the warm-up run");
            for _ in 0..runs {
                let started = Instant::now();
                session
                    .run_blocking(&[(info.name.as_str(), input.clone())])
                    .expect("a run");
                times.push(started.elapsed());
            }
        }
        times.sort();
        let median = times
            .get(times.len() / 2)
            .copied()
            .unwrap_or(Duration::ZERO);
        let after = peak_rss();
        let resident = status_bytes("VmRSS:");
        let mib = |bytes: u64| bytes as f64 / (1024.0 * 1024.0);
        println!(
            "MEASURE opt={:?} threads={} arena={} pattern={} shrink={} sessions={sessions} \
             before_mib={:.1} loaded_mib={:.1} peak_mib={:.1} growth_mib={:.1} \
             rss_after_mib={:.1} median_ms={:.1}",
            options.optimization,
            options.resolved_intra_threads(),
            options.cpu_arena,
            options.memory_pattern,
            options.arena_shrinkage,
            mib(before),
            mib(after_load),
            mib(after),
            mib(after - before),
            mib(resident),
            median.as_secs_f64() * 1000.0,
        );
    }
}
