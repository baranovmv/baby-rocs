//! Terminal user interface (ratatui + crossterm) for baby_rocs.
//!
//! [`Shared`] holds the runtime state exchanged between the audio worker
//! thread and the UI. All fields are lock-free: numeric values live in
//! atomics and log lines flow through a bounded [`ArrayQueue`] ring buffer.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crossbeam_queue::ArrayQueue;

#[cfg(feature = "tui")]
pub mod app;
#[cfg(feature = "tui")]
pub mod crossterm;
#[cfg(feature = "tui")]
pub mod ui;

/// Range of the SNR gauge / threshold control, in dB.
pub const SNR_MIN: f32 = 0.0;
pub const SNR_MAX: f32 = 40.0;

/// Range of the input-level gauge, in dBFS (full scale = 0).
pub const LEVEL_MIN_DBFS: f32 = -60.0;
pub const LEVEL_MAX_DBFS: f32 = 0.0;

/// Maximum number of log lines retained in the ring buffer.
const LOG_CAPACITY: usize = 4096;

/// A single captured roc-toolkit log line.
#[derive(Clone)]
pub struct LogLine {
    pub level: roc::log::Level,
    pub text: String,
}

/// Lock-free state shared between the audio worker thread and the UI.
pub struct Shared {
    /// Most recent SNR reported by the processor (f32 bits).
    current_snr: AtomicU32,
    /// Most recent input level in dBFS reported by the processor (f32 bits).
    current_level: AtomicU32,
    /// SNR threshold above which audio is streamed (f32 bits).
    snr_threshold: AtomicU32,
    /// Silence timeout before streaming stops, in seconds (f32 bits).
    snr_timeout: AtomicU32,
    /// When set, the threshold is bypassed and audio is always streamed.
    bypass: AtomicBool,
    /// Audio is currently being sent
    sending: AtomicBool,
    /// Enable DeepFilterNet
    deepfilternet_enabled: AtomicBool,
    /// Ring buffer of captured log lines.
    logs: ArrayQueue<LogLine>,

}

impl Shared {
    /// Create shared state seeded from the configured processing options.
    pub fn new(bypass: bool, snr_threshold: f32, snr_timeout: f32, deepfilternet_enabled: bool) -> Self {
        Self {
            current_snr: AtomicU32::new(SNR_MIN.to_bits()),
            current_level: AtomicU32::new(LEVEL_MIN_DBFS.to_bits()),
            snr_threshold: AtomicU32::new(snr_threshold.to_bits()),
            snr_timeout: AtomicU32::new(snr_timeout.to_bits()),
            bypass: AtomicBool::new(bypass),
            sending: AtomicBool::new(false),
            deepfilternet_enabled: AtomicBool::new(deepfilternet_enabled),
            logs: ArrayQueue::new(LOG_CAPACITY),
        }
    }

    pub fn current_snr(&self) -> f32 {
        f32::from_bits(self.current_snr.load(Ordering::Relaxed))
    }

    pub fn set_current_snr(&self, value: f32) {
        self.current_snr.store(value.to_bits(), Ordering::Relaxed);
    }

    pub fn current_level(&self) -> f32 {
        f32::from_bits(self.current_level.load(Ordering::Relaxed))
    }

    pub fn set_current_level(&self, value: f32) {
        self.current_level.store(value.to_bits(), Ordering::Relaxed);
    }

    pub fn snr_threshold(&self) -> f32 {
        f32::from_bits(self.snr_threshold.load(Ordering::Relaxed))
    }

    pub fn set_snr_threshold(&self, value: f32) {
        self.snr_threshold
            .store(value.clamp(SNR_MIN, SNR_MAX).to_bits(), Ordering::Relaxed);
    }

    pub fn snr_timeout(&self) -> f32 {
        f32::from_bits(self.snr_timeout.load(Ordering::Relaxed))
    }

    pub fn set_snr_timeout(&self, value: f32) {
        self.snr_timeout
            .store(value.max(0.0).to_bits(), Ordering::Relaxed);
    }

    pub fn bypass(&self) -> bool {
        self.bypass.load(Ordering::Relaxed)
    }

    pub fn set_bypass(&self, value: bool) {
        self.bypass.store(value, Ordering::Relaxed);
    }

    pub fn sending(&self) -> bool {
        self.sending.load(Ordering::Relaxed)
    }

    pub fn set_sending(&self, value: bool) {
        self.sending.store(value, Ordering::Relaxed);
    }

    pub fn deepfilternet_enabled(&self) -> bool {
        self.deepfilternet_enabled.load(Ordering::Relaxed)
    }

    pub fn set_deepfilternet_enabled(&self, value: bool) {
        self.deepfilternet_enabled.store(value, Ordering::Relaxed);
    }

    /// push a log line, overwriting the oldest entry when the ring is full.
    pub fn push_log(&self, level: roc::log::Level, text: impl Into<String>) {
        #[cfg(feature = "tui")]
        self.logs.force_push(LogLine { level, text: text.into() });
    }

    /// Drain all pending log lines from the ring buffer.
    pub fn drain_logs(&self) -> impl Iterator<Item = LogLine> + '_ {
        std::iter::from_fn(move || self.logs.pop())
    }
}
