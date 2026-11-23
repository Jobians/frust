//! `VisionHandle` over a stub runner: the status order, the supersede rule and failures, with no
//! model and no ONNX Runtime session involved.

#![cfg(all(feature = "frust-api", feature = "vision"))]

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use frust::GetUntracked;
use frust_onnx::api::{VisionHandle, VisionOutput, VisionStatus};
use frust_onnx::catalog::VisionTask;
use frust_onnx::{Face, OnnxError, Plane, RgbaView};

const WAIT: Duration = Duration::from_secs(10);

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
        let last = self.settled.recv_timeout(WAIT).expect("the handle settles");
        let statuses = std::mem::take(&mut *self.statuses.lock().unwrap());
        assert_eq!(statuses.last(), Some(&last), "{statuses:?}");
        statuses
    }
}

fn logged(handle: VisionHandle) -> (VisionHandle, StatusLog) {
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
    // Width 1 finished while superseded, and its result was dropped.
    assert_eq!(handle.mask().get_untracked(), None);

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
