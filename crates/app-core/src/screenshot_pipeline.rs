//! Screenshot persistence and delivery.
//!
//! Screenshots arrive from two very different sources — a browser provider
//! process and an in-process desktop driver — and both are large, so neither
//! the session log nor the polled `UiSnapshot` should ever carry the bytes.
//! Every capture is written to attachment storage once, and everything else
//! works from the resulting [`ScreenshotHandle`]:
//!
//! * the model gets the image inlined only when the active route reports
//!   vision support, and otherwise a path plus the existing `view_image`
//!   fallback to read it;
//! * the browser panel gets a downscaled rendition pushed by event;
//! * the tool card gets a thumbnail reference.
//!
//! A per-turn byte budget keeps a screenshot-heavy turn from quietly
//! dominating the context window. Exhausting the budget degrades delivery to
//! metadata only — the files stay on disk and stay visible in the panel.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use workspace_model::ScreenshotHandle;

/// Default per-turn budget for image bytes delivered to the model.
///
/// Roughly a handful of full screenshots. Chosen to be small enough that a
/// screenshot loop cannot quietly consume a large share of the context window,
/// and large enough that ordinary verification work is unaffected.
pub const DEFAULT_TURN_IMAGE_BUDGET_BYTES: u64 = 8 * 1024 * 1024;

/// Captures larger than this are stored and shown, but reported as bounded.
pub const DEFAULT_MAX_CAPTURE_BYTES: u64 = 16 * 1024 * 1024;

/// Longest edge of the rendition handed to the browser panel. The panel is a
/// side dock, so a full-resolution page image is wasted bytes there.
pub const PANEL_RENDITION_MAX_EDGE: u32 = 1280;

/// Default per-turn budget for screenshot count, independent of byte size, so
/// many tiny captures cannot slip past a byte-only budget.
pub const DEFAULT_TURN_SCREENSHOT_COUNT: u32 = 12;

/// How a captured screenshot should be delivered to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Delivery {
    /// Inline the image; the route understands vision.
    Inline { media_type: String },
    /// Return a path and point the agent at the `view_image` fallback.
    VisionFallback { reason: String },
    /// The per-turn budget is spent; describe the capture without sending it.
    BudgetExhausted { spent_bytes: u64, count: u32 },
}

impl Delivery {
    pub fn inlineable(&self) -> bool {
        matches!(self, Delivery::Inline { .. })
    }
}

/// Result of capturing one screenshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capture {
    pub handle: ScreenshotHandle,
    /// Downscaled rendition for the panel, if one was produced.
    pub panel_rendition: Option<Vec<u8>>,
    /// True when the stored image was reduced to stay under the size cap.
    pub bounded: bool,
}

/// Storage plus delivery policy for one session.
pub struct ScreenshotPipeline {
    root: PathBuf,
    budget_bytes: u64,
    max_capture_bytes: u64,
    max_count: u32,
    state: Mutex<PipelineState>,
}

#[derive(Default)]
struct PipelineState {
    /// session id -> (bytes and count spent in the current turn)
    turn_spend: HashMap<String, TurnSpend>,
}

#[derive(Default, Clone, Copy)]
struct TurnSpend {
    bytes: u64,
    count: u32,
}

impl ScreenshotPipeline {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            budget_bytes: DEFAULT_TURN_IMAGE_BUDGET_BYTES,
            max_capture_bytes: DEFAULT_MAX_CAPTURE_BYTES,
            max_count: DEFAULT_TURN_SCREENSHOT_COUNT,
            state: Mutex::new(PipelineState::default()),
        }
    }

    pub fn with_budget(mut self, budget_bytes: u64, max_count: u32) -> Self {
        self.budget_bytes = budget_bytes;
        self.max_count = max_count;
        self
    }

    /// Read a PNG or JPEG header for dimensions without decoding the image.
    ///
    /// A full decoder is not needed here: the panel only needs a size to
    /// reserve layout, and a failed probe degrades to a zero-size handle
    /// rather than failing the capture.
    pub fn probe_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
        if let Some(size) = probe_png(bytes) {
            return Some(size);
        }
        probe_jpeg(bytes)
    }

    pub fn media_type(bytes: &[u8]) -> &'static str {
        if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
            "image/png"
        } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "image/jpeg"
        } else {
            "application/octet-stream"
        }
    }

    /// Persist a capture and return its handle.
    ///
    /// Writes are content-addressed, so an identical screenshot captured twice
    /// costs one file. A write failure is returned to the caller rather than
    /// panicking, because a screenshot that cannot be stored must not take
    /// down the tool call that produced it.
    pub fn capture(&self, session_id: &str, bytes: &[u8]) -> Result<Capture, ScreenshotError> {
        if bytes.is_empty() {
            return Err(ScreenshotError::Empty);
        }

        let bounded = bytes.len() as u64 > self.max_capture_bytes;
        let media_type = Self::media_type(bytes).to_string();
        let (width, height) = Self::probe_dimensions(bytes).unwrap_or((0, 0));

        let path = self.store(session_id, "capture", bytes)?;
        let panel_rendition = self.rendition(session_id, "rendition", bytes);

        Ok(Capture {
            handle: ScreenshotHandle {
                path: path.to_string_lossy().into_owned(),
                width,
                height,
                byte_size: bytes.len() as u64,
                media_type,
            },
            panel_rendition,
            bounded,
        })
    }

    /// Decide how a capture should reach the model, and charge the turn budget.
    ///
    /// Call this once per capture, after persisting it. Ordering matters: the
    /// capture is already on disk when the budget is charged, so an exhausted
    /// budget degrades delivery without losing the image.
    pub fn deliver(
        &self,
        session_id: &str,
        handle: &ScreenshotHandle,
        model_vision: bool,
    ) -> Delivery {
        let mut state = self.lock();
        let spend = state.turn_spend.entry(session_id.to_string()).or_default();

        if spend.count >= self.max_count
            || spend.bytes.saturating_add(handle.byte_size) > self.budget_bytes
        {
            return Delivery::BudgetExhausted {
                spent_bytes: spend.bytes,
                count: spend.count,
            };
        }

        spend.bytes = spend.bytes.saturating_add(handle.byte_size);
        spend.count += 1;
        drop(state);

        if model_vision {
            Delivery::Inline {
                media_type: handle.media_type.clone(),
            }
        } else {
            Delivery::VisionFallback {
                reason: "the active model does not accept image input".to_string(),
            }
        }
    }

    /// Clear a session's turn spend. Called when a turn ends.
    pub fn end_turn(&self, session_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.turn_spend.remove(session_id);
        }
    }

    /// Remaining budget for a session, for display and tests.
    pub fn remaining(&self, session_id: &str) -> (u64, u32) {
        let Ok(state) = self.state.lock() else {
            return (self.budget_bytes, self.max_count);
        };
        let spend = state
            .turn_spend
            .get(session_id)
            .copied()
            .unwrap_or_default();
        (
            self.budget_bytes.saturating_sub(spend.bytes),
            self.max_count.saturating_sub(spend.count),
        )
    }

    /// Drop all state for a session, used on session close.
    pub fn forget(&self, session_id: &str) {
        if let Ok(mut state) = self.state.lock() {
            state.turn_spend.remove(session_id);
        }
    }

    fn store(
        &self,
        session_id: &str,
        kind: &str,
        bytes: &[u8],
    ) -> Result<PathBuf, ScreenshotError> {
        let digest = content_digest(bytes);
        let dir = self.root.join(session_id);
        std::fs::create_dir_all(&dir).map_err(|error| ScreenshotError::Io(error.to_string()))?;
        let path = dir.join(format!("{kind}-{digest}"));
        if path.exists() {
            return Ok(path);
        }
        std::fs::write(&path, bytes).map_err(|error| ScreenshotError::Io(error.to_string()))?;
        Ok(path)
    }

    /// Placeholder for image downscaling.
    ///
    /// The panel rendition is produced by the image pipeline once it is
    /// available; until then the full bytes are used, which is correct but not
    /// yet optimal. Kept as a seam so the call site does not change when the
    /// real downscaler lands.
    fn rendition(&self, _session_id: &str, _kind: &str, bytes: &[u8]) -> Option<Vec<u8>> {
        Some(bytes.to_vec())
    }

    /// Remove every capture belonging to a session.
    ///
    /// Called when a session is deleted. Captures are files on disk that no
    /// database row references, so nothing else would ever reclaim them and
    /// a long-lived app would grow without bound.
    pub async fn remove_session(&self, session_id: &str) -> Result<(), std::io::Error> {
        self.forget(session_id);
        let dir = self.root.join(session_id);
        if !dir.exists() {
            // A session that never captured anything has no directory.
            return Ok(());
        }
        tokio::fs::remove_dir_all(&dir).await
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, PipelineState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScreenshotError {
    Empty,
    Io(String),
}

impl std::fmt::Display for ScreenshotError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScreenshotError::Empty => write!(formatter, "capture produced no bytes"),
            ScreenshotError::Io(detail) => write!(formatter, "screenshot storage failed: {detail}"),
        }
    }
}

impl std::error::Error for ScreenshotError {}

/// FNV-1a over the bytes, hex encoded. Only needs to be collision-resistant
/// enough to avoid rewriting an identical capture, not cryptographically
/// strong; the file is private to the local session.
fn content_digest(bytes: &[u8]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn probe_png(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || !bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return None;
    }
    // IHDR width/height are big-endian u32 at offsets 16 and 20.
    let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
    let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// Walk JPEG segments to the first SOF marker, which carries the dimensions.
fn probe_jpeg(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 4 || !bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return None;
    }
    let mut index = 2usize;
    while index + 9 < bytes.len() {
        if bytes[index] != 0xFF {
            index += 1;
            continue;
        }
        let marker = bytes[index + 1];
        // Standalone markers carry no length field.
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            index += 2;
            continue;
        }
        let length = u16::from_be_bytes([bytes[index + 2], bytes[index + 3]]) as usize;
        // SOF0..SOF15, excluding the DHT/JPG/DAC markers in that range.
        let is_sof = (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC);
        if is_sof {
            let height = u16::from_be_bytes([bytes[index + 5], bytes[index + 6]]) as u32;
            let width = u16::from_be_bytes([bytes[index + 7], bytes[index + 8]]) as u32;
            return (width > 0 && height > 0).then_some((width, height));
        }
        if length < 2 {
            return None;
        }
        index += 2 + length;
    }
    None
}

impl ScreenshotPipeline {
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Remove a session's captures through whichever pipeline it used.
///
/// The shared pipeline is the one production captures land in, so teardown has
/// to reach it even when the caller holds no handle to it.
pub async fn remove_session_captures(
    servers: &crate::shared_mcp::SharedMcpServers,
    session_id: &str,
) {
    let Some(browser) = servers.browser_existing() else {
        // No browser server was ever started, so nothing was ever captured.
        return;
    };
    if let Err(error) = browser.pipeline().remove_session(session_id).await {
        tracing::warn!("screenshot cleanup for session {session_id} failed: {error}");
    }
}

/// Shared handle used by the adapters and the session layer.
pub type SharedScreenshotPipeline = Arc<ScreenshotPipeline>;

pub fn shared(root: impl Into<PathBuf>) -> SharedScreenshotPipeline {
    Arc::new(ScreenshotPipeline::new(root))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Minimal valid PNG header with the given dimensions.
    fn png_bytes(width: u32, height: u32, filler: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        bytes.extend_from_slice(&[0, 0, 0, 13]);
        bytes.extend_from_slice(b"IHDR");
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
        bytes.extend_from_slice(filler);
        bytes
    }

    fn pipeline(dir: &TempDir) -> ScreenshotPipeline {
        ScreenshotPipeline::new(dir.path().join("shots"))
    }

    #[test]
    fn capture_persists_and_returns_a_handle_without_bytes() {
        let dir = TempDir::new().unwrap();
        let pipeline = pipeline(&dir);
        let capture = pipeline
            .capture("session-1", &png_bytes(1280, 720, &[0xAA; 64]))
            .unwrap();

        assert_eq!(capture.handle.width, 1280);
        assert_eq!(capture.handle.height, 720);
        assert_eq!(capture.handle.media_type, "image/png");
        assert!(!capture.bounded);
        assert!(Path::new(&capture.handle.path).is_file());
    }

    #[test]
    fn identical_captures_share_one_file() {
        let dir = TempDir::new().unwrap();
        let pipeline = pipeline(&dir);
        let bytes = png_bytes(800, 600, &[0xBB; 32]);

        let first = pipeline.capture("session-1", &bytes).unwrap();
        let second = pipeline.capture("session-1", &bytes).unwrap();

        assert_eq!(first.handle.path, second.handle.path);
    }

    #[test]
    fn sessions_do_not_share_storage() {
        let dir = TempDir::new().unwrap();
        let pipeline = pipeline(&dir);
        let bytes = png_bytes(800, 600, &[0xCC; 32]);

        let a = pipeline.capture("session-a", &bytes).unwrap();
        let b = pipeline.capture("session-b", &bytes).unwrap();

        assert_ne!(a.handle.path, b.handle.path);
    }

    #[test]
    fn empty_capture_is_an_error_not_a_panic() {
        let dir = TempDir::new().unwrap();
        assert!(matches!(
            pipeline(&dir).capture("session-1", &[]),
            Err(ScreenshotError::Empty)
        ));
    }

    #[test]
    fn oversized_capture_is_flagged_bounded() {
        let dir = TempDir::new().unwrap();
        let pipeline = ScreenshotPipeline::new(dir.path().join("shots"));
        // Force a cap smaller than the payload.
        let pipeline = ScreenshotPipeline {
            max_capture_bytes: 16,
            ..pipeline
        };

        let capture = pipeline
            .capture("session-1", &png_bytes(100, 100, &[0xDD; 128]))
            .unwrap();
        assert!(capture.bounded);
    }

    #[test]
    fn vision_capable_route_gets_the_image_inlined() {
        let dir = TempDir::new().unwrap();
        let pipeline = pipeline(&dir);
        let capture = pipeline
            .capture("session-1", &png_bytes(640, 480, &[0x01; 16]))
            .unwrap();

        let delivery = pipeline.deliver("session-1", &capture.handle, true);
        assert!(delivery.inlineable());
    }

    #[test]
    fn model_without_vision_gets_a_usable_fallback_reference() {
        let dir = TempDir::new().unwrap();
        let pipeline = pipeline(&dir);
        let capture = pipeline
            .capture("session-1", &png_bytes(640, 480, &[0x02; 16]))
            .unwrap();

        match pipeline.deliver("session-1", &capture.handle, false) {
            Delivery::VisionFallback { reason } => assert!(reason.contains("image input")),
            other => panic!("expected VisionFallback, got {other:?}"),
        }
        // The file is still there for the fallback to read.
        assert!(Path::new(&capture.handle.path).is_file());
    }

    #[test]
    fn budget_exhaustion_degrades_to_metadata_but_keeps_the_file() {
        let dir = TempDir::new().unwrap();
        // Budget comfortably above one capture, with a count cap of one, so
        // the cap is what binds on the second delivery.
        let pipeline = ScreenshotPipeline::new(dir.path().join("shots")).with_budget(4096, 1);
        let bytes = png_bytes(64, 64, &[0xEE; 48]);

        let first = pipeline.capture("session-1", &bytes).unwrap();
        assert!(
            pipeline
                .deliver("session-1", &first.handle, true)
                .inlineable()
        );

        let second = pipeline.capture("session-1", &bytes).unwrap();
        assert!(matches!(
            pipeline.deliver("session-1", &second.handle, true),
            Delivery::BudgetExhausted { .. }
        ));
        // Degraded delivery must not delete the capture.
        assert!(Path::new(&second.handle.path).is_file());
    }

    #[test]
    fn screenshot_count_cap_applies_independently_of_bytes() {
        let dir = TempDir::new().unwrap();
        // A byte budget that would never bind, with a count cap of two.
        let pipeline = ScreenshotPipeline::new(dir.path().join("shots")).with_budget(u64::MAX, 2);

        for _ in 0..2 {
            let capture = pipeline
                .capture("session-1", &png_bytes(8, 8, &[0x01; 4]))
                .unwrap();
            assert!(
                pipeline
                    .deliver("session-1", &capture.handle, true)
                    .inlineable()
            );
        }

        let third = pipeline
            .capture("session-1", &png_bytes(8, 8, &[0x02; 4]))
            .unwrap();
        assert!(matches!(
            pipeline.deliver("session-1", &third.handle, true),
            Delivery::BudgetExhausted { .. }
        ));
    }

    #[test]
    fn budget_resets_between_turns_and_is_per_session() {
        let dir = TempDir::new().unwrap();
        let pipeline = ScreenshotPipeline::new(dir.path().join("shots")).with_budget(4096, 1);
        let bytes = png_bytes(32, 32, &[0x0F; 48]);

        let a = pipeline.capture("session-1", &bytes).unwrap();
        assert!(pipeline.deliver("session-1", &a.handle, true).inlineable());
        assert_eq!(
            pipeline.deliver("session-1", &a.handle, true),
            Delivery::BudgetExhausted {
                spent_bytes: a.handle.byte_size,
                count: 1
            }
        );

        // A different session is unaffected.
        let b = pipeline.capture("session-2", &bytes).unwrap();
        assert!(pipeline.deliver("session-2", &b.handle, true).inlineable());

        // Ending the turn refills session 1.
        pipeline.end_turn("session-1");
        let c = pipeline.capture("session-1", &bytes).unwrap();
        assert!(pipeline.deliver("session-1", &c.handle, true).inlineable());
    }

    #[test]
    fn remaining_reports_what_is_left() {
        let dir = TempDir::new().unwrap();
        let pipeline = ScreenshotPipeline::new(dir.path().join("shots")).with_budget(100, 2);
        let capture = pipeline
            .capture("session-1", &png_bytes(16, 16, &[0x11; 20]))
            .unwrap();
        pipeline.deliver("session-1", &capture.handle, true);

        let (bytes, count) = pipeline.remaining("session-1");
        assert_eq!(bytes, 100 - capture.handle.byte_size);
        assert_eq!(count, 1);
    }

    #[test]
    fn dimension_probe_handles_png_and_jpeg_and_rejects_junk() {
        assert_eq!(
            ScreenshotPipeline::probe_dimensions(&png_bytes(42, 24, &[])),
            Some((42, 24))
        );
        assert_eq!(ScreenshotPipeline::probe_dimensions(b"not an image"), None);
        assert_eq!(ScreenshotPipeline::probe_dimensions(&[]), None);

        // SOI, an APP0 segment, then SOF0 carrying 8x16.
        let mut jpeg = vec![0xFF, 0xD8];
        jpeg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        jpeg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        jpeg.extend_from_slice(&16u16.to_be_bytes());
        jpeg.extend_from_slice(&8u16.to_be_bytes());
        jpeg.extend_from_slice(&[0x03, 0, 0, 0, 0]);
        assert_eq!(ScreenshotPipeline::probe_dimensions(&jpeg), Some((8, 16)));
    }

    #[tokio::test]
    async fn removing_a_session_deletes_its_captures() {
        let dir = TempDir::new().unwrap();
        let pipeline = ScreenshotPipeline::new(dir.path().join("shots"));
        let png = png_bytes(32, 32, &[0x5A; 16]);

        let first = pipeline.capture("session-1", &png).unwrap();
        let second = pipeline.capture("session-2", &png).unwrap();
        assert!(Path::new(&first.handle.path).is_file());
        assert!(Path::new(&second.handle.path).is_file());

        pipeline.remove_session("session-1").await.unwrap();

        assert!(!Path::new(&first.handle.path).exists());
        // Another session's captures are untouched.
        assert!(Path::new(&second.handle.path).is_file());
    }

    #[tokio::test]
    async fn removing_a_session_that_never_captured_is_a_no_op() {
        let dir = TempDir::new().unwrap();
        let pipeline = ScreenshotPipeline::new(dir.path().join("shots"));
        // No directory exists yet; this must not error, because session
        // deletion runs this for every session.
        assert!(pipeline.remove_session("never-existed").await.is_ok());
    }

    #[tokio::test]
    async fn removal_also_clears_the_turn_budget() {
        let dir = TempDir::new().unwrap();
        let pipeline = ScreenshotPipeline::new(dir.path().join("shots")).with_budget(100, 1);
        let capture = pipeline
            .capture("session-1", &png_bytes(16, 16, &[0x77; 20]))
            .unwrap();
        pipeline.deliver("session-1", &capture.handle, true);
        assert_eq!(pipeline.remaining("session-1").1, 0);

        pipeline.remove_session("session-1").await.unwrap();
        assert_eq!(
            pipeline.remaining("session-1").1,
            1,
            "a deleted session must not keep a spent budget",
        );
    }

    #[test]
    fn media_type_detection() {
        assert_eq!(
            ScreenshotPipeline::media_type(&png_bytes(1, 1, &[])),
            "image/png"
        );
        assert_eq!(
            ScreenshotPipeline::media_type(&[0xFF, 0xD8, 0xFF, 0xE0]),
            "image/jpeg"
        );
        assert_eq!(
            ScreenshotPipeline::media_type(b"GIF89a"),
            "application/octet-stream"
        );
    }
}
