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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;

    #[test]
    fn get_or_create_returns_the_same_instance_for_a_repeated_name() {
        let registry = StatsRegistry::new();
        let a = registry.get_or_create("pipeline-a");
        a.bytes_in.fetch_add(42, Ordering::Relaxed);

        let b = registry.get_or_create("pipeline-a");
        assert_eq!(b.bytes_in.load(Ordering::Relaxed), 42);
    }

    #[test]
    fn snapshot_reflects_live_counters() {
        let stats = PipelineStats::default();
        stats.bytes_in.fetch_add(10, Ordering::Relaxed);
        stats.bytes_out.fetch_add(20, Ordering::Relaxed);
        stats.connections_total.fetch_add(1, Ordering::Relaxed);
        stats.connections_active.fetch_add(1, Ordering::Relaxed);
        stats.errors_total.fetch_add(2, Ordering::Relaxed);

        let snap = stats.snapshot();
        assert_eq!(snap.bytes_in, 10);
        assert_eq!(snap.bytes_out, 20);
        assert_eq!(snap.connections_total, 1);
        assert_eq!(snap.connections_active, 1);
        assert_eq!(snap.errors_total, 2);
    }

    #[test]
    fn snapshot_all_is_sorted_by_pipeline_name() {
        let registry = StatsRegistry::new();
        registry.get_or_create("zeta");
        registry.get_or_create("alpha");
        registry.get_or_create("mid");

        let names: Vec<_> = registry
            .snapshot_all()
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert_eq!(names, vec!["alpha", "mid", "zeta"]);
    }
}
