//! Typed tensors and the model's input/output descriptions.

use std::fmt;

use crate::OnnxError;

/// A tensor element type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DType {
    /// 32-bit float (`tensor(float)`).
    F32,
    /// Unsigned byte (`tensor(uint8)`).
    U8,
    /// 64-bit signed integer (`tensor(int64)`).
    I64,
    /// Any other type a model declares, named as the runtime names it; [`Tensor`] cannot hold it.
    Other(String),
}

impl fmt::Display for DType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DType::F32 => f.write_str("f32"),
            DType::U8 => f.write_str("u8"),
            DType::I64 => f.write_str("i64"),
            DType::Other(name) => f.write_str(name),
        }
    }
}

/// A tensor's elements, flat in row-major order.
#[derive(Debug, Clone, PartialEq)]
pub enum TensorData {
    F32(Vec<f32>),
    U8(Vec<u8>),
    I64(Vec<i64>),
}

impl TensorData {
    /// Number of elements.
    pub fn len(&self) -> usize {
        match self {
            TensorData::F32(v) => v.len(),
            TensorData::U8(v) => v.len(),
            TensorData::I64(v) => v.len(),
        }
    }

    /// True when there are no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// The element type.
    pub fn dtype(&self) -> DType {
        match self {
            TensorData::F32(_) => DType::F32,
            TensorData::U8(_) => DType::U8,
            TensorData::I64(_) => DType::I64,
        }
    }
}

/// A shaped tensor whose element count always equals the product of its shape (an empty shape is
/// a scalar: one element). Constructors refuse anything else with [`OnnxError::InvalidInput`].
#[derive(Debug, Clone, PartialEq)]
pub struct Tensor {
    shape: Vec<usize>,
    data: TensorData,
}

impl Tensor {
    /// A tensor of `shape` over `data`; fails if the lengths disagree.
    pub fn new(shape: impl Into<Vec<usize>>, data: TensorData) -> Result<Self, OnnxError> {
        let shape = shape.into();
        let expected = element_count(&shape)?;
        if expected != data.len() {
            return Err(OnnxError::InvalidInput(format!(
                "shape {shape:?} holds {expected} elements but {} were given",
                data.len()
            )));
        }
        Ok(Self { shape, data })
    }

    /// An f32 tensor; fails if `data.len()` is not the product of `shape`.
    pub fn from_f32(shape: impl Into<Vec<usize>>, data: Vec<f32>) -> Result<Self, OnnxError> {
        Self::new(shape, TensorData::F32(data))
    }

    /// A u8 tensor; fails if `data.len()` is not the product of `shape`.
    pub fn from_u8(shape: impl Into<Vec<usize>>, data: Vec<u8>) -> Result<Self, OnnxError> {
        Self::new(shape, TensorData::U8(data))
    }

    /// An i64 tensor; fails if `data.len()` is not the product of `shape`.
    pub fn from_i64(shape: impl Into<Vec<usize>>, data: Vec<i64>) -> Result<Self, OnnxError> {
        Self::new(shape, TensorData::I64(data))
    }

    /// The dimensions, outermost first.
    pub fn shape(&self) -> &[usize] {
        &self.shape
    }

    /// The element type.
    pub fn dtype(&self) -> DType {
        self.data.dtype()
    }

    /// The elements.
    pub fn data(&self) -> &TensorData {
        &self.data
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// True when the tensor has no elements (some dimension is zero).
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// The f32 elements, or `None` for another element type.
    pub fn as_f32(&self) -> Option<&[f32]> {
        match &self.data {
            TensorData::F32(v) => Some(v),
            _ => None,
        }
    }

    /// The u8 elements, or `None` for another element type.
    pub fn as_u8(&self) -> Option<&[u8]> {
        match &self.data {
            TensorData::U8(v) => Some(v),
            _ => None,
        }
    }

    /// The i64 elements, or `None` for another element type.
    pub fn as_i64(&self) -> Option<&[i64]> {
        match &self.data {
            TensorData::I64(v) => Some(v),
            _ => None,
        }
    }

    /// Splits the tensor into its shape and elements.
    pub fn into_parts(self) -> (Vec<usize>, TensorData) {
        (self.shape, self.data)
    }
}

/// One named model input or output, as the model declares it.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TensorInfo {
    /// The name a run addresses it by.
    pub name: String,
    /// Declared dimensions; `None` is a dynamic dimension (any size). Empty for a scalar, and for
    /// a non-tensor value (sequence, map), whose `dtype` is [`DType::Other`].
    pub shape: Vec<Option<usize>>,
    /// Declared element type.
    pub dtype: DType,
}

impl TensorInfo {
    pub(crate) fn new(name: String, shape: Vec<Option<usize>>, dtype: DType) -> Self {
        Self { name, shape, dtype }
    }
}

/// The product of `shape`, refusing one that overflows `usize`.
fn element_count(shape: &[usize]) -> Result<usize, OnnxError> {
    shape
        .iter()
        .try_fold(1usize, |acc, &dim| acc.checked_mul(dim))
        .ok_or_else(|| {
            OnnxError::InvalidInput(format!(
                "shape {shape:?} has more elements than fit in memory"
            ))
        })
}
