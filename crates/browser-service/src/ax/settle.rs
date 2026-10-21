//! Waiting for a page to stop moving before reading it.
//!
//! A capture taken mid-update is worse than a slow one: the model reads a tree
//! that never existed, then acts on it. The original fix for this — and the one
//! copied here — is to wait for the page to go **quiet**, not to sleep for a
//! guessed duration.
//!
//! "Quiet" means a cheap page signature has stopped changing for a short window.
//! The signature is deliberately cheap (`readyState`, element count, text
//! length): polling a full accessibility tree to detect stability would cost
//! more than the capture it is protecting.
//!
//! The deadline matters as much as the window. A page with a blinking cursor or
//! a live clock never goes quiet, and waiting for it forever would hang the
//! turn; after [`SettleOptions::deadline`] the current state is accepted as-is.

use std::future::Future;
use std::time::Duration;

use tokio::time::{Instant, sleep};

/// How long to wait for quiet, and how hard to look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettleOptions {
    /// The signature must be unchanged for this long to count as quiet.
    pub quiet: Duration,
    /// Give up and accept the current state after this long.
    pub deadline: Duration,
    /// How often to re-read the signature.
    pub poll: Duration,
}

impl Default for SettleOptions {
    fn default() -> Self {
        // Straight from the implementation this copies: a 100ms quiet window, a
        // 25ms poll, and a 750ms ceiling.
        Self {
            quiet: Duration::from_millis(100),
            deadline: Duration::from_millis(750),
            poll: Duration::from_millis(25),
        }
    }
}

/// Why settling stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleOutcome {
    /// The signature held still for the quiet window.
    Quiet,
    /// The deadline passed first; the returned state is whatever was current.
    Deadline,
}

/// Poll `probe` until its output stops changing, or the deadline passes.
///
/// Returns the last signature seen, which the caller may use as the baseline
/// for the next settle without probing again.
pub async fn settle<F, Fut>(options: SettleOptions, mut probe: F) -> (SettleOutcome, String)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = String>,
{
    let start = Instant::now();
    let deadline = start + options.deadline;
    let mut current = probe().await;
    let mut last_change = start;

    loop {
        if Instant::now() >= deadline {
            return (SettleOutcome::Deadline, current);
        }
        // Never sleep past the deadline: a slow poll should shorten the wait,
        // not extend it.
        let sleep_for = options.poll.min(deadline.saturating_duration_since(Instant::now()));
        sleep(sleep_for).await;

        let next = probe().await;
        if next != current {
            current = next;
            last_change = Instant::now();
            continue;
        }
        if Instant::now().saturating_duration_since(last_change) >= options.quiet {
            return (SettleOutcome::Quiet, current);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn options() -> SettleOptions {
        SettleOptions {
            quiet: Duration::from_millis(100),
            deadline: Duration::from_millis(750),
            poll: Duration::from_millis(25),
        }
    }

    /// A probe that returns each value in turn and then repeats the last one.
    async fn run_probe(values: Vec<&'static str>) -> (SettleOutcome, String, Duration) {
        tokio::time::pause();
        let step = Cell::new(0usize);
        let last = values.len() - 1;
        let started = Instant::now();
        let (outcome, signature) = settle(options(), || {
            let index = step.get();
            step.set((index + 1).min(last));
            let value = values[index];
            async move { value.to_string() }
        })
        .await;
        let elapsed = Instant::now().saturating_duration_since(started);
        (outcome, signature, elapsed)
    }

    #[tokio::test]
    async fn a_stable_page_returns_after_the_quiet_window() {
        let (outcome, signature, elapsed) = run_probe(vec!["a"]).await;
        assert_eq!(outcome, SettleOutcome::Quiet);
        assert_eq!(signature, "a");
        // Virtual time advances in timer-sized steps, so the window is a lower
        // bound rather than an exact instant.
        assert!(
            elapsed >= Duration::from_millis(100) && elapsed <= Duration::from_millis(110),
            "one quiet window, not more: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn a_changing_page_is_followed_until_it_stops() {
        let (outcome, signature, elapsed) = run_probe(vec!["a", "b", "c"]).await;
        assert_eq!(outcome, SettleOutcome::Quiet);
        assert_eq!(signature, "c");
        assert!(
            elapsed >= Duration::from_millis(150) && elapsed <= Duration::from_millis(165),
            "two polls of change, then one quiet window: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn a_page_that_never_settles_stops_at_the_deadline() {
        tokio::time::pause();
        let tick = Cell::new(0u64);
        let started = Instant::now();
        let (outcome, _) = settle(options(), || {
            tick.set(tick.get() + 1);
            let value = format!("{}", tick.get());
            async move { value }
        })
        .await;
        let elapsed = Instant::now().saturating_duration_since(started);
        assert_eq!(outcome, SettleOutcome::Deadline);
        assert!(
            elapsed >= Duration::from_millis(750) && elapsed <= Duration::from_millis(760),
            "the deadline is a ceiling, not a suggestion: {elapsed:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_slow_probe_does_not_overrun_the_deadline() {
        let started = Instant::now();
        let mut calls = 0;
        let (outcome, _) = settle(
            SettleOptions {
                quiet: Duration::from_millis(10),
                deadline: Duration::from_millis(30),
                poll: Duration::from_secs(5),
            },
            || {
                calls += 1;
                async move { format!("{calls}") }
            },
        )
        .await;
        let elapsed = Instant::now().saturating_duration_since(started);
        assert_eq!(outcome, SettleOutcome::Deadline);
        assert!(elapsed <= Duration::from_millis(30), "elapsed {elapsed:?}");
    }
}
