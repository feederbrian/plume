//! plume-core: the data side of Plume, a lightweight system monitor.
//!
//! No GTK dependency. Everything reads `/proc` and `/sys` directly, keeps the
//! files it polls open, and avoids allocation on the hot path.

pub mod actions;
pub mod cpu;
pub mod disk;
pub mod gpu;
pub mod history;
pub mod mem;
pub mod net;
pub mod netproc;
pub mod proc;
pub mod sampler;
pub mod util;

pub use sampler::{Sampler, SamplerConfig, Shared};
