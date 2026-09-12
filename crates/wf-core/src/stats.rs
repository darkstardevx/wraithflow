use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Live counters for one pipeline. All fields are atomics so every
/// connection task can update them concurrently without a lock.
#[derive(Default)]
pub struct PipelineStats {
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    pub connections_total: AtomicU64,
    pub connections_active: AtomicU64,
    pub errors_total: AtomicU64,
}

impl PipelineStats {
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            bytes_in: self.bytes_in.load(Ordering::Relaxed),
            bytes_out: self.bytes_out.load(Ordering::Relaxed),
            connections_total: self.connections_total.load(Ordering::Relaxed),
            connections_active: self.connections_active.load(Ordering::Relaxed),
            errors_total: self.errors_total.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct StatsSnapshot {
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub connections_total: u64,
    pub connections_active: u64,
    pub errors_total: u64,
}

/// Process-wide, cloneable registry of per-pipeline counters. Every
/// pipeline's connection tasks and the periodic stats logger all share the
/// same underlying map via `Arc`.
#[derive(Clone, Default)]
pub struct StatsRegistry {
    inner: Arc<Mutex<HashMap<String, Arc<PipelineStats>>>>,
}

impl StatsRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_or_create(&self, pipeline: &str) -> Arc<PipelineStats> {
        let mut map = self.inner.lock().unwrap();
        map.entry(pipeline.to_string())
            .or_insert_with(|| Arc::new(PipelineStats::default()))
            .clone()
    }

    pub fn snapshot_all(&self) -> Vec<(String, StatsSnapshot)> {
        let map = self.inner.lock().unwrap();
        let mut out: Vec<_> = map.iter().map(|(k, v)| (k.clone(), v.snapshot())).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}
