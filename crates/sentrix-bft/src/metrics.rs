//! BFT consensus metrics. Tendermint-style observability for round stall,
//! timeout, vote, and jail detection.
//!
//! # Design
//!
//! [`BftMetrics`] is registered against a [`prometheus::Registry`] at
//! construction. The validator binary creates one instance against the
//! default registry; tests can construct against a private registry to
//! avoid global state pollution.
//!
//! Metrics are wrapped behind cheap accessors so engine-side call sites stay
//! one-liners. All counters/gauges use `IntCounter` / `IntGauge` for
//! per-event throughput; the only `Histogram` is round duration, which needs
//! distributional summary.
//!
//! Phase labels (`propose`/`prevote`/`precommit`) and jail reason labels are
//! emitted as Prometheus labels so dashboards can break down by phase
//! without separate counter names per phase.
//!
//! # Performance
//!
//! Each metric op is one atomic add (counter/gauge) or one histogram
//! observation (lock-free in `prometheus 0.14`). The optional wrapping in
//! [`Option<Arc<BftMetrics>>`] on the engine side adds one `if let` —
//! negligible vs the BFT hot-path work that follows. Hot-path call sites
//! are noted in `engine.rs`.

use prometheus::{Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry};
use std::sync::Arc;

/// Phase label values used by the per-phase counters. Kept as `&'static str`
/// so the metric impls don't allocate per increment.
pub mod phase {
    pub const PROPOSE: &str = "propose";
    pub const PREVOTE: &str = "prevote";
    pub const PRECOMMIT: &str = "precommit";
}

/// Reason label values used by the jail counter.
pub mod jail_reason {
    pub const LIVENESS: &str = "liveness";
    pub const DOUBLE_SIGN: &str = "double_sign";
    pub const MANUAL: &str = "manual";
}

/// All BFT consensus metrics. Cheap to clone via [`Arc`]; engine holds an
/// [`Option<Arc<BftMetrics>>`] so test paths can pass `None` and skip the
/// metric calls entirely.
#[derive(Clone, Debug)]
pub struct BftMetrics {
    /// Distribution of round wall-clock durations (start of round to next-round
    /// advance or successful commit). Buckets cover sub-second to a full minute
    /// — anything past that is a real consensus stall, captured as +Inf.
    pub round_duration: Histogram,

    /// Per-phase timeout counter. Increments every time a phase times out
    /// without progress. Sustained non-zero rate on any phase = consensus
    /// liveness pressure.
    pub timeouts: IntCounterVec,

    /// Per-phase votes-received counter. Increments on every `on_proposal` /
    /// `on_prevote` / `on_precommit` accept. Useful for vote-loss detection
    /// (compare to expected `active_count - 1` per round).
    pub votes_received: IntCounterVec,

    /// Total round advances at the current height. Reset implicit via
    /// `new_height` — read via Prometheus delta over a window, not absolute.
    pub rounds_total: IntCounter,

    /// Jail events keyed by reason (`liveness` / `double_sign` / `manual`).
    /// Increment is operator-driven (BFT engine doesn't enforce slashing
    /// directly — slashing module signals; engine surfaces the count).
    pub jail_events: IntCounterVec,

    /// Current BFT round at the active height. Useful for "round stuck >0"
    /// alerts and per-validator round-skew diagnostics.
    pub current_round: IntGauge,

    /// Current BFT height. Mirrors chain.height + 1 when proposing.
    pub current_height: IntGauge,
}

impl BftMetrics {
    /// Construct and register all metrics against the given registry.
    ///
    /// Returns an [`Arc`] so the engine can hold a cheap clone and call
    /// sites stay one-liners.
    pub fn new(registry: &Registry) -> Result<Arc<Self>, prometheus::Error> {
        let round_duration = Histogram::with_opts(
            HistogramOpts::new(
                "bft_round_duration_seconds",
                "Wall-clock duration of each BFT round (propose-to-commit or propose-to-next-round).",
            )
            .buckets(vec![
                0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 20.0, 30.0, 60.0,
            ]),
        )?;
        registry.register(Box::new(round_duration.clone()))?;

        let timeouts = IntCounterVec::new(
            Opts::new(
                "bft_timeout_total",
                "BFT phase-timeout count, labeled by phase.",
            ),
            &["phase"],
        )?;
        registry.register(Box::new(timeouts.clone()))?;

        let votes_received = IntCounterVec::new(
            Opts::new(
                "bft_votes_received_total",
                "BFT votes received and accepted by the local engine, labeled by phase.",
            ),
            &["phase"],
        )?;
        registry.register(Box::new(votes_received.clone()))?;

        let rounds_total = IntCounter::with_opts(Opts::new(
            "bft_rounds_total",
            "Cumulative BFT round advances since process start (any height).",
        ))?;
        registry.register(Box::new(rounds_total.clone()))?;

        let jail_events = IntCounterVec::new(
            Opts::new(
                "bft_jail_events_total",
                "Jail events observed by the consensus path, labeled by reason.",
            ),
            &["reason"],
        )?;
        registry.register(Box::new(jail_events.clone()))?;

        let current_round = IntGauge::with_opts(Opts::new(
            "bft_current_round",
            "Current BFT round at the active consensus height.",
        ))?;
        registry.register(Box::new(current_round.clone()))?;

        let current_height = IntGauge::with_opts(Opts::new(
            "bft_current_height",
            "Current BFT height (= chain.height + 1 while proposing).",
        ))?;
        registry.register(Box::new(current_height.clone()))?;

        Ok(Arc::new(Self {
            round_duration,
            timeouts,
            votes_received,
            rounds_total,
            jail_events,
            current_round,
            current_height,
        }))
    }

    /// Convenience: instantiate against a fresh private registry. Useful in
    /// tests so each test gets isolated counters.
    pub fn for_test() -> Arc<Self> {
        let registry = Registry::new();
        Self::new(&registry).expect("test metrics registration cannot fail")
    }

    // ── Hot-path one-liners ───────────────────────────────────────────────

    /// Increment per-phase timeout counter.
    #[inline]
    pub fn inc_timeout(&self, phase_label: &str) {
        self.timeouts.with_label_values(&[phase_label]).inc();
    }

    /// Increment per-phase vote-received counter.
    #[inline]
    pub fn inc_vote(&self, phase_label: &str) {
        self.votes_received.with_label_values(&[phase_label]).inc();
    }

    /// Increment round-advance counter.
    #[inline]
    pub fn inc_round(&self) {
        self.rounds_total.inc();
    }

    /// Increment jail-event counter for a given reason.
    #[inline]
    pub fn inc_jail(&self, reason: &str) {
        self.jail_events.with_label_values(&[reason]).inc();
    }

    /// Observe a completed round's wall-clock duration.
    #[inline]
    pub fn observe_round_duration(&self, seconds: f64) {
        self.round_duration.observe(seconds);
    }

    /// Set current round gauge.
    #[inline]
    pub fn set_round(&self, round: u32) {
        self.current_round.set(round as i64);
    }

    /// Set current height gauge.
    #[inline]
    pub fn set_height(&self, height: u64) {
        self.current_height.set(height as i64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_register_against_fresh_registry() {
        let registry = Registry::new();
        let m = BftMetrics::new(&registry).unwrap();
        // Smoke-check: all increments work without panic.
        m.inc_timeout(phase::PROPOSE);
        m.inc_vote(phase::PREVOTE);
        m.inc_round();
        m.inc_jail(jail_reason::LIVENESS);
        m.observe_round_duration(0.5);
        m.set_round(7);
        m.set_height(12345);
    }

    #[test]
    fn test_double_register_errors_cleanly() {
        let registry = Registry::new();
        let _ok = BftMetrics::new(&registry).unwrap();
        // Must be a clean Err, not a panic. The exact error string varies
        // across prometheus crate versions; the contract we care about is
        // "second registration returns Err".
        assert!(BftMetrics::new(&registry).is_err());
    }

    #[test]
    fn test_for_test_helper_isolates() {
        let m1 = BftMetrics::for_test();
        let m2 = BftMetrics::for_test();
        m1.inc_round();
        assert_eq!(m1.rounds_total.get(), 1);
        // m2 is on its own registry, so m1's increment doesn't bleed.
        assert_eq!(m2.rounds_total.get(), 0);
    }
}
