//! Shared primitives, buffers, and runtime state used by WraithFlow's
//! other crates (`wf-proxy`, `wf-packet`). Nothing here talks to a socket —
//! it's the vocabulary the rest of the workspace shares.

mod buffer;
mod packet;
mod stats;

pub use buffer::BufferPool;
pub use packet::{Direction, Packet};
pub use stats::{PipelineStats, StatsRegistry, StatsSnapshot};
