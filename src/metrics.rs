//! Lightweight Prometheus metrics for the operator.
//!
//! Uses atomic counters to avoid external crate dependencies. Metrics are
//! rendered as Prometheus text format on the `/metrics` endpoint.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::sync::OnceLock;
use std::time::Instant;

static METRICS: OnceLock<OperatorMetrics> = OnceLock::new();

/// Histogram bucket boundaries in milliseconds.
const DURATION_BUCKETS_MS: &[u64] = &[5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000, 10000];

/// Global operator metrics singleton.
pub struct OperatorMetrics {
    pub reconcile_total: AtomicU64,
    pub reconcile_success: AtomicU64,
    pub reconcile_errors: AtomicU64,
    pub reconcile_cluster_total: AtomicU64,
    pub reconcile_topic_total: AtomicU64,
    pub reconcile_user_total: AtomicU64,
    pub reconcile_cluster_errors: AtomicU64,
    pub reconcile_topic_errors: AtomicU64,
    pub reconcile_user_errors: AtomicU64,
    pub leader_transitions: AtomicU64,
    duration_buckets: Mutex<DurationHistogram>,
}

impl OperatorMetrics {
    fn new() -> Self {
        Self {
            reconcile_total: AtomicU64::new(0),
            reconcile_success: AtomicU64::new(0),
            reconcile_errors: AtomicU64::new(0),
            reconcile_cluster_total: AtomicU64::new(0),
            reconcile_topic_total: AtomicU64::new(0),
            reconcile_user_total: AtomicU64::new(0),
            reconcile_cluster_errors: AtomicU64::new(0),
            reconcile_topic_errors: AtomicU64::new(0),
            reconcile_user_errors: AtomicU64::new(0),
            leader_transitions: AtomicU64::new(0),
            duration_buckets: Mutex::new(DurationHistogram::new()),
        }
    }

    pub fn inc_reconcile(&self, resource: &str) {
        self.reconcile_total.fetch_add(1, Ordering::Relaxed);
        match resource {
            "cluster" => { self.reconcile_cluster_total.fetch_add(1, Ordering::Relaxed); }
            "topic" => { self.reconcile_topic_total.fetch_add(1, Ordering::Relaxed); }
            "user" => { self.reconcile_user_total.fetch_add(1, Ordering::Relaxed); }
            _ => {}
        }
    }

    pub fn inc_success(&self) {
        self.reconcile_success.fetch_add(1, Ordering::Relaxed);
    }

    pub fn inc_error(&self, resource: &str) {
        self.reconcile_errors.fetch_add(1, Ordering::Relaxed);
        match resource {
            "cluster" => { self.reconcile_cluster_errors.fetch_add(1, Ordering::Relaxed); }
            "topic" => { self.reconcile_topic_errors.fetch_add(1, Ordering::Relaxed); }
            "user" => { self.reconcile_user_errors.fetch_add(1, Ordering::Relaxed); }
            _ => {}
        }
    }

    pub fn inc_leader_transition(&self) {
        self.leader_transitions.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a reconciliation duration in milliseconds.
    pub fn observe_duration_ms(&self, ms: u64) {
        if let Ok(mut h) = self.duration_buckets.lock() {
            h.observe(ms);
        }
    }

    /// Start a timer that records duration on drop.
    pub fn start_timer(&self) -> ReconcileTimer<'_> {
        ReconcileTimer { metrics: self, start: Instant::now() }
    }

    /// Render metrics in Prometheus text exposition format.
    pub fn render(&self) -> String {
        let mut out = String::with_capacity(2048);

        out.push_str("# HELP streamline_operator_reconcile_total Total reconciliation attempts\n");
        out.push_str("# TYPE streamline_operator_reconcile_total counter\n");
        push_counter(&mut out, "streamline_operator_reconcile_total", &[], self.reconcile_total.load(Ordering::Relaxed));

        out.push_str("# HELP streamline_operator_reconcile_success_total Successful reconciliations\n");
        out.push_str("# TYPE streamline_operator_reconcile_success_total counter\n");
        push_counter(&mut out, "streamline_operator_reconcile_success_total", &[], self.reconcile_success.load(Ordering::Relaxed));

        out.push_str("# HELP streamline_operator_reconcile_errors_total Failed reconciliations\n");
        out.push_str("# TYPE streamline_operator_reconcile_errors_total counter\n");
        push_counter(&mut out, "streamline_operator_reconcile_errors_total", &[], self.reconcile_errors.load(Ordering::Relaxed));

        out.push_str("# HELP streamline_operator_reconcile_by_resource_total Reconciliations per resource type\n");
        out.push_str("# TYPE streamline_operator_reconcile_by_resource_total counter\n");
        push_counter(&mut out, "streamline_operator_reconcile_by_resource_total", &[("resource", "cluster")], self.reconcile_cluster_total.load(Ordering::Relaxed));
        push_counter(&mut out, "streamline_operator_reconcile_by_resource_total", &[("resource", "topic")], self.reconcile_topic_total.load(Ordering::Relaxed));
        push_counter(&mut out, "streamline_operator_reconcile_by_resource_total", &[("resource", "user")], self.reconcile_user_total.load(Ordering::Relaxed));

        out.push_str("# HELP streamline_operator_reconcile_errors_by_resource_total Errors per resource type\n");
        out.push_str("# TYPE streamline_operator_reconcile_errors_by_resource_total counter\n");
        push_counter(&mut out, "streamline_operator_reconcile_errors_by_resource_total", &[("resource", "cluster")], self.reconcile_cluster_errors.load(Ordering::Relaxed));
        push_counter(&mut out, "streamline_operator_reconcile_errors_by_resource_total", &[("resource", "topic")], self.reconcile_topic_errors.load(Ordering::Relaxed));
        push_counter(&mut out, "streamline_operator_reconcile_errors_by_resource_total", &[("resource", "user")], self.reconcile_user_errors.load(Ordering::Relaxed));

        out.push_str("# HELP streamline_operator_leader_transitions_total Leader election transitions\n");
        out.push_str("# TYPE streamline_operator_leader_transitions_total counter\n");
        push_counter(&mut out, "streamline_operator_leader_transitions_total", &[], self.leader_transitions.load(Ordering::Relaxed));

        // Duration histogram
        if let Ok(h) = self.duration_buckets.lock() {
            out.push_str("# HELP streamline_operator_reconcile_duration_ms Reconciliation duration in milliseconds\n");
            out.push_str("# TYPE streamline_operator_reconcile_duration_ms histogram\n");
            let mut cumulative = 0u64;
            for (i, &boundary) in DURATION_BUCKETS_MS.iter().enumerate() {
                cumulative += h.buckets[i];
                out.push_str(&format!(
                    "streamline_operator_reconcile_duration_ms_bucket{{le=\"{}\"}} {}\n",
                    boundary, cumulative
                ));
            }
            cumulative += h.buckets[DURATION_BUCKETS_MS.len()];
            out.push_str(&format!(
                "streamline_operator_reconcile_duration_ms_bucket{{le=\"+Inf\"}} {}\n",
                cumulative
            ));
            out.push_str(&format!(
                "streamline_operator_reconcile_duration_ms_sum {}\n",
                h.sum
            ));
            out.push_str(&format!(
                "streamline_operator_reconcile_duration_ms_count {}\n",
                h.count
            ));
        }

        out
    }
}

/// RAII timer that records reconcile duration on drop.
pub struct ReconcileTimer<'a> {
    metrics: &'a OperatorMetrics,
    start: Instant,
}

impl<'a> Drop for ReconcileTimer<'a> {
    fn drop(&mut self) {
        let elapsed_ms = self.start.elapsed().as_millis() as u64;
        self.metrics.observe_duration_ms(elapsed_ms);
    }
}

/// Simple fixed-bucket histogram.
struct DurationHistogram {
    /// buckets[i] = count of observations in (BUCKETS[i-1], BUCKETS[i]].
    /// buckets[len] = count of observations > last bucket boundary.
    buckets: Vec<u64>,
    sum: u64,
    count: u64,
}

impl DurationHistogram {
    fn new() -> Self {
        Self {
            buckets: vec![0; DURATION_BUCKETS_MS.len() + 1],
            sum: 0,
            count: 0,
        }
    }

    fn observe(&mut self, value_ms: u64) {
        self.sum += value_ms;
        self.count += 1;
        for (i, &boundary) in DURATION_BUCKETS_MS.iter().enumerate() {
            if value_ms <= boundary {
                self.buckets[i] += 1;
                return;
            }
        }
        // Overflow bucket (+Inf)
        *self.buckets.last_mut().expect("buckets non-empty") += 1;
    }
}

fn push_counter(out: &mut String, name: &str, labels: &[(&str, &str)], value: u64) {
    out.push_str(name);
    if !labels.is_empty() {
        out.push('{');
        for (i, (k, v)) in labels.iter().enumerate() {
            if i > 0 { out.push(','); }
            out.push_str(k);
            out.push_str("=\"");
            out.push_str(v);
            out.push('"');
        }
        out.push('}');
    }
    out.push(' ');
    out.push_str(&value.to_string());
    out.push('\n');
}

/// Get or initialize the global metrics instance.
pub fn get() -> &'static OperatorMetrics {
    METRICS.get_or_init(OperatorMetrics::new)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inc_reconcile() {
        let m = OperatorMetrics::new();
        m.inc_reconcile("cluster");
        m.inc_reconcile("topic");
        m.inc_reconcile("topic");
        assert_eq!(m.reconcile_total.load(Ordering::Relaxed), 3);
        assert_eq!(m.reconcile_cluster_total.load(Ordering::Relaxed), 1);
        assert_eq!(m.reconcile_topic_total.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_inc_success_and_error() {
        let m = OperatorMetrics::new();
        m.inc_success();
        m.inc_success();
        m.inc_error("cluster");
        assert_eq!(m.reconcile_success.load(Ordering::Relaxed), 2);
        assert_eq!(m.reconcile_errors.load(Ordering::Relaxed), 1);
        assert_eq!(m.reconcile_cluster_errors.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn test_render_prometheus_format() {
        let m = OperatorMetrics::new();
        m.inc_reconcile("cluster");
        m.inc_success();
        let output = m.render();
        assert!(output.contains("streamline_operator_reconcile_total 1"));
        assert!(output.contains("streamline_operator_reconcile_success_total 1"));
        assert!(output.contains("# TYPE streamline_operator_reconcile_total counter"));
        assert!(output.contains(r#"resource="cluster""#));
    }

    #[test]
    fn test_leader_transitions() {
        let m = OperatorMetrics::new();
        m.inc_leader_transition();
        m.inc_leader_transition();
        assert_eq!(m.leader_transitions.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn test_global_singleton() {
        let m1 = get();
        let m2 = get();
        assert!(std::ptr::eq(m1, m2));
    }

    #[test]
    fn test_duration_histogram() {
        let m = OperatorMetrics::new();
        m.observe_duration_ms(3);    // bucket: ≤5
        m.observe_duration_ms(50);   // bucket: ≤50
        m.observe_duration_ms(500);  // bucket: ≤500
        m.observe_duration_ms(20000); // bucket: +Inf
        let output = m.render();
        assert!(output.contains("reconcile_duration_ms_bucket"));
        assert!(output.contains("reconcile_duration_ms_sum 20553"));
        assert!(output.contains("reconcile_duration_ms_count 4"));
        assert!(output.contains(r#"le="+Inf"} 4"#));
    }

    #[test]
    fn test_duration_histogram_buckets_cumulative() {
        let h = &mut DurationHistogram::new();
        h.observe(1);   // ≤5
        h.observe(7);   // ≤10
        h.observe(100); // ≤100
        assert_eq!(h.count, 3);
        assert_eq!(h.sum, 108);
        assert_eq!(h.buckets[0], 1); // ≤5
        assert_eq!(h.buckets[1], 1); // ≤10
    }

    #[test]
    fn test_start_timer_records_duration() {
        let m = OperatorMetrics::new();
        {
            let _t = m.start_timer();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let h = m.duration_buckets.lock().unwrap();
        assert_eq!(h.count, 1);
        assert!(h.sum >= 5);
    }
}
