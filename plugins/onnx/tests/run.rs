//! End-to-end inference over the committed `add_f32` fixture, on the host's ONNX Runtime.

mod support;

use std::sync::Arc;
use std::thread;

use frust_onnx::{
    DType, LinkKind, OnnxError, Session, Tensor, TensorData, TensorInfo, runtime_info,
};
use support::{add_f32_bytes, add_f32_path, add_f32_session};

fn row(values: [f32; 3]) -> Tensor {
    Tensor::from_f32([1, 3], values.to_vec()).expect("a [1, 3] tensor holds three values")
}

fn run_sum(session: &Session, a: [f32; 3], b: [f32; 3]) -> Vec<f32> {
    let outputs = session
        .run_blocking(&[("a", row(a)), ("b", row(b))])
        .expect("the add model runs");
    assert_eq!(outputs.len(), 1);
    let (name, sum) = &outputs[0];
    assert_eq!(name, "sum");
    assert_eq!(sum.shape(), &[1, 3]);
    sum.as_f32().expect("sum is f32").to_vec()
}

fn invalid_input(result: Result<Vec<(String, Tensor)>, OnnxError>) -> String {
    match result {
        Err(OnnxError::InvalidInput(message)) => message,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn runtime_reports_a_release_and_static_link() {
    let info = runtime_info().expect("the runtime initialises");
    assert!(info.ort_version.starts_with("1."), "{info:?}");
    if cfg!(all(target_os = "linux", target_env = "gnu")) {
        // The prebuilt archive the exact `ort` pin downloads.
        assert_eq!(info.link, LinkKind::Static);
        assert_eq!(info.ort_version, "1.31.0");
    }
}

#[test]
fn loads_from_file_and_describes_io() {
    let session = add_f32_session();
    let expected = |name: &str| (name.to_owned(), vec![Some(1), Some(3)], DType::F32);
    let summary = |info: &TensorInfo| (info.name.clone(), info.shape.clone(), info.dtype.clone());
    let inputs: Vec<_> = session.inputs().iter().map(summary).collect();
    let outputs: Vec<_> = session.outputs().iter().map(summary).collect();
    assert_eq!(inputs, vec![expected("a"), expected("b")]);
    assert_eq!(outputs, vec![expected("sum")]);
}

#[test]
fn runs_from_file() {
    let session = add_f32_session();
    assert_eq!(
        run_sum(&session, [1.0, 2.0, 3.0], [10.0, 20.0, 30.0]),
        [11.0, 22.0, 33.0]
    );
}

#[test]
fn runs_from_bytes_with_inputs_in_any_order() {
    let session = Session::from_bytes(&add_f32_bytes()).expect("the fixture loads from bytes");
    assert_eq!(session.inputs().len(), 2);
    let outputs = session
        .run_blocking(&[("b", row([0.5, 0.5, 0.5])), ("a", row([1.0, -1.0, 0.0]))])
        .expect("the add model runs");
    assert_eq!(outputs[0].1.as_f32(), Some(&[1.5, -0.5, 0.5][..]));
}

#[test]
fn a_missing_file_is_a_load_error() {
    let path = add_f32_path().with_file_name("no_such_model.onnx");
    match Session::from_file(&path) {
        Err(OnnxError::Load {
            path: Some(reported),
            reason,
        }) => {
            assert_eq!(reported, path);
            assert!(!reason.is_empty());
        }
        other => panic!("expected a Load error, got {other:?}"),
    }
}

#[test]
fn bytes_that_are_not_a_model_are_a_load_error() {
    match Session::from_bytes(b"not an onnx model") {
        Err(OnnxError::Load { path: None, .. }) => {}
        other => panic!("expected a Load error, got {other:?}"),
    }
}

#[test]
fn an_unknown_input_name_is_refused() {
    let session = add_f32_session();
    let message = invalid_input(session.run_blocking(&[
        ("a", row([0.0; 3])),
        ("b", row([0.0; 3])),
        ("c", row([0.0; 3])),
    ]));
    assert!(message.contains("`c`"), "{message}");
}

#[test]
fn a_missing_input_is_refused() {
    let session = add_f32_session();
    let message = invalid_input(session.run_blocking(&[("a", row([0.0; 3]))]));
    assert!(message.contains("`b`"), "{message}");
}

#[test]
fn a_repeated_input_is_refused() {
    let session = add_f32_session();
    let message = invalid_input(session.run_blocking(&[
        ("a", row([0.0; 3])),
        ("a", row([0.0; 3])),
        ("b", row([0.0; 3])),
    ]));
    assert!(message.contains("twice"), "{message}");
}

#[test]
fn a_wrong_element_type_is_refused() {
    let session = add_f32_session();
    let bytes = Tensor::from_u8([1, 3], vec![1, 2, 3]).unwrap();
    let message = invalid_input(session.run_blocking(&[("a", bytes), ("b", row([0.0; 3]))]));
    assert!(
        message.contains("f32") && message.contains("u8"),
        "{message}"
    );
}

#[test]
fn a_wrong_static_dimension_is_refused() {
    let session = add_f32_session();
    let wide = Tensor::from_f32([1, 4], vec![0.0; 4]).unwrap();
    let message = invalid_input(session.run_blocking(&[("a", wide), ("b", row([0.0; 3]))]));
    assert!(message.contains("[1, 3]"), "{message}");
}

#[test]
fn a_wrong_rank_is_refused() {
    let session = add_f32_session();
    let flat = Tensor::from_f32([3], vec![0.0; 3]).unwrap();
    let message = invalid_input(session.run_blocking(&[("a", flat), ("b", row([0.0; 3]))]));
    assert!(message.contains("rank 2"), "{message}");
}

#[test]
fn tensor_constructors_refuse_a_length_that_misses_the_shape() {
    let refused = |result: Result<Tensor, OnnxError>| {
        assert!(
            matches!(result, Err(OnnxError::InvalidInput(_))),
            "{result:?}"
        );
    };
    refused(Tensor::from_f32([1, 3], vec![0.0; 2]));
    refused(Tensor::from_u8([2, 2], vec![0; 5]));
    refused(Tensor::from_i64([0], vec![7]));
    refused(Tensor::new(vec![], TensorData::F32(Vec::new())));
    refused(Tensor::from_u8([usize::MAX, 2], Vec::new()));
}

#[test]
fn tensor_constructors_accept_matching_data() {
    let scalar = Tensor::from_i64(Vec::<usize>::new(), vec![42]).unwrap();
    assert_eq!(
        (scalar.shape(), scalar.as_i64()),
        (&[][..], Some(&[42][..]))
    );
    assert_eq!(scalar.dtype(), DType::I64);

    let empty = Tensor::from_u8([2, 0], Vec::new()).unwrap();
    assert!(empty.is_empty());
    assert_eq!((empty.as_u8(), empty.as_f32()), (Some(&[][..]), None));

    let pixels = Tensor::from_u8([1, 2, 2], vec![1, 2, 3, 4]).unwrap();
    assert_eq!(pixels.len(), 4);
    let (shape, data) = pixels.into_parts();
    assert_eq!(
        (shape, data),
        (vec![1, 2, 2], TensorData::U8(vec![1, 2, 3, 4]))
    );
}

#[test]
fn two_threads_share_one_session() {
    let session = Arc::new(add_f32_session());
    let workers: Vec<_> = (0..2)
        .map(|worker| {
            let session = Arc::clone(&session);
            thread::spawn(move || {
                for step in 0..25 {
                    let base = (worker * 100 + step) as f32;
                    let sum = run_sum(&session, [base, base + 1.0, base + 2.0], [1.0, 1.0, 1.0]);
                    assert_eq!(sum, [base + 1.0, base + 2.0, base + 3.0]);
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("a worker thread finished cleanly");
    }
}

#[test]
fn session_is_send_and_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Session>();
}
