//! `frust-charts`: live charts for Frust apps, platform-free and built on the facade alone.
//!
//! Data lives in shared handles, not in view state: a [`Series`] is a fixed-capacity ring of
//! evenly spaced samples (newest last, `None` a gap), a [`TimedSeries`] keeps timestamped
//! points bounded by a count and/or a time window, and [`Samples`] is a fixed set of current
//! values. All are cheap to clone and `Send + Sync`, so a background thread writes while a
//! chart paints. Charts repaint when a handle is written, with no rebuild and no frame loop:
//! each write bumps the handle's revision and calls [`frust::request_repaint`] once.
//!
//! Charts are display-only and tap-transparent by default, so a tap reaches whatever lies
//! beneath. Setting `on_scrub` on a `time_series` opts in to scrubbing: its plot then handles
//! presses. A [`YAxis`] adds tick rules and labels, also opt-in; on an automatic domain it asks
//! for one layout pass when new data changes its ticks, the only time a write leads to more
//! than a repaint. A chart's colours come from the caller first, then the installed theme,
//! then the fallback constants on [`Tone`]. [`YDomain`] and [`nice_max`] pick the y range.
//!
//! ```
//! use frust_charts::{Samples, Series, YDomain};
//!
//! let load = Series::new(120); // two minutes at one sample a second
//! let worker = load.clone();
//! std::thread::spawn(move || worker.push(42.0)).join().unwrap();
//! assert_eq!(load.latest(), Some(42.0));
//! assert_eq!(YDomain::auto_from_zero().resolve(load.max()), (0.0, 50.0));
//!
//! let cores = Samples::new(8);
//! cores.slot(3).set(97.5);
//! ```

mod axis;
mod bar;
mod candlestick;
mod categories;
mod gauge;
mod geometry;
mod heat;
mod label;
mod meter;
mod pie;
mod polar;
mod radar;
mod samples;
mod scale;
mod scatter;
mod scrub;
mod series;
mod sparkline;
mod time_series;
mod timed;
mod tone;

pub use axis::{AxisSide, TickFormat, YAxis, ticks};
pub use categories::{Categories, CategoryRow};
pub use polar::{
    angle_of, arc_path, normalize_angle, point_on_circle, sector_path, sweep_contains,
};
pub use samples::{SampleSlot, Samples};
pub use scale::{YDomain, nice_max};
pub use scrub::{ScrubEvent, ScrubSample};
pub use series::{Series, SlotsView};
pub use timed::{SeriesSource, TimedSeries};
pub use tone::Tone;

#[allow(unused_imports)]
pub use bar::*;
#[allow(unused_imports)]
pub use candlestick::*;
#[allow(unused_imports)]
pub use gauge::*;
pub use geometry::*;
pub use heat::*;
pub use meter::*;
#[allow(unused_imports)]
pub use pie::*;
#[allow(unused_imports)]
pub use radar::*;
#[allow(unused_imports)]
pub use scatter::*;
pub use sparkline::*;
pub use time_series::*;
