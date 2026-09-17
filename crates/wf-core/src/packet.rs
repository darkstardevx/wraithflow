use crate::BufferPool;
use chrono::{DateTime, Utc};

/// Which way a chunk of payload was moving through a pipeline.
///
/// `Outbound` = client -> target (the direction data flows away from
/// WraithFlow toward the thing it's proxying to). `Inbound` = target ->
/// client (the response coming back). This matches the labels the original
/// hexdump logger used, kept for continuity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Inbound,
    Outbound,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Direction::Inbound => "INBOUND",
            Direction::Outbound => "OUTBOUND",
        }
    }
}

/// One captured chunk of payload crossing a pipeline.
///
/// This is *not* a network-layer packet — WraithFlow proxies TCP byte
/// streams, not raw IP frames, so there's no header to parse here. A
/// `Packet` is just one `read()`'s worth of bytes, timestamped and tagged
/// with which pipeline and direction it belongs to, so `wf-packet` has
/// something uniform to filter and render.
#[derive(Debug, Clone)]
pub struct Packet {
    pub pipeline: String,
    pub direction: Direction,
    pub timestamp: DateTime<Utc>,
    pub bytes: Vec<u8>,
}

impl Packet {
    pub fn new(pipeline: impl Into<String>, direction: Direction, bytes: &[u8]) -> Self {
        Self {
            pipeline: pipeline.into(),
            direction,
            timestamp: Utc::now(),
            bytes: bytes.to_vec(),
        }
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Same as `new`, but the payload copy comes from a `BufferPool`
    /// instead of a fresh allocation — the path a busy pipeline with
    /// payload logging on should use.
    pub fn pooled(
        pool: &BufferPool,
        pipeline: impl Into<String>,
        direction: Direction,
        bytes: &[u8],
    ) -> Self {
        let mut buf = pool.acquire();
        buf.extend_from_slice(bytes);
        Self {
            pipeline: pipeline.into(),
            direction,
            timestamp: Utc::now(),
            bytes: buf,
        }
    }

    /// Return this packet's buffer to the pool once you're done with it.
    pub fn recycle(self, pool: &BufferPool) {
        pool.release(self.bytes);
    }
}
