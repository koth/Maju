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
    ///
    /// **OpenAI convention: `prompt_tokens` is the full prompt size and
    /// already subsumes the cached prefix.** The CodeBuddy CLI reports
    /// `input_tokens` the same way (for glm-5.2-ioa, `input_tokens ==
    /// cache_read_input_tokens + cache_creation_input_tokens` exactly), so
    /// the delta passes through unchanged. An earlier revision added
    /// `cache_read` on top of `prompt_tokens` to mirror the CodeBuddy
    /// backend's *billing* convention (cache reads billed additively), but
    /// the downstream `normalized_chat_usage` treats `prompt_tokens` as
    /// cache-inclusive and subtracts `cached_tokens` again — the adjustment
    /// cancelled itself out for `input_tokens` while inflating
    /// `total_tokens`, double-counting the cached prefix there.
    ///
    /// `cache_read_input_tokens` / `prompt_tokens_details.cached_tokens`
    /// stay as the honest cache-hit count (display-only breakdown);
    /// `total_tokens = input + output`.
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
    // Prefer Claude/Anthropic-format fields (`input_tokens`/
    // `output_tokens`) and fall back to OpenAI-format names
    // (`prompt_tokens`/`completion_tokens`) when absent. Some model
    // backends surfaced through the CodeBuddy CLI (e.g. glm-5.2-ioa)
    // report usage in OpenAI shape; without the fallback, extraction
    // returns all-zeros and the turn is recorded as 0 tokens.
    let extracted = CliUsage {
        input_tokens: usage_u64_preferred(usage, "input_tokens", "prompt_tokens"),
        output_tokens: usage_u64_preferred(usage, "output_tokens", "completion_tokens"),
        cache_read_input_tokens: usage_u64_fallbacks(usage, &[
            "cache_read_input_tokens",
            "cached_input_tokens",
            "cache_read_tokens",
            "cached_tokens",
        ]),
        cache_creation_input_tokens: usage_u64_fallbacks(usage, &[
            "cache_creation_input_tokens",
            "cache_write_tokens",
        ]),
    };
    // A usage object whose recognized fields all parse to 0 carries no
    // information (e.g. an early `message_delta` frame before any tokens
    // were generated, or a shape we don't map). Treat it as absent so it
    // cannot become a `Some(zero)` that masks the cumulative-delta fallback
    // in `resolve_reported_usage` — that was the root cause of `finish=stop`
    // turns reporting 0 tokens while the CLI's cumulative had advanced.
    if extracted.input_tokens == 0
        && extracted.output_tokens == 0
        && extracted.cache_read_input_tokens == 0
        && extracted.cache_creation_input_tokens == 0
    {
        return None;
    }
    Some(extracted)
}

fn usage_u64(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Read a u64 preferring `primary`; fall back to `secondary` only when
/// `primary` is absent (not present in the object at all). A present-but-zero
/// `primary` is still authoritative so a real Claude-format usage object with
/// `input_tokens: 0` is not masked by an OpenAI `prompt_tokens` field.
fn usage_u64_preferred(usage: &Value, primary: &str, secondary: &str) -> u64 {
    if usage.get(primary).is_some() {
        return usage_u64(usage, primary);
    }
    usage_u64(usage, secondary)
}

/// Try each candidate field in order, returning the first present value
/// (present-but-zero is authoritative). Falls back to 0 when none exist.
fn usage_u64_fallbacks(usage: &Value, candidates: &[&str]) -> u64 {
    for key in candidates {
        if usage.get(*key).is_some() {
            return usage_u64(usage, key);
        }
    }
    0
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
/// - Billing/turn accounting uses the **session-cumulative delta** from
///   `result.usage` (the net per-turn consumption: cumulative counters
///   advanced since the previous turn). codex sums each turn's
///   `prompt_tokens` into its `total_token_usage`, so reporting the
///   per-request `last_model_call` (whose `input_tokens` is the **full
///   context size** of the last model call) would inflate the session total
///   by the entire context every turn — e.g. 5 turns × 50k context reported
///   as 250k instead of the ~55k real cumulative.
/// - Falls back to `last_model_call` only when the CLI emitted no cumulative
///   `result.usage` this turn but did surface a per-call usage (so we still
///   report something rather than zero).
/// - Always advances the cumulative baseline from `result.usage` when present
///   so subsequent deltas stay correct.
///
/// The earlier "report the full context to stop codex from auto-compacting"
/// concern is moot: the CodeBuddy provider advertises a ~1B context window
/// (see `app_core::settings::model_context_window_for_provider`), so codex
/// never compacts CodeBuddy sessions regardless of the reported `used` value.
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
    // Report the real per-turn net consumption. codex sums this into the
    // session cumulative, so delta-based reporting keeps totals / daily /
    // per-model breakdown honest. `last_model_call` (full context size) is
    // only a fallback when the CLI gave no cumulative reading this turn.
    let reported = if cumulative.is_some() {
        delta_usage
    } else if let Some(call) = last_model_call {
        call.to_openai_usage()
    } else {
        OaiUsage::zero()
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

    /// glm-5.2-ioa and other OpenAI-shape backends surface usage with
    /// `prompt_tokens`/`completion_tokens` instead of Claude's
    /// `input_tokens`/`output_tokens`. The extractor must fall back so the
    /// turn is recorded with real numbers instead of zeros.
    #[test]
    fn extract_cli_usage_falls_back_to_openai_field_names() {
        let u = json!({
            "prompt_tokens": 5000,
            "completion_tokens": 800,
            "cached_tokens": 3000,
        });
        assert_eq!(
            extract_cli_usage(Some(&u)),
            Some(CliUsage {
                input_tokens: 5000,
                output_tokens: 800,
                cache_read_input_tokens: 3000,
                cache_creation_input_tokens: 0,
            })
        );
    }

    /// When the usage object carries both Claude and OpenAI field names, the
    /// Claude-format `input_tokens` wins (a present-but-zero value is
    /// authoritative) so a genuine zero is not masked by a non-zero
    /// `prompt_tokens` from a different accounting axis.
    #[test]
    fn extract_cli_usage_prefers_claude_fields_when_both_present() {
        let u = json!({
            "input_tokens": 0,
            "prompt_tokens": 999,
            "output_tokens": 42,
            "completion_tokens": 1,
        });
        assert_eq!(
            extract_cli_usage(Some(&u)),
            Some(CliUsage {
                input_tokens: 0,
                output_tokens: 42,
                cache_read_input_tokens: 0,
                cache_creation_input_tokens: 0,
            })
        );
    }

    #[test]
    fn extract_cli_usage_none_when_absent() {
        assert_eq!(extract_cli_usage(None), None);
    }

    /// A usage object whose recognized fields are all zero carries no
    /// information and must be treated as absent, so it cannot become a
    /// `Some(zero)` that masks the cumulative-delta fallback. Regression
    /// for `finish=stop` turns reporting 0 tokens while the CLI's
    /// cumulative had advanced.
    #[test]
    fn extract_cli_usage_none_when_all_fields_zero() {
        let u = json!({"input_tokens": 0, "output_tokens": 0});
        assert_eq!(extract_cli_usage(Some(&u)), None);
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
        // OpenAI convention: prompt_tokens is the full cache-inclusive
        // prompt size; the cache fields stay display-only and must not be
        // added on top (that would double-count the cached prefix).
        assert_eq!(oai.prompt_tokens, 150_000);
        assert_eq!(oai.completion_tokens, 200);
        assert_eq!(oai.total_tokens, 150_000 + 200);
        assert_eq!(
            oai.prompt_tokens_details,
            Some(OaiPromptTokensDetails {
                cached_tokens: 80_000
            })
        );
        // Honest cache-hit count is preserved as a display-only field.
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
        // delta input = 150 (already cache-inclusive); delta cache_read = 40
        // stays a display-only breakdown field.
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

    /// With a cumulative `result.usage` reading, the reported usage must be the
    /// per-turn **delta** (net new tokens), not the `last_model_call`'s full
    /// context size. codex sums each turn's `prompt_tokens` into the session
    /// total, so reporting the full context would inflate the total by the
    /// entire context every turn. Here the delta's input is the cache-write
    /// jump (~629k) — that is the real consumption this turn and must be what
    /// surfaces in Settings → 用量. `prompt_tokens` follows the OpenAI
    /// cache-inclusive convention, so it equals the delta input as-is.
    #[test]
    fn reported_usage_uses_cumulative_delta_when_present() {
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
        let last_call = CliUsage {
            input_tokens: 39_217,
            output_tokens: 519,
            cache_read_input_tokens: 37_888,
            cache_creation_input_tokens: 1_329,
        };
        let (reported, next, delta) =
            resolve_reported_usage(Some(prev), Some(cumulative), Some(last_call));
        assert_eq!(reported.prompt_tokens, 629_096);
        assert_eq!(reported.completion_tokens, 1_839);
        assert_eq!(reported.cache_creation_input_tokens, Some(629_096));
        assert_eq!(delta.input_tokens, 629_096);
        assert_eq!(next, Some(cumulative));
    }

    /// When the CLI emitted no cumulative `result.usage` this turn, fall back to
    /// the per-call usage so we still report something rather than zero.
    #[test]
    fn reported_usage_falls_back_to_last_model_call_without_cumulative() {
        let prev = CliUsage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let last_call = CliUsage {
            input_tokens: 39_217,
            output_tokens: 519,
            cache_read_input_tokens: 37_888,
            cache_creation_input_tokens: 1_329,
        };
        let (reported, next, delta) =
            resolve_reported_usage(Some(prev), None, Some(last_call));
        // Fallback path passes the per-call usage through unchanged:
        // prompt_tokens is already cache-inclusive (39_217 covers the
        // 37_888 cache-read prefix).
        assert_eq!(reported.prompt_tokens, 39_217);
        assert_eq!(reported.completion_tokens, 519);
        // No cumulative → delta stays zero and the baseline is unchanged.
        assert_eq!(delta.input_tokens, 0);
        assert_eq!(next, Some(prev));
    }

    /// With neither a cumulative reading nor a per-call usage, report zero and
    /// keep the prior baseline.
    #[test]
    fn reported_usage_zero_when_neither_present() {
        let prev = CliUsage {
            input_tokens: 100,
            output_tokens: 10,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
        };
        let (reported, next, delta) = resolve_reported_usage(Some(prev), None, None);
        assert_eq!(reported, OaiUsage::zero());
        assert_eq!(delta.input_tokens, 0);
        assert_eq!(next, Some(prev));
    }
}
