//! Auto-update download progress: shared observable state plus throttled
//! console logs and `update-progress` events.
//!
//! The served GUI is a remote-origin SPA without `window.__TAURI__` IPC, so
//! the progress events are consumed by local pages (splash, plugin manager)
//! and by Rust-side state machines (e.g. the update menu state machine in
//! `updater.rs`); the throttled `[dsh-desktop]` console logs are always
//! visible in the app log regardless of UI.
//!
//! The wiring into the updater plugin lives in `updater.rs`
//! (`Update::download` callbacks); this module owns the accounting,
//! throttling, event emission and the shared snapshot. The event name and the
//! `version`/`done` payload fields follow the update state machine in
//! `updater.rs` (X1's `update-progress` contract); this module adds
//! `percent`, `phase` and a terminal `downloaded` event, and throttles the
//! per-chunk noise.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use tauri::{AppHandle, Emitter};

/// Event name for download progress (payload: `progress_payload`), shared
/// with the update state machine in `updater.rs`.
pub const PROGRESS_EVENT: &str = "update-progress";

/// Maximum frequency of log lines / events during a download.
const REPORT_INTERVAL: Duration = Duration::from_millis(500);

/// Lifecycle phase of an update download.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProgressPhase {
    #[default]
    Idle,
    Downloading,
    Downloaded,
    Failed,
}

impl ProgressPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Downloading => "downloading",
            Self::Downloaded => "downloaded",
            Self::Failed => "failed",
        }
    }
}

/// One observable progress snapshot (the app-managed state and the event
/// payload share this shape).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateProgress {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub percent: Option<u8>,
    pub phase: ProgressPhase,
}

/// App-managed handle to the latest snapshot; anything with the app handle
/// can read it, and local pages can subscribe to [`PROGRESS_EVENT`].
#[derive(Default)]
pub struct ProgressState(pub Arc<Mutex<Option<UpdateProgress>>>);

/// Pure download accounting + throttling decisions (unit-testable without an
/// app handle). Percent is derived from the HTTP `Content-Length` when the
/// server provides one; chunked responses leave `total` unknown.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProgressStats {
    pub downloaded: u64,
    pub total: Option<u64>,
    pub percent: Option<u8>,
}

impl ProgressStats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one received chunk; updates running totals and percent.
    pub fn record_chunk(&mut self, chunk_len: usize, total: Option<u64>) {
        self.downloaded = self.downloaded.saturating_add(chunk_len as u64);
        if total.is_some() {
            self.total = total;
        }
        self.percent = self.total.map(|t| percent_of(self.downloaded, t));
    }
}

/// Whole-number percentage, clamped to 100; 0 for a zero/unknown total.
pub fn percent_of(downloaded: u64, total: u64) -> u8 {
    if total == 0 {
        0
    } else {
        (downloaded.saturating_mul(100) / total).min(100) as u8
    }
}

/// Throttle decision: report when the percent jumped (>= 1 point) or when
/// `interval` has elapsed since the last report.
pub fn should_report(prev_percent: Option<u8>, percent: Option<u8>, elapsed: Duration, interval: Duration) -> bool {
    if percent != prev_percent {
        return true;
    }
    elapsed >= interval
}

/// The `update-progress` event payload (superset of the state machine's
/// `{version, downloaded, total, done}` contract: adds `percent` and `phase`).
pub fn progress_payload(stats: &ProgressStats, phase: ProgressPhase, version: &str) -> serde_json::Value {
    json!({
        "version": version,
        "downloaded": stats.downloaded,
        "total": stats.total,
        "percent": stats.percent,
        "phase": phase.as_str(),
        "done": phase == ProgressPhase::Downloaded,
    })
}

/// Emits [`PROGRESS_EVENT`] and console logs for one update flow; wraps
/// [`ProgressStats`] with the app handle. Create one per download.
pub struct ProgressTracker {
    app: AppHandle,
    state: Arc<Mutex<Option<UpdateProgress>>>,
    version: String,
    stats: ProgressStats,
    last_percent: Option<u8>,
    last_report: Instant,
    interval: Duration,
}

impl ProgressTracker {
    pub fn new(app: AppHandle, state: Arc<Mutex<Option<UpdateProgress>>>, version: impl Into<String>) -> Self {
        Self {
            app,
            state,
            version: version.into(),
            stats: ProgressStats::new(),
            last_percent: None,
            last_report: Instant::now(),
            interval: REPORT_INTERVAL,
        }
    }

    /// Called by the updater plugin for every downloaded chunk.
    pub fn on_chunk(&mut self, chunk_len: usize, total: Option<u64>) {
        self.stats.record_chunk(chunk_len, total);
        self.publish(ProgressPhase::Downloading);
    }

    /// Called once when the download stream ends (before signature check).
    pub fn on_download_finish(&mut self) {
        self.publish(ProgressPhase::Downloaded);
    }

    /// Called when the download fails.
    pub fn on_failed(&mut self) {
        self.publish(ProgressPhase::Failed);
    }

    /// Update the shared snapshot on every call; log + emit, throttled.
    fn publish(&mut self, phase: ProgressPhase) {
        if let Ok(mut guard) = self.state.lock() {
            *guard = Some(UpdateProgress {
                downloaded: self.stats.downloaded,
                total: self.stats.total,
                percent: self.stats.percent,
                phase,
            });
        }
        let now = Instant::now();
        let force = phase != ProgressPhase::Downloading;
        let report = force
            || should_report(
                self.last_percent,
                self.stats.percent,
                now.duration_since(self.last_report),
                self.interval,
            );
        if !report {
            return;
        }
        self.last_percent = self.stats.percent;
        self.last_report = now;
        match phase {
            ProgressPhase::Downloading => match (self.stats.downloaded, self.stats.total, self.stats.percent) {
                (downloaded, Some(total), Some(percent)) => {
                    eprintln!("[dsh-desktop] update download: {downloaded} / {total} bytes ({percent}%)");
                }
                (downloaded, None, _) => {
                    eprintln!("[dsh-desktop] update download: {downloaded} bytes (total unknown)");
                }
                _ => {}
            },
            ProgressPhase::Downloaded => {
                eprintln!("[dsh-desktop] update downloaded ({} bytes)", self.stats.downloaded);
            }
            ProgressPhase::Failed => {
                eprintln!("[dsh-desktop] update download failed");
            }
            _ => {}
        }
        let _ = self.app.emit(PROGRESS_EVENT, progress_payload(&self.stats, phase, &self.version));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_clamps_and_guards_zero_total() {
        assert_eq!(percent_of(0, 100), 0);
        assert_eq!(percent_of(50, 100), 50);
        assert_eq!(percent_of(100, 100), 100);
        assert_eq!(percent_of(150, 100), 100); // clamped
        assert_eq!(percent_of(0, 0), 0); // no division panic
    }

    #[test]
    fn stats_accumulate_chunks_and_keep_known_total() {
        let mut stats = ProgressStats::new();
        stats.record_chunk(10, None);
        assert_eq!(stats.total, None);
        stats.record_chunk(20, Some(100));
        stats.record_chunk(30, None); // total stays known once seen
        assert_eq!(stats.downloaded, 60);
        assert_eq!(stats.total, Some(100));
        assert_eq!(stats.percent, Some(60));
    }

    #[test]
    fn throttle_reports_on_percent_jump_or_interval() {
        let interval = Duration::from_secs(1);
        // percent jump → report even at zero elapsed
        assert!(should_report(Some(0), Some(1), Duration::ZERO, interval));
        // same percent, no time elapsed → no report
        assert!(!should_report(Some(1), Some(1), Duration::from_millis(100), interval));
        // same percent, interval elapsed → report
        assert!(should_report(Some(1), Some(1), Duration::from_secs(2), interval));
        // unknown percent → report only on interval
        assert!(!should_report(None, None, Duration::from_millis(100), interval));
        assert!(should_report(None, None, Duration::from_secs(2), interval));
    }

    #[test]
    fn payload_shape() {
        let stats = ProgressStats {
            downloaded: 42,
            total: Some(100),
            percent: Some(42),
        };
        let payload = progress_payload(&stats, ProgressPhase::Downloading, "1.2.3");
        assert_eq!(payload["version"], "1.2.3");
        assert_eq!(payload["downloaded"], 42);
        assert_eq!(payload["total"], 100);
        assert_eq!(payload["percent"], 42);
        assert_eq!(payload["phase"], "downloading");
        assert_eq!(payload["done"], false);
        let done = progress_payload(&stats, ProgressPhase::Downloaded, "1.2.3");
        assert_eq!(done["done"], true);
    }

    #[test]
    fn simulated_download_flow_reaches_terminal_phases() {
        // Simulate the updater plugin's callback sequence (see
        // tauri-plugin-updater-2.10.1/src/updater.rs:705-710) without an app
        // handle: record chunks, then finish.
        let mut stats = ProgressStats::new();
        for chunk in [16usize, 32, 16, 36] {
            stats.record_chunk(chunk, Some(100));
        }
        assert_eq!(stats.downloaded, 100);
        assert_eq!(stats.percent, Some(100));
        let done = progress_payload(&stats, ProgressPhase::Downloaded, "1.2.3");
        assert_eq!(done["phase"], "downloaded");
        assert_eq!(done["done"], true);
        let failed = progress_payload(&stats, ProgressPhase::Failed, "1.2.3");
        assert_eq!(failed["phase"], "failed");
        assert_eq!(failed["done"], false);
    }
}
