//! CLI usage parsing and session-cumulative → per-turn delta conversion.
//!
//! CodeBuddy CLI's `result.usage.input_tokens` / `output_tokens` are
//! session-level cumulative counters (they grow monotonically across turns
//! inside one pooled CLI process). OpenAI's `prompt_tokens` means the
//! per-request input size. Reporting the cumulative value as `prompt_tokens`
//! makes codex believe the live context is hundreds of thousands of tokens
//! and trips ACP compaction.
//!
//! This module keeps a per-session baseline and reports only the saturating
//! delta between consecutive CLI readings.

use serde_json::Value;

use crate::openai_types::{OaiPromptTokensDetails, OaiUsage};

/// Snapshot of the CLI's cumulative usage counters for one pooled session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CliUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
}

impl CliUsage {
    /// Turn a session-cumulative reading into a per-request OpenAI usage.
    ///
    /// - First observation (`prev = None`): the cumulative value *is* the
    ///   first-turn cost, so report it as-is.
    /// - Subsequent observations: saturating subtract so a CLI reset cannot
    ///   produce underflows / huge wraparound values.
    pub fn turn_delta(&self, prev: Option<&CliUsage>) -> CliUsage {
        match prev {
            None => *self,
            Some(p) => CliUsage {
                input_tokens: self.input_tokens.saturating_sub(p.input_tokens),
                output_tokens: self.output_tokens.saturating_sub(p.output_tokens),
                cache_read_input_tokens: self
                    .cache_read_input_tokens
                    .saturating_sub(p.cache_read_input_tokens),
                cache_creation_input_tokens: self
                    .cache_creation_input_tokens
                    .saturating_sub(p.cache_creation_input_tokens),
            },
        }
    }

    /// Map a per-turn delta into the OpenAI usage shape that
    /// `codex_api_proxy::normalized_chat_usage` already understands.
    pub fn to_openai_usage(&self) -> OaiUsage {
        let cache_read = nonzero(self.cache_read_input_tokens);
        let cache_write = nonzero(self.cache_creation_input_tokens);
        OaiUsage {
            prompt_tokens: self.input_tokens,
            completion_tokens: self.output_tokens,
            total_tokens: self.input_tokens.saturating_add(self.output_tokens),
            prompt_tokens_details: cache_read.map(|cached_tokens| OaiPromptTokensDetails {
                cached_tokens,
            }),
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: cache_write,
        }
    }

    /// Approximate live context occupancy for this model call.
    /// Prefer input size; fall back to cache-read when input is missing.
    pub fn context_tokens(&self) -> u64 {
        if self.input_tokens > 0 {
            self.input_tokens
        } else {
            self.cache_read_input_tokens
        }
    }
}

fn nonzero(v: u64) -> Option<u64> {
    if v == 0 { None } else { Some(v) }
}

/// Pull cumulative counters from a CLI `usage` object
/// (`result.usage` / `assistant.message.usage` / stream event usage).
///
/// Returns `None` when the message carries no usage object at all.
pub fn extract_cli_usage(usage: Option<&Value>) -> Option<CliUsage> {
    let usage = usage?;
    Some(CliUsage {
        input_tokens: usage_u64(usage, "input_tokens"),
        output_tokens: usage_u64(usage, "output_tokens"),
        cache_read_input_tokens: usage_u64(usage, "cache_read_input_tokens"),
        cache_creation_input_tokens: usage_u64(usage, "cache_creation_input_tokens"),
    })
}

fn usage_u64(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Given the previous session baseline and the latest CLI cumulative reading,
/// produce the OpenAI per-turn usage and the new baseline to store.
///
/// When `current` is `None` (CLI emitted no usage this turn), keep the prior
/// baseline and report zero tokens — do not invent numbers.
pub fn resolve_turn_usage(
    prev: Option<CliUsage>,
    current: Option<CliUsage>,
) -> (OaiUsage, Option<CliUsage>) {
    match current {
        Some(curr) => {
            let delta = curr.turn_delta(prev.as_ref());
            (delta.to_openai_usage(), Some(curr))
        }
        None => (OaiUsage::zero(), prev),
    }
}

/// Build the OpenAI usage reported to codex/ACP.
///
/// - Billing/turn accounting prefers the **last model-call** usage when the CLI
///   exposes it on `assistant` / stream events (per-request shape).
/// - Falls back to the session-cumulative delta from `result.usage`.
/// - Always advances the cumulative baseline from `result.usage` when present
///   so subsequent deltas stay correct.
///
/// Returning huge cumulative deltas as `prompt_tokens` makes codex treat them
/// as live context occupancy and can trip compaction; prefer last-call.
pub fn resolve_reported_usage(
    prev: Option<CliUsage>,
    cumulative: Option<CliUsage>,
    last_model_call: Option<CliUsage>,
) -> (OaiUsage, Option<CliUsage>, CliUsage /*delta*/) {
    let (delta_usage, next_baseline) = resolve_turn_usage(prev, cumulative);
    let delta = match cumulative {
        Some(curr) => curr.turn_delta(prev.as_ref()),
        None => CliUsage::default(),
    };
    let reported = if let Some(call) = last_model_call {
        call.to_openai_usage()
    } else {
        delta_usage
    };
    (reported, next_baseline, delta)
}

/// Heuristic: a single turn's **live** context (the per-request
/// `assistant`/stream usage `input_tokens`) grew by this much vs the previous
/// turn's live reading — a runaway-growth signal, used to schedule a same-id
/// session recreate on the next request.
///
/// Why live-context *growth*, not the cumulative delta or absolute size: the
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_cli_usage_reads_all_fields() {
        let u = json!({
            "input_tokens": 120,
            "output_tokens": 30,
            "cache_read_input_tokens": 80,
            "cache_creation_input_tokens": 10,
        });
        assert_eq!(
            extract_cli_usage(Some(&u)),
            Some(CliUsage {
                input_tokens: 120,
                output_tokens: 30,
                cache_read_input_tokens: 80,
                cache_creation_input_tokens: 10,
            })
        );
    }

    #[test]
    fn extract_cli_usage_defaults_missing_fields_to_zero() {
        let u = json!({"input_tokens": 7});
        assert_eq!(
            extract_cli_usage(Some(&u)),
            Some(CliUsage {
                input_tokens: 7,
                output_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        );
    }

    #[test]
    fn extract_cli_usage_none_when_absent() {
        assert_eq!(extract_cli_usage(None), None);
    }

    #[test]
    fn extract_cli_usage_handles_non_numeric() {
        let u = json!({"input_tokens": "x", "output_tokens": 5});
        assert_eq!(
            extract_cli_usage(Some(&u)),
            Some(CliUsage {
                input_tokens: 0,
                output_tokens: 5,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        );
    }

    #[test]
    fn first_turn_delta_is_the_cumulative_value() {
        let curr = CliUsage {
            input_tokens: 203_849,
            output_tokens: 412,
            cache_read_input_tokens: 100,
            cache_creation_input_tokens: 20,
        };
        assert_eq!(curr.turn_delta(None), curr);
    }

    #[test]
    fn subsequent_turn_delta_is_saturating_sub() {
        let prev = CliUsage {
            input_tokens: 1_624_901,
            output_tokens: 5_904,
            cache_read_input_tokens: 800,
            cache_creation_input_tokens: 50,
        };
        let curr = CliUsage {
            input_tokens: 1_777_731,
            output_tokens: 6_086,
            cache_read_input_tokens: 1_100,
            cache_creation_input_tokens: 70,
        };
        assert_eq!(
            curr.turn_delta(Some(&prev)),
            CliUsage {
                input_tokens: 152_830,
                output_tokens: 182,
                cache_read_input_tokens: 300,
                cache_creation_input_tokens: 20,
            }
        );
    }

    #[test]
    fn zero_delta_when_cumulative_unchanged() {
        let prev = CliUsage {
            input_tokens: 2_731_779,
            output_tokens: 17_112,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let curr = prev;
        assert_eq!(
            curr.turn_delta(Some(&prev)),
            CliUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }
        );
    }

    #[test]
    fn saturating_sub_on_cli_reset() {
        let prev = CliUsage {
            input_tokens: 1_000,
            output_tokens: 50,
            cache_read_input_tokens: 10,
            cache_creation_input_tokens: 5,
        };
        // CLI process somehow restarts mid-pool: counters drop.
        let curr = CliUsage {
            input_tokens: 200,
            output_tokens: 10,
            cache_read_input_tokens: 2,
            cache_creation_input_tokens: 1,
        };
        assert_eq!(
            curr.turn_delta(Some(&prev)),
            CliUsage {
                input_tokens: 0,
                output_tokens: 0,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            }
        );
    }

    #[test]
    fn to_openai_usage_maps_cache_fields() {
        let delta = CliUsage {
            input_tokens: 150_000,
            output_tokens: 200,
            cache_read_input_tokens: 80_000,
            cache_creation_input_tokens: 1_000,
        };
        let oai = delta.to_openai_usage();
        assert_eq!(oai.prompt_tokens, 150_000);
        assert_eq!(oai.completion_tokens, 200);
        assert_eq!(oai.total_tokens, 150_200);
        assert_eq!(
            oai.prompt_tokens_details,
            Some(OaiPromptTokensDetails {
                cached_tokens: 80_000
            })
        );
        assert_eq!(oai.cache_read_input_tokens, Some(80_000));
        assert_eq!(oai.cache_creation_input_tokens, Some(1_000));
    }

    #[test]
    fn to_openai_usage_omits_zero_cache_fields() {
        let delta = CliUsage {
            input_tokens: 10,
            output_tokens: 2,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let oai = delta.to_openai_usage();
        assert!(oai.prompt_tokens_details.is_none());
        assert!(oai.cache_read_input_tokens.is_none());
        assert!(oai.cache_creation_input_tokens.is_none());
    }

    #[test]
    fn resolve_turn_usage_updates_baseline() {
        let prev = CliUsage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let curr = CliUsage {
            input_tokens: 250,
            output_tokens: 30,
            cache_read_input_tokens: 40,
            cache_creation_input_tokens: 5,
        };
        let (oai, next) = resolve_turn_usage(Some(prev), Some(curr));
        assert_eq!(oai.prompt_tokens, 150);
        assert_eq!(oai.completion_tokens, 20);
        assert_eq!(oai.total_tokens, 170);
        assert_eq!(oai.cache_read_input_tokens, Some(40));
        assert_eq!(oai.cache_creation_input_tokens, Some(5));
        assert_eq!(next, Some(curr));
    }

    #[test]
    fn resolve_turn_usage_keeps_baseline_when_current_missing() {
        let prev = CliUsage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let (oai, next) = resolve_turn_usage(Some(prev), None);
        assert_eq!(oai, OaiUsage::zero());
        assert_eq!(next, Some(prev));
    }

    #[test]
    fn reported_usage_prefers_last_model_call_over_cumulative_delta() {
        let prev = CliUsage {
            input_tokens: 300_000,
            output_tokens: 1_000,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let cumulative = CliUsage {
            input_tokens: 929_096,
            output_tokens: 2_839,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 629_096,
        };
        // Pathological cumulative delta (~629k) must not become prompt_tokens
        // when the last model call reported a sane per-request size.
        let last_call = CliUsage {
            input_tokens: 39_217,
            output_tokens: 519,
            cache_read_input_tokens: 37_888,
            cache_creation_input_tokens: 1_329,
        };
        let (reported, next, delta) =
            resolve_reported_usage(Some(prev), Some(cumulative), Some(last_call));
        assert_eq!(reported.prompt_tokens, 39_217);
        assert_eq!(reported.completion_tokens, 519);
        assert_eq!(delta.input_tokens, 629_096);
        assert_eq!(next, Some(cumulative));
    }

    #[test]
    fn reported_usage_falls_back_to_delta_without_last_call() {
        let prev = CliUsage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let cumulative = CliUsage {
            input_tokens: 250,
            output_tokens: 30,
            cache_read_input_tokens: 40,
            cache_creation_input_tokens: 5,
        };
        let (reported, next, delta) = resolve_reported_usage(Some(prev), Some(cumulative), None);
        assert_eq!(reported.prompt_tokens, 150);
        assert_eq!(reported.completion_tokens, 20);
        assert_eq!(delta.input_tokens, 150);
        assert_eq!(next, Some(cumulative));
    }
}
