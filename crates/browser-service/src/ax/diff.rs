//! Diffing two captures so a re-read only costs what changed.
//!
//! A capture is addressed by number, and the number belongs to the element, so
//! a diff is free to compare by number: the same number is the same element. An
//! insertion above an element does not renumber it, which is what keeps a diff
//! small on pages that grow from the top — a filled field gains a text child, a
//! toast appears above the content, and with positional numbers everything below
//! would be re-reported.
//!
//! The output mirrors the implementation this is modelled on:
//!
//! ```text
//! ~\t2 button Sign in, Help: Opens the menu
//! +\t3 text field Email
//! Removed element IDs: 4-6
//! ```
//!
//! Two guards keep a diff from being worse than a re-read:
//!
//! - **Nothing changed is its own answer**, one fixed sentence.
//! - **A diff that is not smaller is not used.** Below [`DiffOptions`]' ratios
//!   the caller falls back to the full state, which is easier to read anyway.

use crate::ax::render::Rendered;

/// The sentence a capture returns when nothing moved.
pub const UNCHANGED: &str = "There has been no change in the accessibility tree.";

/// The sentence that introduces a diff.
///
/// Deliberately short: on a real step the diff is two or three lines, and the
/// long form of this sentence used to be more than three quarters of it. The
/// prefixes are all it has to teach.
pub const DIFF_HEADER: &str = "Diff (~ changed, + added, removed by ID range):";

/// What a diff had to say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Diff {
    /// The tree did not change.
    Unchanged,
    /// A diff worth showing, header included.
    Changed(String),
    /// The diff was too long to be cheaper than the full state.
    TooLarge(DiffError),
}

/// Why a diff was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffError {
    /// The diff saved less than [`DiffOptions::min_saved_ratio`] of the full
    /// state.
    InsufficientSaving,
    /// The diff exceeded [`DiffOptions::max_lines`].
    LineBudget,
}

impl Diff {
    /// The text to hand to the model, if any.
    pub fn text(&self) -> Option<&str> {
        match self {
            Diff::Changed(text) => Some(text),
            Diff::Unchanged => Some(UNCHANGED),
            Diff::TooLarge(_) => None,
        }
    }
}

/// Thresholds for using a diff at all.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiffOptions {
    /// The diff must be at most this fraction of the full rendering.
    pub min_saved_ratio: f64,
    /// ... and must save at least this many bytes. A tiny page is cheaper to
    /// re-read than to diff, and a diff that is longer than the page teaches
    /// the model nothing.
    pub min_saved_bytes: usize,
    /// Hard cap on diff lines, counting the removed-id line.
    pub max_lines: usize,
}

impl Default for DiffOptions {
    fn default() -> Self {
        Self {
            min_saved_ratio: 0.5,
            min_saved_bytes: 512,
            max_lines: 400,
        }
    }
}

/// Diff `previous` against `next`.
pub fn diff(previous: &Rendered, next: &Rendered, options: DiffOptions) -> Diff {
    let mut before = std::collections::HashMap::with_capacity(previous.lines.len());
    for line in &previous.lines {
        before.insert(line.index, line);
    }
    let mut after = std::collections::HashSet::with_capacity(next.lines.len());
    for line in &next.lines {
        after.insert(line.index);
    }

    let mut body = String::new();
    let mut changes = 0usize;

    for line in &next.lines {
        let kind = match before.get(&line.index) {
            None => '+',
            Some(previous_line) => {
                // The number is the identity, so only the description can have
                // changed. Depth is part of the description: the same element
                // moving in the tree is worth knowing about.
                if previous_line.body == line.body && previous_line.depth == line.depth {
                    continue;
                }
                '~'
            }
        };
        changes += 1;
        if changes > options.max_lines {
            return Diff::TooLarge(DiffError::LineBudget);
        }
        body.push(kind);
        body.push_str(&line.text());
        body.push('\n');
    }

    let mut removed: Vec<u64> = previous
        .lines
        .iter()
        .filter(|line| !after.contains(&line.index))
        .map(|line| line.index)
        .collect();
    removed.sort_unstable();

    if changes == 0 && removed.is_empty() {
        return Diff::Unchanged;
    }

    if !removed.is_empty() {
        body.push_str("Removed element IDs: ");
        body.push_str(&compress_ranges(&removed));
        body.push('\n');
    }

    let full_bytes = next.text().len().max(1);
    let diff_bytes = body.len() + DIFF_HEADER.len() + 1;
    let saved = full_bytes.saturating_sub(diff_bytes);
    let small_enough = (diff_bytes as f64) <= (full_bytes as f64) * options.min_saved_ratio;
    if !small_enough && saved < options.min_saved_bytes {
        return Diff::TooLarge(DiffError::InsufficientSaving);
    }

    let mut text = String::with_capacity(diff_bytes + 1);
    text.push_str(DIFF_HEADER);
    text.push('\n');
    text.push_str(&body);
    Diff::Changed(text)
}

/// Collapse consecutive numbers into ranges: `[3,4,5,9]` -> `3-5, 9`.
///
/// A removed subtree is usually contiguous, so the range form turns a teardown
/// into a handful of numbers instead of a page of them.
fn compress_ranges(ids: &[u64]) -> String {
    let mut parts = Vec::new();
    let mut index = 0;
    while index < ids.len() {
        let start = ids[index];
        let mut end = start;
        let mut run = 1;
        while index + 1 < ids.len() && ids[index + 1] == end + 1 {
            index += 1;
            end = ids[index];
            run += 1;
        }
        if run >= 3 {
            parts.push(format!("{start}-{end}"));
        } else if run == 2 {
            parts.push(start.to_string());
            parts.push(end.to_string());
        } else {
            parts.push(start.to_string());
        }
        index += 1;
    }
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ax::render::{Line, Rendered};

    /// Build a rendering from `(number, depth, body)` triples.
    fn rendered(lines: &[(u64, usize, &str)]) -> Rendered {
        Rendered {
            lines: lines
                .iter()
                .map(|(index, depth, body)| Line {
                    index: *index,
                    depth: *depth,
                    body: (*body).to_string(),
                    backend: None,
                })
                .collect(),
            omitted: 0,
        }
    }

    fn many(count: usize) -> Vec<(u64, usize, String)> {
        (0..count)
            .map(|index| {
                (
                    index as u64,
                    1usize,
                    format!("button Button {index} with a reasonably long label"),
                )
            })
            .collect()
    }

    fn as_rendered(lines: &[(u64, usize, String)]) -> Rendered {
        Rendered {
            lines: lines
                .iter()
                .map(|(index, depth, body)| Line {
                    index: *index,
                    depth: *depth,
                    body: body.clone(),
                    backend: None,
                })
                .collect(),
            omitted: 0,
        }
    }

    #[test]
    fn an_identical_capture_reports_the_fixed_sentence() {
        let lines = rendered(&[(1, 1, "heading Hi"), (2, 1, "button Go")]);
        assert_eq!(diff(&lines, &lines, DiffOptions::default()), Diff::Unchanged);
        assert_eq!(
            Diff::Unchanged.text(),
            Some("There has been no change in the accessibility tree.")
        );
    }

    /// Options that let a tiny synthetic page produce a diff at all; the
    /// saving thresholds are calibrated for real pages.
    fn relaxed() -> DiffOptions {
        DiffOptions {
            min_saved_bytes: 0,
            min_saved_ratio: 1.0,
            ..DiffOptions::default()
        }
    }

    #[test]
    fn a_changed_line_keeps_its_prefix_and_number() {
        let before = rendered(&[(1, 1, "textbox Email, Value: "), (2, 1, "button Go")]);
        let after = rendered(&[(1, 1, "textbox Email, Value: a@b.c"), (2, 1, "button Go")]);
        let text = diff(&before, &after, relaxed()).text().unwrap().to_string();
        assert!(text.starts_with(DIFF_HEADER), "got {text:?}");
        assert!(text.contains("\n~\t1 textbox Email, Value: a@b.c"), "got {text:?}");
        assert!(!text.contains("button Go"), "unchanged lines are not re-sent");
    }

    #[test]
    fn a_new_element_is_prefixed_with_a_plus() {
        let before = rendered(&[(1, 1, "heading Hi")]);
        let after = rendered(&[(1, 1, "heading Hi"), (2, 1, "button New")]);
        let text = diff(&before, &after, relaxed()).text().unwrap().to_string();
        assert!(text.contains("+\t2 button New"), "got {text:?}");
    }

    /// An element that moves in the tree keeps its number, so its description
    /// is not re-sent. This is what makes a diff cheap on a page that grows from
    /// the top.
    #[test]
    fn a_moved_but_unchanged_line_is_not_re_reported() {
        let before = rendered(&[(10, 1, "button A"), (11, 1, "button B")]);
        // A disappeared; B is still B, at the same depth, with the same number.
        let after = rendered(&[(11, 1, "button B")]);
        match diff(&before, &after, relaxed()) {
            Diff::Changed(text) => {
                assert!(!text.contains("button B"), "unchanged line re-sent: {text:?}");
                assert!(text.contains("Removed element IDs: 10"), "got {text:?}");
            }
            other => panic!("a removal is still a change, got {other:?}"),
        }
    }

    #[test]
    fn removed_elements_are_summarized_by_range() {
        let before_lines = many(40);
        let before = as_rendered(&before_lines);
        let kept: Vec<(u64, usize, String)> = before_lines
            .iter()
            .filter(|(index, _, _)| !matches!(index, 2 | 3 | 4 | 9))
            .map(|(index, depth, body)| (*index, *depth, body.clone()))
            .collect();
        let after = as_rendered(&kept);
        let text = diff(&before, &after, DiffOptions::default())
            .text()
            .unwrap()
            .to_string();
        assert!(text.contains("Removed element IDs: 2-4, 9"), "got {text:?}");
    }

    #[test]
    fn a_removal_alone_is_still_a_change() {
        let before_lines = many(40);
        let before = as_rendered(&before_lines);
        let kept: Vec<(u64, usize, String)> = before_lines
            .iter()
            .filter(|(index, _, _)| *index != 7)
            .map(|(index, depth, body)| (*index, *depth, body.clone()))
            .collect();
        let after = as_rendered(&kept);
        let text = diff(&before, &after, DiffOptions::default());
        assert!(
            text.text()
                .is_some_and(|text| text.contains("Removed element IDs: 7"))
        );
    }

    #[test]
    fn a_diff_that_saves_nothing_is_rejected() {
        let before = rendered(&[(1, 1, "button A")]);
        let after = rendered(&[(1, 1, "button B")]);
        assert_eq!(
            diff(
                &before,
                &after,
                DiffOptions {
                    min_saved_bytes: 10_000,
                    ..DiffOptions::default()
                }
            ),
            Diff::TooLarge(DiffError::InsufficientSaving)
        );
    }

    #[test]
    fn a_huge_diff_is_rejected_by_the_line_budget() {
        let before = rendered(&[]);
        let lines = many(500);
        let after = as_rendered(&lines);
        assert_eq!(
            diff(&before, &after, DiffOptions::default()),
            Diff::TooLarge(DiffError::LineBudget)
        );
    }

    #[test]
    fn a_small_change_on_a_big_page_is_accepted() {
        let lines = many(40);
        let before = as_rendered(&lines);
        let mut changed = lines.clone();
        changed[0].2 = "button Renamed with a reasonably long label".to_string();
        let after = as_rendered(&changed);
        match diff(&before, &after, DiffOptions::default()) {
            Diff::Changed(text) => assert!(text.contains("~\t0 button Renamed"), "got {text:?}"),
            other => panic!("expected a diff, got {other:?}"),
        }
    }

    #[test]
    fn ranges_use_a_dash_only_for_three_or_more() {
        assert_eq!(compress_ranges(&[1, 2]), "1, 2");
        assert_eq!(compress_ranges(&[1, 2, 3]), "1-3");
        assert_eq!(compress_ranges(&[1, 3, 4, 5, 8]), "1, 3-5, 8");
        assert_eq!(compress_ranges(&[]), "");
    }
}
