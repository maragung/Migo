//! A small Prometheus-compatible metrics registry.
//!
//! Hand-rolled for two reasons. First, dependency weight: the exposition format
//! is a few hundred lines and we need exactly three metric types. Second, and
//! more important, it lets us **enforce** the bounded-cardinality rule instead
//! of merely writing it down: a family that exceeds
//! [`MAX_SERIES_PER_FAMILY`] folds further label combinations into a single
//! `overflow` series rather than growing without limit. A metrics registry that
//! can be made to allocate by a remote peer is a memory leak with a dashboard.
//!
//! Usage is register-once, then hit the handle:
//!
//! ```
//! use migo_core::metrics::{Registry, LATENCY_BUCKETS_MS};
//! let registry = Registry::new();
//! let frames = registry.counter("migo_frames_total", "Frames decoded.", &[("op", "MESSAGE_SEND")]);
//! frames.inc();
//! let latency = registry.histogram("migo_handler_ms", "Handler latency.", &[], LATENCY_BUCKETS_MS);
//! latency.observe(3.5);
//! assert!(registry.render().contains("migo_frames_total"));
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::RwLock;

/// Maximum distinct label combinations per metric family.
pub const MAX_SERIES_PER_FAMILY: usize = 512;

/// Latency buckets in milliseconds, tuned for the values we actually alert on:
/// sub-millisecond codec work through multi-second federation calls.
pub const LATENCY_BUCKETS_MS: &[f64] = &[
    0.5, 1.0, 2.5, 5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 5000.0,
];

/// Size buckets in bytes, aligned with the frame budget in
/// `docs/05-bandwidth-budget.md`.
pub const SIZE_BUCKETS_BYTES: &[f64] = &[
    16.0, 32.0, 64.0, 128.0, 256.0, 512.0, 1024.0, 4096.0, 16384.0, 65536.0, 262_144.0,
];

/// A monotonically increasing count.
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    /// Adds one.
    pub fn inc(&self) {
        self.add(1);
    }

    /// Adds `n`.
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    /// Current value.
    #[must_use]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// A value that goes up and down.
#[derive(Debug, Default)]
pub struct Gauge(AtomicI64);

impl Gauge {
    /// Adds one.
    pub fn inc(&self) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }

    /// Subtracts one.
    pub fn dec(&self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }

    /// Overwrites the value.
    pub fn set(&self, value: i64) {
        self.0.store(value, Ordering::Relaxed);
    }

    /// Current value.
    #[must_use]
    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// A cumulative histogram with fixed bucket bounds.
#[derive(Debug)]
pub struct Histogram {
    bounds: Vec<f64>,
    /// One counter per bucket plus a final `+Inf` bucket.
    counts: Vec<AtomicU64>,
    /// Sum is stored as scaled integer milli-units so the metric needs no lock
    /// and stays exactly reproducible across platforms.
    sum_milli: AtomicU64,
}

impl Histogram {
    /// Builds a histogram over the given upper bounds, which must be ascending.
    #[must_use]
    pub fn new(bounds: &[f64]) -> Self {
        let mut sorted: Vec<f64> = bounds.to_vec();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let counts = (0..=sorted.len()).map(|_| AtomicU64::new(0)).collect();
        Self {
            bounds: sorted,
            counts,
            sum_milli: AtomicU64::new(0),
        }
    }

    /// Records one observation.
    pub fn observe(&self, value: f64) {
        let index = match self.bounds.iter().position(|bound| value <= *bound) {
            Some(i) => i,
            None => self.bounds.len(),
        };
        self.counts[index].fetch_add(1, Ordering::Relaxed);
        let scaled = if value.is_finite() && value > 0.0 {
            (value * 1000.0) as u64
        } else {
            0
        };
        self.sum_milli.fetch_add(scaled, Ordering::Relaxed);
    }

    /// Total number of observations.
    #[must_use]
    pub fn count(&self) -> u64 {
        self.counts.iter().map(|c| c.load(Ordering::Relaxed)).sum()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Counter,
    Gauge,
    Histogram,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Counter => "counter",
            Kind::Gauge => "gauge",
            Kind::Histogram => "histogram",
        }
    }
}

enum Series {
    Counter(Arc<Counter>),
    Gauge(Arc<Gauge>),
    Histogram(Arc<Histogram>),
}

struct Family {
    help: &'static str,
    kind: Kind,
    series: BTreeMap<String, Series>,
}

/// Holds every metric in the process.
#[derive(Default)]
pub struct Registry {
    families: RwLock<BTreeMap<&'static str, Family>>,
}

impl Registry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers or looks up a counter.
    pub fn counter(
        &self,
        name: &'static str,
        help: &'static str,
        labels: &[(&str, &str)],
    ) -> Arc<Counter> {
        self.series(name, help, Kind::Counter, labels, |series| match series {
            Series::Counter(counter) => Some(Arc::clone(counter)),
            _ => None,
        })
        .unwrap_or_else(|| Arc::new(Counter::default()))
    }

    /// Registers or looks up a gauge.
    pub fn gauge(
        &self,
        name: &'static str,
        help: &'static str,
        labels: &[(&str, &str)],
    ) -> Arc<Gauge> {
        self.series(name, help, Kind::Gauge, labels, |series| match series {
            Series::Gauge(gauge) => Some(Arc::clone(gauge)),
            _ => None,
        })
        .unwrap_or_else(|| Arc::new(Gauge::default()))
    }

    /// Registers or looks up a histogram. `bounds` is honoured only on first
    /// registration of a given name and label set.
    pub fn histogram(
        &self,
        name: &'static str,
        help: &'static str,
        labels: &[(&str, &str)],
        bounds: &[f64],
    ) -> Arc<Histogram> {
        let key = encode_labels(labels);
        {
            let families = self.families.read();
            if let Some(existing) = families
                .get(name)
                .and_then(|family| family.series.get(&key))
                .and_then(|series| match series {
                    Series::Histogram(histogram) => Some(Arc::clone(histogram)),
                    _ => None,
                })
            {
                return existing;
            }
        }
        let mut families = self.families.write();
        let family = families.entry(name).or_insert_with(|| Family {
            help,
            kind: Kind::Histogram,
            series: BTreeMap::new(),
        });
        let key = Self::bounded_key(family, key);
        match family
            .series
            .entry(key)
            .or_insert_with(|| Series::Histogram(Arc::new(Histogram::new(bounds))))
        {
            Series::Histogram(histogram) => Arc::clone(histogram),
            // Name reused with a different type: a programming error we do not
            // want to panic on in production, so hand back a detached handle.
            _ => Arc::new(Histogram::new(bounds)),
        }
    }

    fn series<T>(
        &self,
        name: &'static str,
        help: &'static str,
        kind: Kind,
        labels: &[(&str, &str)],
        pick: impl Fn(&Series) -> Option<Arc<T>>,
    ) -> Option<Arc<T>> {
        let key = encode_labels(labels);
        {
            let families = self.families.read();
            if let Some(found) = families
                .get(name)
                .and_then(|family| family.series.get(&key))
                .and_then(&pick)
            {
                return Some(found);
            }
        }
        let mut families = self.families.write();
        let family = families.entry(name).or_insert_with(|| Family {
            help,
            kind,
            series: BTreeMap::new(),
        });
        if family.kind != kind {
            return None;
        }
        let key = Self::bounded_key(family, key);
        let entry = family.series.entry(key).or_insert_with(|| match kind {
            Kind::Counter => Series::Counter(Arc::new(Counter::default())),
            Kind::Gauge => Series::Gauge(Arc::new(Gauge::default())),
            Kind::Histogram => Series::Histogram(Arc::new(Histogram::new(LATENCY_BUCKETS_MS))),
        });
        pick(entry)
    }

    /// Folds a new label combination into `overflow` once the family is full.
    fn bounded_key(family: &Family, key: String) -> String {
        if family.series.contains_key(&key) || family.series.len() < MAX_SERIES_PER_FAMILY {
            key
        } else {
            "overflow=\"true\"".to_string()
        }
    }

    /// Renders the Prometheus text exposition format.
    #[must_use]
    pub fn render(&self) -> String {
        let families = self.families.read();
        let mut out = String::with_capacity(4096);
        for (name, family) in families.iter() {
            let _ = writeln!(out, "# HELP {name} {}", family.help);
            let _ = writeln!(out, "# TYPE {name} {}", family.kind.as_str());
            for (labels, series) in &family.series {
                match series {
                    Series::Counter(counter) => {
                        let _ = writeln!(out, "{}{} {}", name, braces(labels), counter.get());
                    }
                    Series::Gauge(gauge) => {
                        let _ = writeln!(out, "{}{} {}", name, braces(labels), gauge.get());
                    }
                    Series::Histogram(histogram) => {
                        let mut cumulative = 0u64;
                        for (index, bound) in histogram.bounds.iter().enumerate() {
                            cumulative += histogram.counts[index].load(Ordering::Relaxed);
                            let _ = writeln!(
                                out,
                                "{}_bucket{} {}",
                                name,
                                braces_with(labels, &format!("le=\"{bound}\"")),
                                cumulative
                            );
                        }
                        cumulative +=
                            histogram.counts[histogram.bounds.len()].load(Ordering::Relaxed);
                        let _ = writeln!(
                            out,
                            "{}_bucket{} {}",
                            name,
                            braces_with(labels, "le=\"+Inf\""),
                            cumulative
                        );
                        let sum = histogram.sum_milli.load(Ordering::Relaxed) as f64 / 1000.0;
                        let _ = writeln!(out, "{}_sum{} {}", name, braces(labels), sum);
                        let _ = writeln!(out, "{}_count{} {}", name, braces(labels), cumulative);
                    }
                }
            }
        }
        out
    }
}

fn encode_labels(labels: &[(&str, &str)]) -> String {
    if labels.is_empty() {
        return String::new();
    }
    let mut pairs: Vec<String> = labels
        .iter()
        .map(|(key, value)| format!("{key}=\"{}\"", escape(value)))
        .collect();
    // Sorted so that the same labels in a different order are one series.
    pairs.sort();
    pairs.join(",")
}

fn escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

fn braces(labels: &str) -> String {
    if labels.is_empty() {
        String::new()
    } else {
        format!("{{{labels}}}")
    }
}

fn braces_with(labels: &str, extra: &str) -> String {
    if labels.is_empty() {
        format!("{{{extra}}}")
    } else {
        format!("{{{labels},{extra}}}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_handles_are_shared() {
        let registry = Registry::new();
        let a = registry.counter("migo_test_total", "Test.", &[("op", "PING")]);
        let b = registry.counter("migo_test_total", "Test.", &[("op", "PING")]);
        a.inc();
        b.add(4);
        assert_eq!(a.get(), 5);
    }

    #[test]
    fn label_order_does_not_create_a_second_series() {
        let registry = Registry::new();
        let a = registry.counter("migo_x_total", "X.", &[("a", "1"), ("b", "2")]);
        let b = registry.counter("migo_x_total", "X.", &[("b", "2"), ("a", "1")]);
        a.inc();
        assert_eq!(b.get(), 1);
    }

    #[test]
    fn cardinality_is_capped() {
        let registry = Registry::new();
        for i in 0..(MAX_SERIES_PER_FAMILY + 50) {
            registry
                .counter(
                    "migo_hostile_total",
                    "Attacker-controlled label.",
                    &[("id", &i.to_string())],
                )
                .inc();
        }
        let rendered = registry.render();
        let series = rendered
            .lines()
            .filter(|line| line.starts_with("migo_hostile_total"))
            .count();
        assert_eq!(
            series,
            MAX_SERIES_PER_FAMILY + 1,
            "cap plus the overflow series"
        );
        assert!(rendered.contains("overflow=\"true\""));
    }

    #[test]
    fn histogram_buckets_are_cumulative() {
        let registry = Registry::new();
        let histogram = registry.histogram("migo_ms", "Latency.", &[], &[1.0, 10.0, 100.0]);
        histogram.observe(0.5);
        histogram.observe(5.0);
        histogram.observe(50.0);
        histogram.observe(5000.0);
        let rendered = registry.render();
        assert!(
            rendered.contains("migo_ms_bucket{le=\"1\"} 1"),
            "{rendered}"
        );
        assert!(
            rendered.contains("migo_ms_bucket{le=\"10\"} 2"),
            "{rendered}"
        );
        assert!(
            rendered.contains("migo_ms_bucket{le=\"100\"} 3"),
            "{rendered}"
        );
        assert!(
            rendered.contains("migo_ms_bucket{le=\"+Inf\"} 4"),
            "{rendered}"
        );
        assert!(rendered.contains("migo_ms_count 4"), "{rendered}");
        assert_eq!(histogram.count(), 4);
    }

    #[test]
    fn label_values_are_escaped() {
        let registry = Registry::new();
        registry
            .counter("migo_esc_total", "Escaping.", &[("k", "a\"b\\c")])
            .inc();
        let rendered = registry.render();
        assert!(rendered.contains(r#"k="a\"b\\c""#), "{rendered}");
    }

    #[test]
    fn exposition_has_help_and_type_headers() {
        let registry = Registry::new();
        registry
            .gauge("migo_sessions", "Open sessions.", &[])
            .set(3);
        let rendered = registry.render();
        assert!(rendered.contains("# HELP migo_sessions Open sessions."));
        assert!(rendered.contains("# TYPE migo_sessions gauge"));
        assert!(rendered.contains("migo_sessions 3"));
    }
}
