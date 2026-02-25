use std::collections::BTreeMap;
use std::sync::RwLock;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

/// A lightweight, thread-safe metrics registry that renders in Prometheus text exposition format.
pub struct MetricsRegistry {
    counters: RwLock<BTreeMap<String, Counter>>,
    gauges: RwLock<BTreeMap<String, Gauge>>,
}

/// Monotonically increasing counter.
pub struct Counter {
    value: AtomicU64,
    help: String,
}

/// Value that can go up or down.
pub struct Gauge {
    value: AtomicI64,
    help: String,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            counters: RwLock::new(BTreeMap::new()),
            gauges: RwLock::new(BTreeMap::new()),
        }
    }

    /// Register a counter. If it already exists, this is a no-op.
    pub fn register_counter(&self, name: &str, help: &str) {
        let mut counters = self.counters.write().unwrap();
        counters.entry(name.to_string()).or_insert_with(|| Counter {
            value: AtomicU64::new(0),
            help: help.to_string(),
        });
    }

    /// Register a gauge. If it already exists, this is a no-op.
    pub fn register_gauge(&self, name: &str, help: &str) {
        let mut gauges = self.gauges.write().unwrap();
        gauges.entry(name.to_string()).or_insert_with(|| Gauge {
            value: AtomicI64::new(0),
            help: help.to_string(),
        });
    }

    /// Increment a counter by 1.
    pub fn counter_inc(&self, name: &str) {
        let counters = self.counters.read().unwrap();
        if let Some(c) = counters.get(name) {
            c.value.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Increment a counter by a given amount.
    pub fn counter_add(&self, name: &str, val: u64) {
        let counters = self.counters.read().unwrap();
        if let Some(c) = counters.get(name) {
            c.value.fetch_add(val, Ordering::Relaxed);
        }
    }

    /// Set a gauge to a specific value.
    pub fn gauge_set(&self, name: &str, val: i64) {
        let gauges = self.gauges.read().unwrap();
        if let Some(g) = gauges.get(name) {
            g.value.store(val, Ordering::Relaxed);
        }
    }

    /// Increment a gauge by 1.
    pub fn gauge_inc(&self, name: &str) {
        let gauges = self.gauges.read().unwrap();
        if let Some(g) = gauges.get(name) {
            g.value.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Decrement a gauge by 1.
    pub fn gauge_dec(&self, name: &str) {
        let gauges = self.gauges.read().unwrap();
        if let Some(g) = gauges.get(name) {
            g.value.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Render all metrics in Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut output = String::new();

        // Counters
        let counters = self.counters.read().unwrap();
        for (name, counter) in counters.iter() {
            output.push_str(&format!("# HELP {} {}\n", name, counter.help));
            output.push_str(&format!("# TYPE {} counter\n", name));
            output.push_str(&format!(
                "{} {}\n",
                name,
                counter.value.load(Ordering::Relaxed)
            ));
        }

        // Gauges
        let gauges = self.gauges.read().unwrap();
        for (name, gauge) in gauges.iter() {
            output.push_str(&format!("# HELP {} {}\n", name, gauge.help));
            output.push_str(&format!("# TYPE {} gauge\n", name));
            output.push_str(&format!(
                "{} {}\n",
                name,
                gauge.value.load(Ordering::Relaxed)
            ));
        }

        output
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}
