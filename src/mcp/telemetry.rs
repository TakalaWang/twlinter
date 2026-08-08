// Per-request token telemetry for LLM cost accounting (51.1).
//
// Accumulates counters during scan + disambig + sampling, then derives
// three ratio metrics plus cache/tier2 counters for the response.
// All counters use u64 with saturating arithmetic.  Ratio metrics
// with zero denominators emit null.

use serde::Serialize;

/// Raw counters accumulated during a single tool invocation.
#[derive(Debug, Clone, Default)]
pub struct TokenTelemetry {
    /// Total input characters processed (after NFC normalization).
    pub input_chars: u64,
    /// Number of rule hits from the AC scanner (Tier 1).
    pub rule_hits: u64,
    /// Number of ambiguous terms entering Tier 2+.
    pub ambiguous_terms: u64,
    /// Number of LLM round-trips (Tier 3 sampling calls).
    pub llm_round_trips: u64,
    /// Total prompt tokens sent to LLM across all round-trips.
    pub prompt_tokens: u64,
    /// Total completion tokens received from LLM.
    pub completion_tokens: u64,
    /// Number of issues resolved by Tier 2 local disambiguation.
    pub tier2_resolved: u64,
    /// Judgment cache hits (51.4).
    pub cache_hits: u64,
    /// Judgment cache misses (51.4).
    pub cache_misses: u64,
    /// Final fix count (issues that resulted in changes).
    pub final_fixes: u64,
}

impl TokenTelemetry {
    /// Record an LLM round-trip with its token usage.
    pub fn record_llm_call(&mut self, prompt_tokens: u64, completion_tokens: u64) {
        self.llm_round_trips = self.llm_round_trips.saturating_add(1);
        self.prompt_tokens = self.prompt_tokens.saturating_add(prompt_tokens);
        self.completion_tokens = self.completion_tokens.saturating_add(completion_tokens);
    }

    /// Total LLM tokens (prompt + completion).
    pub fn total_llm_tokens(&self) -> u64 {
        self.prompt_tokens.saturating_add(self.completion_tokens)
    }

    /// Derive ratio metrics and snapshot counters.  Returns None for
    /// ratio metrics whose denominator is zero (spec: emit null, not 0 or NaN).
    pub fn derive_metrics(&self) -> TelemetryMetrics {
        let total_llm = self.total_llm_tokens();

        // 1. Average token cost per 1000 chars.
        let tokens_per_1k_chars = if self.input_chars > 0 {
            Some((total_llm as f64) * 1000.0 / (self.input_chars as f64))
        } else {
            None
        };

        // 2. LLM token share of total per-call cost.
        //    We define 'total cost' as rule_hits + total_llm_tokens.
        //    When both are zero, the metric is undefined.
        let total_cost = (self.rule_hits as f64) + (total_llm as f64);
        let llm_token_share = if total_cost > 0.0 {
            Some((total_llm as f64) / total_cost)
        } else {
            None
        };

        // 3. Average tokens spent per ambiguous term.
        let tokens_per_ambiguous_term = if self.ambiguous_terms > 0 {
            Some((total_llm as f64) / (self.ambiguous_terms as f64))
        } else {
            None
        };

        TelemetryMetrics {
            estimated_tokens_per_1k_chars: tokens_per_1k_chars,
            estimated_llm_token_share: llm_token_share,
            estimated_tokens_per_ambiguous_term: tokens_per_ambiguous_term,
            cache_hit_count: self.cache_hits,
            cache_miss_count: self.cache_misses,
            tier2_local_resolutions: self.tier2_resolved,
            raw: RawTelemetryCounters {
                input_chars: self.input_chars,
                rule_hits: self.rule_hits,
                ambiguous_terms: self.ambiguous_terms,
                llm_round_trips: self.llm_round_trips,
                estimated_prompt_tokens: self.prompt_tokens,
                estimated_completion_tokens: self.completion_tokens,
                final_fixes: self.final_fixes,
            },
        }
    }
}

/// Derived telemetry metrics for the response payload.
/// Metrics with zero denominators serialize as null (not omitted).
#[derive(Debug, Clone, Serialize)]
pub struct TelemetryMetrics {
    /// Average estimated LLM token cost per 1000 input characters.  null if input is empty.
    pub estimated_tokens_per_1k_chars: Option<f64>,
    /// Estimated LLM token share of total per-call cost [0.0, 1.0].  null if no work done.
    pub estimated_llm_token_share: Option<f64>,
    /// Average estimated LLM tokens spent per ambiguous term.  null if no ambiguous terms.
    pub estimated_tokens_per_ambiguous_term: Option<f64>,
    /// Judgment cache hit count (51.4 ROI).
    pub cache_hit_count: u64,
    /// Judgment cache miss count.
    pub cache_miss_count: u64,
    /// Number of issues resolved by Tier 2 local disambiguation.
    pub tier2_local_resolutions: u64,
    /// Raw counters for detailed analysis.
    pub raw: RawTelemetryCounters,
}

/// Raw counter snapshot included in telemetry output.
/// prompt/completion tokens are estimates (bytes/3 heuristic), not actual counts.
#[derive(Debug, Clone, Serialize)]
pub struct RawTelemetryCounters {
    pub input_chars: u64,
    pub rule_hits: u64,
    pub ambiguous_terms: u64,
    pub llm_round_trips: u64,
    pub estimated_prompt_tokens: u64,
    pub estimated_completion_tokens: u64,
    pub final_fixes: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_input_yields_null_metrics() {
        let t = TokenTelemetry::default();
        let m = t.derive_metrics();
        assert!(m.estimated_tokens_per_1k_chars.is_none());
        assert!(m.estimated_llm_token_share.is_none());
        assert!(m.estimated_tokens_per_ambiguous_term.is_none());
    }

    #[test]
    fn no_llm_yields_zero_share() {
        let t = TokenTelemetry {
            input_chars: 5000,
            rule_hits: 10,
            ..Default::default()
        };
        let m = t.derive_metrics();
        assert_eq!(m.estimated_tokens_per_1k_chars, Some(0.0));
        assert_eq!(m.estimated_llm_token_share, Some(0.0));
        assert!(m.estimated_tokens_per_ambiguous_term.is_none());
    }

    #[test]
    fn llm_metrics_computed_correctly() {
        let t = TokenTelemetry {
            input_chars: 2000,
            rule_hits: 8,
            ambiguous_terms: 2,
            llm_round_trips: 2,
            prompt_tokens: 100,
            completion_tokens: 20,
            ..Default::default()
        };
        let m = t.derive_metrics();
        // total_llm = 120, per 1k chars = 120 * 1000 / 2000 = 60.0
        assert_eq!(m.estimated_tokens_per_1k_chars, Some(60.0));
        // share = 120 / (8 + 120) = 120/128 = 0.9375
        assert!((m.estimated_llm_token_share.unwrap() - 0.9375).abs() < 1e-6);
        // per ambiguous = 120 / 2 = 60.0
        assert_eq!(m.estimated_tokens_per_ambiguous_term, Some(60.0));
    }

    #[test]
    fn saturating_accumulation() {
        let mut t = TokenTelemetry {
            prompt_tokens: u64::MAX,
            ..Default::default()
        };
        t.record_llm_call(1, 0);
        assert_eq!(t.prompt_tokens, u64::MAX);
        assert_eq!(t.llm_round_trips, 1);
    }

    #[test]
    fn cache_counters_in_metrics() {
        let t = TokenTelemetry {
            cache_hits: 5,
            cache_misses: 3,
            ..Default::default()
        };
        let m = t.derive_metrics();
        assert_eq!(m.cache_hit_count, 5);
        assert_eq!(m.cache_miss_count, 3);
    }
}
