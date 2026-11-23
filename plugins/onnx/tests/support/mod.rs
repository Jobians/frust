//! Shared fixtures for the integration tests.

#![allow(dead_code)] // each test binary uses its own subset

use std::path::PathBuf;

use frust_onnx::{Session, Tensor};

/// `tests/fixtures/add_f32.onnx`: f32 inputs `a` and `b`, both `[1, 3]`; output `sum = a + b`.
pub fn add_f32_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/add_f32.onnx")
}

pub fn add_f32_bytes() -> Vec<u8> {
    std::fs::read(add_f32_path()).expect("the add_f32 fixture is readable")
}

pub fn add_f32_session() -> Session {
    Session::from_file(add_f32_path()).expect("the add_f32 fixture loads")
}

pub fn row(values: [f32; 3]) -> Tensor {
    Tensor::from_f32([1, 3], values.to_vec()).expect("a [1, 3] tensor holds three values")
}

/// Owned inputs for the add model, as `Session::run` takes them.
pub fn add_inputs(a: [f32; 3], b: [f32; 3]) -> Vec<(String, Tensor)> {
    vec![("a".to_owned(), row(a)), ("b".to_owned(), row(b))]
}
