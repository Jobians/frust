//! Reactive handles over the vision tasks, behind the default-on `frust-api` feature.
//!
//! [`VisionHandle`] runs [`VisionModels`](crate::VisionModels)' tasks on a worker thread and
//! publishes each result and the handle's status as signals. It follows the rule a component's
//! `build` reads and handlers write, and every signal write is made on the UI thread: the worker
//! hands each result and status to the UI thread, which writes them in order on its next pump of
//! the local task queue. The worker never reads a signal and never blocks the UI thread. This
//! module is the only place in the crate that names `frust`, so `--no-default-features` drops the
//! facade from the dependency graph.

#[cfg(feature = "vision")]
mod publish;
#[cfg(feature = "vision")]
mod vision;

#[cfg(feature = "vision")]
pub use vision::{VisionHandle, VisionOutput, VisionStatus};
