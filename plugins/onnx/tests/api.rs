//! `VisionHandle` over a stub runner: the status order, the supersede rule, failures, the
//! per-task run reports, and that every signal write lands on the UI thread, with no model and no
//! ONNX Runtime session involved.
//!
//! Each test starts the reactive runtime on its own thread, which makes that thread the UI
//! thread, and pumps its local task queue the way a shell does each frame.

#![cfg(all(feature = "frust-api", feature = "vision"))]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::{Duration, Instant};

use frust::{Get, GetUntracked};
use frust_models::ModelId;
use frust_onnx::api::{VisionHandle, VisionOutput, VisionStatus};
use frust_onnx::catalog::VisionTask;
use frust_onnx::{Face, OnnxError, Plane, Provider, RgbaView, SessionOptions, VisionModels};
use frust_reactive::{ReactiveRuntime, RepaintScope};

const WAIT: Duration = Duration::from_secs(10);

const TASKS: [VisionTask; 3] = [
    VisionTask::Segmentation,
    VisionTask::FaceDetection,
    VisionTask::Depth,
];

/// Starts the reactive runtime with the calling test thread as its UI thread, as a shell does.
fn runtime() -> &'static ReactiveRuntime {
    ReactiveRuntime::init(Arc::new(|| {}))
}

/// Pumps the UI thread's local queue, as a shell does each frame, until `rx` yields.
fn pump_recv<T>(rx: &Receiver<T>) -> T {
    let rt = runtime();
    let start = Instant::now();
    loop {
        rt.pump_local();
        if let Ok(value) = rx.try_recv() {
            return value;
        }
        assert!(start.elapsed() < WAIT, "timed out pumping the UI thread");
        thread::sleep(Duration::from_millis(1));
    }
}

/// Pumps the UI thread's local queue until `cond` holds.
fn pump_until(what: &str, mut cond: impl FnMut() -> bool) {
    let rt = runtime();
    let start = Instant::now();
    loop {
        rt.pump_local();
        if cond() {
            return;
        }
        assert!(start.elapsed() < WAIT, "timed out: {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

/// An opaque RGBA8 image of `width` x `height`.
fn image(width: u32, height: u32) -> Arc<[u8]> {
    vec![128u8; (width * height * 4) as usize].into()
}

/// A plane the size of `image`, every sample `value`.
fn plane_of(image: RgbaView<'_>, value: f32) -> Plane {
    let (w, h) = (image.width() as usize, image.height() as usize);
    Plane::new(w, h, vec![value; w * h]).unwrap()
}

fn face(score: f32) -> Face {
    Face {
        rect: [1.0, 2.0, 3.0, 4.0],
        landmarks: [[0.0; 2]; 5],
        score,
    }
}

/// Every status a handle published, recorded by its `on_status` observer, so none can be missed
/// the way a sampled signal read can miss one.
struct StatusLog {
    statuses: Arc<Mutex<Vec<VisionStatus>>>,
    settled: Receiver<VisionStatus>,
}

impl StatusLog {
    /// Waits for the next `Idle` or `Failed` and returns every status recorded up to it,
    /// clearing the log.
    fn next_settled(&self) -> Vec<VisionStatus> {
        let last = pump_recv(&self.settled);
        let statuses = std::mem::take(&mut *self.statuses.lock().unwrap());
        assert_eq!(statuses.last(), Some(&last), "{statuses:?}");
        statuses
    }
}

fn logged(handle: VisionHandle) -> (VisionHandle, StatusLog) {
    runtime();
    let statuses: Arc<Mutex<Vec<VisionStatus>>> = Arc::default();
    let (tx, settled) = mpsc::channel();
    let record = statuses.clone();
    let tx = Mutex::new(tx);
    let handle = handle.on_status(move |status| {
        record.lock().unwrap().push(status.clone());
        if matches!(status, VisionStatus::Idle | VisionStatus::Failed(_)) {
            let _ = tx.lock().unwrap().send(status.clone());
        }
    });
    (handle, StatusLog { statuses, settled })
}

#[test]
fn each_task_runs_on_the_worker_and_publishes_its_result_then_idle() {
    let (threads_tx, threads) = mpsc::channel::<String>();
    let threads_tx = Mutex::new(threads_tx);
    let (handle, log) = logged(VisionHandle::with_runner(move |task, image| {
        let name = std::thread::current().name().unwrap_or("").to_owned();
        threads_tx.lock().unwrap().send(name).unwrap();
        Ok(match task {
            VisionTask::Segmentation => VisionOutput::Plane(plane_of(image, 0.25)),
            VisionTask::FaceDetection => VisionOutput::Faces(vec![face(0.9), face(0.7)]),
            VisionTask::Depth => VisionOutput::Plane(plane_of(image, 0.75)),
        })
    }));
    assert_eq!(handle.status().get_untracked(), VisionStatus::Idle);
    assert_eq!(handle.mask().get_untracked(), None);
    assert!(handle.faces().get_untracked().is_empty());
    assert_eq!(handle.depth().get_untracked(), None);
    assert!(handle.missing_models().is_empty());

    handle.segment(image(4, 3), 4, 3);
    assert_eq!(
        log.next_settled(),
        [
            VisionStatus::Running(VisionTask::Segmentation),
            VisionStatus::Idle
        ]
    );
    let mask = handle.mask().get_untracked().expect("a mask");
    assert_eq!((mask.width, mask.height), (4, 3));
    assert!(mask.data.iter().all(|&v| v == 0.25));

    handle.detect_faces(image(2, 2), 2, 2, None);
    assert_eq!(
        log.next_settled(),
        [
            VisionStatus::Running(VisionTask::FaceDetection),
            VisionStatus::Idle
        ]
    );
    assert_eq!(*handle.faces().get_untracked(), [face(0.9), face(0.7)]);

    handle.estimate_depth(image(5, 1), 5, 1);
    assert_eq!(
        log.next_settled(),
        [VisionStatus::Running(VisionTask::Depth), VisionStatus::Idle]
    );
    let depth = handle.depth().get_untracked().expect("a depth map");
    assert_eq!((depth.width, depth.height), (5, 1));
    // The other results are left as they were.
    assert_eq!(handle.mask().get_untracked(), Some(mask));

    let names: Vec<String> = threads.try_iter().collect();
    assert_eq!(names.len(), 3);
    assert!(
        names.iter().all(|name| name == "frust-onnx-vision"),
        "{names:?}"
    );
}

/// Blocks each run until the test releases it, reporting the image width it started with.
struct Gate {
    started: Receiver<u32>,
    release: Sender<()>,
}

fn gated() -> (VisionHandle, StatusLog, Gate) {
    let (started_tx, started) = mpsc::channel();
    let (release, release_rx) = mpsc::channel::<()>();
    let started_tx = Mutex::new(started_tx);
    let release_rx = Mutex::new(release_rx);
    let (handle, log) = logged(VisionHandle::with_runner(move |task, image| {
        started_tx.lock().unwrap().send(image.width()).unwrap();
        release_rx.lock().unwrap().recv_timeout(WAIT).unwrap();
        Ok(match task {
            VisionTask::FaceDetection => VisionOutput::Faces(vec![face(image.width() as f32)]),
            _ => VisionOutput::Plane(plane_of(image, image.width() as f32)),
        })
    }));
    (handle, log, Gate { started, release })
}

#[test]
fn a_newer_request_supersedes_an_older_one_of_the_same_task() {
    let (handle, log, gate) = gated();
    handle.segment(image(1, 1), 1, 1);
    assert_eq!(gate.started.recv_timeout(WAIT).unwrap(), 1);
    // Width 2 is queued, then superseded unrun by width 3.
    handle.segment(image(2, 1), 2, 1);
    handle.segment(image(3, 1), 3, 1);
    assert_eq!(
        handle.status().get_untracked(),
        VisionStatus::Running(VisionTask::Segmentation)
    );

    gate.release.send(()).unwrap();
    // The next run is width 3: width 2 never ran.
    assert_eq!(gate.started.recv_timeout(WAIT).unwrap(), 3);
    // Width 1 finished while superseded, and its result was dropped, its run report with it.
    assert_eq!(handle.mask().get_untracked(), None);
    assert_eq!(
        handle.last_run(VisionTask::Segmentation).get_untracked(),
        None
    );

    gate.release.send(()).unwrap();
    assert_eq!(
        log.next_settled(),
        [
            VisionStatus::Running(VisionTask::Segmentation),
            VisionStatus::Idle
        ]
    );
    let mask = handle.mask().get_untracked().expect("the newest mask");
    assert_eq!(mask.width, 3);
    assert!(gate.started.try_recv().is_err(), "nothing else ran");
    // A plain stub runner names no provider.
    let run = handle
        .last_run(VisionTask::Segmentation)
        .get_untracked()
        .expect("the published run is reported");
    assert_eq!(run.provider, None);
}

#[test]
fn requests_for_different_tasks_do_not_supersede_each_other() {
    let (handle, log, gate) = gated();
    handle.segment(image(1, 1), 1, 1);
    assert_eq!(gate.started.recv_timeout(WAIT).unwrap(), 1);
    handle.estimate_depth(image(2, 1), 2, 1);
    handle.detect_faces(image(3, 1), 3, 1, None);
    for width in [2, 3] {
        gate.release.send(()).unwrap();
        assert_eq!(gate.started.recv_timeout(WAIT).unwrap(), width);
    }
    gate.release.send(()).unwrap();
    assert_eq!(
        log.next_settled(),
        [
            VisionStatus::Running(VisionTask::Segmentation),
            VisionStatus::Running(VisionTask::Depth),
            VisionStatus::Running(VisionTask::FaceDetection),
            VisionStatus::Idle,
        ]
    );
    assert_eq!(handle.mask().get_untracked().unwrap().width, 1);
    assert_eq!(handle.depth().get_untracked().unwrap().width, 2);
    assert_eq!(handle.faces().get_untracked()[0].score, 3.0);
}

#[test]
fn an_error_surfaces_as_failed_and_keeps_the_previous_result() {
    let fail = Arc::new(Mutex::new(false));
    let failing = fail.clone();
    let (handle, log) = logged(VisionHandle::with_runner(move |task, image| {
        if *failing.lock().unwrap() {
            return Err(OnnxError::ModelMissing {
                key: "u2netp".to_owned(),
            });
        }
        Ok(match task {
            VisionTask::Depth => VisionOutput::Faces(Vec::new()),
            _ => VisionOutput::Plane(plane_of(image, 1.0)),
        })
    }));

    handle.segment(image(2, 2), 2, 2);
    log.next_settled();
    let first = handle.mask().get_untracked().expect("a mask");

    *fail.lock().unwrap() = true;
    handle.segment(image(4, 4), 4, 4);
    let message = OnnxError::ModelMissing {
        key: "u2netp".to_owned(),
    }
    .to_string();
    assert_eq!(
        log.next_settled(),
        [
            VisionStatus::Running(VisionTask::Segmentation),
            VisionStatus::Failed(message.clone()),
        ]
    );
    assert_eq!(
        handle.status().get_untracked(),
        VisionStatus::Failed(message)
    );
    assert_eq!(handle.mask().get_untracked(), Some(first.clone()));

    // A buffer of the wrong size fails before the runner is called.
    *fail.lock().unwrap() = false;
    handle.segment(image(2, 2), 3, 3);
    let statuses = log.next_settled();
    assert_eq!(statuses[0], VisionStatus::Running(VisionTask::Segmentation));
    assert!(
        matches!(&statuses[1], VisionStatus::Failed(m) if m.starts_with("invalid input")),
        "{statuses:?}"
    );
    assert_eq!(handle.mask().get_untracked(), Some(first));

    // A runner answering with another task's result is a failure too, never a write.
    handle.estimate_depth(image(1, 1), 1, 1);
    let statuses = log.next_settled();
    assert!(
        matches!(statuses.last(), Some(VisionStatus::Failed(m)) if m.contains("Depth")),
        "{statuses:?}"
    );
    assert_eq!(handle.depth().get_untracked(), None);
    assert!(handle.faces().get_untracked().is_empty());

    // The next good request clears the failure.
    handle.segment(image(1, 1), 1, 1);
    assert_eq!(
        log.next_settled(),
        [
            VisionStatus::Running(VisionTask::Segmentation),
            VisionStatus::Idle
        ]
    );
}

#[test]
fn a_panicking_runner_is_a_failure_and_the_worker_keeps_serving() {
    let (handle, log) = logged(VisionHandle::with_runner(|_, image| {
        if image.width() == 1 {
            panic!("stub runner panic");
        }
        Ok(VisionOutput::Plane(plane_of(image, 0.0)))
    }));
    handle.segment(image(1, 1), 1, 1);
    let statuses = log.next_settled();
    assert!(
        matches!(statuses.last(), Some(VisionStatus::Failed(m)) if m.contains("panicked")),
        "{statuses:?}"
    );
    handle.segment(image(2, 1), 2, 1);
    assert_eq!(log.next_settled().last(), Some(&VisionStatus::Idle));
    assert_eq!(handle.mask().get_untracked().unwrap().width, 2);
}

#[test]
fn each_task_reports_the_provider_and_duration_of_its_last_published_run() {
    const DELAY: Duration = Duration::from_millis(30);
    let fail = Arc::new(Mutex::new(false));
    let failing = fail.clone();
    let (handle, log) = logged(VisionHandle::with_runner_on(
        Provider::Cpu,
        move |task, image| {
            std::thread::sleep(DELAY);
            if *failing.lock().unwrap() {
                return Err(OnnxError::Run("stub failure".to_owned()));
            }
            Ok(match task {
                VisionTask::FaceDetection => VisionOutput::Faces(Vec::new()),
                _ => VisionOutput::Plane(plane_of(image, 0.5)),
            })
        },
    ));
    let tasks = [
        VisionTask::Segmentation,
        VisionTask::FaceDetection,
        VisionTask::Depth,
    ];
    for task in tasks {
        assert_eq!(handle.last_run(task).get_untracked(), None, "{task:?}");
    }

    handle.segment(image(2, 2), 2, 2);
    log.next_settled();
    let first = handle
        .last_run(VisionTask::Segmentation)
        .get_untracked()
        .expect("the run is reported once its result is published");
    assert_eq!(first.provider, Some(Provider::Cpu));
    assert!(first.duration >= DELAY, "{first:?}");
    assert!(first.duration < WAIT, "{first:?}");
    // A stub runner loads no session.
    assert!(!first.loaded_model);
    // The other tasks have not run.
    assert_eq!(
        handle.last_run(VisionTask::FaceDetection).get_untracked(),
        None
    );
    assert_eq!(handle.last_run(VisionTask::Depth).get_untracked(), None);

    // A failed run leaves the last published run's report.
    *fail.lock().unwrap() = true;
    handle.segment(image(2, 2), 2, 2);
    assert!(matches!(
        log.next_settled().last(),
        Some(VisionStatus::Failed(_))
    ));
    assert_eq!(
        handle.last_run(VisionTask::Segmentation).get_untracked(),
        Some(first)
    );

    // Each task keeps its own report.
    *fail.lock().unwrap() = false;
    handle.detect_faces(image(3, 3), 3, 3, None);
    handle.estimate_depth(image(3, 3), 3, 3);
    assert_eq!(log.next_settled().last(), Some(&VisionStatus::Idle));
    for task in [VisionTask::FaceDetection, VisionTask::Depth] {
        let run = handle.last_run(task).get_untracked().expect("reported");
        assert_eq!(run.provider, Some(Provider::Cpu), "{task:?}");
        assert!(run.duration >= DELAY, "{task:?}: {run:?}");
    }
    assert_eq!(
        handle.last_run(VisionTask::Segmentation).get_untracked(),
        Some(first)
    );
}

/// Reads every signal of `handle` the way a component's `build` does.
fn read_all(handle: &VisionHandle) {
    let _ = handle.status().get();
    let _ = handle.mask().get();
    let _ = handle.faces().get();
    let _ = handle.depth().get();
    for task in TASKS {
        let _ = handle.last_run(task).get();
    }
}

/// The UI thread reads every signal in a tight loop, pumping its queue as a shell does each
/// frame, while 2,000 requests complete. A worker that wrote the signals itself made such a read
/// meet its write lock, which panics, or made its write meet the read and vanish.
#[test]
fn ui_reads_in_a_tight_loop_while_two_thousand_requests_complete() {
    const REQUESTS: usize = 2_000;
    let rt = runtime();
    let settled = Arc::new(AtomicUsize::new(0));
    let count = settled.clone();
    let handle = VisionHandle::with_runner(|task, image| {
        Ok(match task {
            VisionTask::FaceDetection => VisionOutput::Faces(vec![face(image.width() as f32)]),
            _ => VisionOutput::Plane(plane_of(image, image.width() as f32)),
        })
    })
    .on_status(move |status| {
        if *status == VisionStatus::Idle {
            count.fetch_add(1, Ordering::SeqCst);
        }
    });
    for i in 0..REQUESTS {
        let width = (i % 7 + 1) as u32;
        let before = settled.load(Ordering::SeqCst);
        match TASKS[i % TASKS.len()] {
            VisionTask::Segmentation => handle.segment(image(width, 1), width, 1),
            VisionTask::FaceDetection => handle.detect_faces(image(width, 1), width, 1, None),
            VisionTask::Depth => handle.estimate_depth(image(width, 1), width, 1),
        }
        let start = Instant::now();
        while settled.load(Ordering::SeqCst) == before {
            read_all(&handle);
            rt.pump_local();
            assert!(
                start.elapsed() < WAIT,
                "request {i} never settled: {:?}",
                handle.status().get()
            );
        }
    }
    read_all(&handle);
    assert_eq!(handle.status().get(), VisionStatus::Idle);
    // The last three requests, one per task, left their results.
    let last = |offset: usize| ((REQUESTS - offset) % 7 + 1) as u32;
    let by_task = |task: VisionTask| {
        (1..=TASKS.len())
            .find(|&offset| TASKS[(REQUESTS - offset) % TASKS.len()] == task)
            .map(last)
            .unwrap()
    };
    assert_eq!(
        handle.mask().get().map(|m| m.width as u32),
        Some(by_task(VisionTask::Segmentation))
    );
    assert_eq!(
        handle.depth().get().map(|d| d.width as u32),
        Some(by_task(VisionTask::Depth))
    );
    assert_eq!(
        handle.faces().get()[0].score,
        by_task(VisionTask::FaceDetection) as f32
    );
    for task in TASKS {
        assert!(handle.last_run(task).get().is_some(), "{task:?}");
    }
}

/// Records the thread of every write to the signals `track` reads, at the moment of the write.
fn write_threads(track: impl FnOnce()) -> (RepaintScope, Arc<Mutex<Vec<ThreadId>>>) {
    let threads: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let record = threads.clone();
    let scope = RepaintScope::every_change(Arc::new(move || {
        record.lock().unwrap().push(thread::current().id());
    }));
    scope.track(track);
    (scope, threads)
}

#[test]
fn every_signal_write_and_every_observer_call_runs_on_the_ui_thread() {
    let ui = thread::current().id();
    let observed: Arc<Mutex<Vec<ThreadId>>> = Arc::default();
    let record = observed.clone();
    runtime();
    let handle = VisionHandle::with_runner(|task, image| {
        Ok(match task {
            VisionTask::FaceDetection => VisionOutput::Faces(vec![face(0.9)]),
            _ => VisionOutput::Plane(plane_of(image, 0.5)),
        })
    })
    .on_status(move |_| record.lock().unwrap().push(thread::current().id()));
    let (_scope, writes) = write_threads(|| read_all(&handle));

    handle.segment(image(2, 2), 2, 2);
    handle.detect_faces(image(2, 2), 2, 2, None);
    handle.estimate_depth(image(2, 2), 2, 2);
    pump_until("every task to publish", || {
        handle.status().get_untracked() == VisionStatus::Idle
            && TASKS
                .iter()
                .all(|&task| handle.last_run(task).get_untracked().is_some())
    });
    // A failure is written the same way.
    handle.segment(image(2, 2), 3, 3);
    pump_until("the failure", || {
        matches!(handle.status().get_untracked(), VisionStatus::Failed(_))
    });

    let writes = writes.lock().unwrap().clone();
    // Three results, three run reports and at least `Running`, `Idle`, `Running`, `Failed`.
    assert!(writes.len() >= 10, "{writes:?}");
    assert!(writes.iter().all(|&id| id == ui), "{writes:?}");
    let observed = observed.lock().unwrap().clone();
    assert!(observed.len() >= 4, "{observed:?}");
    assert!(observed.iter().all(|&id| id == ui), "{observed:?}");
}

/// A status, the mask's width and whether the mask's run was reported, as an observer saw them.
type Seen = (VisionStatus, Option<usize>, bool);

#[test]
fn running_is_written_before_the_request_returns_and_a_result_before_the_idle_after_it() {
    // The log's observer is replaced below by one that also reads the signals.
    let (handle, _log, gate) = gated();
    // What the observer saw of the mask and its run report when each status was written.
    let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
    let record = seen.clone();
    let (mask, run) = (handle.mask(), handle.last_run(VisionTask::Segmentation));
    let handle = handle.on_status(move |status| {
        let width = mask.get_untracked().map(|m| m.width);
        let reported = run.get_untracked().is_some();
        record
            .lock()
            .unwrap()
            .push((status.clone(), width, reported));
    });

    handle.segment(image(4, 1), 4, 1);
    // No pump yet: the request call itself wrote `Running`.
    assert_eq!(
        handle.status().get_untracked(),
        VisionStatus::Running(VisionTask::Segmentation)
    );
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [(VisionStatus::Running(VisionTask::Segmentation), None, false)]
    );
    assert_eq!(gate.started.recv_timeout(WAIT).unwrap(), 4);
    gate.release.send(()).unwrap();
    let settled = || seen.lock().unwrap().last().map(|s| s.0.clone()) == Some(VisionStatus::Idle);
    pump_until("Idle", settled);
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&(VisionStatus::Idle, Some(4), true)),
        "the mask and its run report were written before Idle"
    );

    // From idle again, a second request: `Running` before it returns, the new mask by `Idle`.
    handle.segment(image(6, 1), 6, 1);
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&(
            VisionStatus::Running(VisionTask::Segmentation),
            Some(4),
            true
        ))
    );
    assert_eq!(gate.started.recv_timeout(WAIT).unwrap(), 6);
    gate.release.send(()).unwrap();
    pump_until("Idle", settled);
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&(VisionStatus::Idle, Some(6), true))
    );
}

#[test]
fn dropping_every_clone_mid_request_neither_panics_nor_leaks_the_task() {
    runtime();
    let alive = Arc::new(());
    let held = alive.clone();
    let (started_tx, started) = mpsc::channel::<()>();
    let (release, release_rx) = mpsc::channel::<()>();
    let (started_tx, release_rx) = (Mutex::new(started_tx), Mutex::new(release_rx));
    let handle = VisionHandle::with_runner(move |_, image| {
        let _held = &held;
        started_tx.lock().unwrap().send(()).unwrap();
        release_rx.lock().unwrap().recv_timeout(WAIT).unwrap();
        Ok(VisionOutput::Plane(plane_of(image, 1.0)))
    });
    let clone = handle.clone();
    handle.segment(image(2, 1), 2, 1);
    clone.estimate_depth(image(2, 1), 2, 1);
    started.recv_timeout(WAIT).unwrap();
    drop(handle);
    drop(clone);
    // Both queued requests finish with no handle left; their writes are applied and dropped.
    release.send(()).unwrap();
    release.send(()).unwrap();
    // The runner is freed once the worker has ended and the UI thread's task has dropped it.
    pump_until("the worker and the UI task to end", || {
        Arc::strong_count(&alive) == 1
    });
}

#[test]
fn a_request_off_the_ui_thread_panics_and_changes_nothing() {
    let (handle, log) = logged(VisionHandle::with_runner(|_, image| {
        Ok(VisionOutput::Plane(plane_of(image, 1.0)))
    }));
    // Before any request, `frust::spawn_local` refuses the call; after one, the handle does.
    for width in [1, 2] {
        let mask = handle.mask().get_untracked();
        let off = handle.clone();
        let outcome = thread::spawn(move || off.segment(image(5, 1), 5, 1)).join();
        assert!(outcome.is_err(), "a request off the UI thread panics");
        runtime().pump_local();
        assert_eq!(handle.status().get_untracked(), VisionStatus::Idle);
        assert_eq!(handle.mask().get_untracked(), mask);
        handle.segment(image(width, 1), width, 1);
        assert_eq!(log.next_settled().last(), Some(&VisionStatus::Idle));
        assert_eq!(handle.mask().get_untracked().unwrap().width, width as usize);
    }
}

/// The provider a model-backed handle reports comes from `VisionModels::loaded_provider`, which
/// reads the loaded session and never loads one itself.
#[test]
fn loaded_provider_reads_the_loaded_session_and_never_loads() {
    let store = support::empty_store("api-provider");
    let id = ModelId::new("local", "fixtures", "add_f32.onnx").unwrap();
    let models = VisionModels::new(store.clone(), SessionOptions::default())
        .with_model(VisionTask::Depth, id.clone());
    // Not in the store yet, then in the store but not loaded.
    assert_eq!(models.loaded_provider(VisionTask::Depth), None);
    store.import(&support::add_f32_path(), &id, None).unwrap();
    assert_eq!(models.loaded_provider(VisionTask::Depth), None);
    assert!(models.cache().is_empty(), "asking loads nothing");
    assert_eq!(models.cache().load_count(), 0);

    let path = store.get(&id).unwrap().path;
    let session = models.cache().get_or_load(&path, models.options()).unwrap();
    assert_eq!(
        models.loaded_provider(VisionTask::Depth),
        Some(session.provider())
    );
    // Another task's model is a different file: not loaded.
    assert_eq!(models.loaded_provider(VisionTask::Segmentation), None);
    models.cache().clear();
    assert_eq!(models.loaded_provider(VisionTask::Depth), None);
    assert_eq!(models.cache().load_count(), 1);
    let _ = std::fs::remove_dir_all(store.root());
}
