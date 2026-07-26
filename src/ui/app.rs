//! Application state and input handling for the TUI.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::ui::{LogLine, Shared};

/// Number of log lines kept in the local display history.
const LOG_HISTORY: usize = 1000;

/// Step applied to the SNR threshold per Up/Down press, in dB.
const THRESHOLD_STEP: f32 = 1.0;

/// Step applied to the SNR timeout per Up/Down press, in seconds.
const TIMEOUT_STEP: f32 = 0.5;

/// Tabs available in the interface.
pub const TAB_TITLES: [&str; 2] = ["Main", "Logs"];

/// Focusable control on the main tab.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Threshold,
    Bypass,
    Timeout,
    DFN,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Focus::Threshold => Focus::Bypass,
            Focus::Bypass => Focus::Timeout,
            Focus::Timeout => Focus::DFN,
            Focus::DFN => Focus::Threshold,
        }
    }

    fn previous(self) -> Self {
        match self {
            Focus::Threshold => Focus::DFN,
            Focus::Bypass => Focus::Threshold,
            Focus::Timeout => Focus::Bypass,
            Focus::DFN => Focus::Timeout,
        }
    }
}

pub struct App {
    pub shared: Arc<Shared>,
    pub tab_index: usize,
    pub focus: Focus,
    pub should_quit: bool,
    /// Local, render-only copy of recent log lines.
    pub logs: VecDeque<LogLine>,
}

impl App {
    pub fn new(shared: Arc<Shared>) -> Self {
        Self {
            shared,
            tab_index: 0,
            focus: Focus::Threshold,
            should_quit: false,
            logs: VecDeque::with_capacity(LOG_HISTORY),
        }
    }

    /// Switch to the next tab (Tab key).
    pub fn on_tab(&mut self) {
        self.tab_index = (self.tab_index + 1) % TAB_TITLES.len();
    }

    /// Move focus to the previous control (Left key).
    pub fn on_left(&mut self) {
        self.focus = self.focus.previous();
    }

    /// Move focus to the next control (Right key).
    pub fn on_right(&mut self) {
        self.focus = self.focus.next();
    }

    /// Increase the focused control's value (Up key).
    pub fn on_up(&mut self) {
        match self.focus {
            Focus::Threshold => {
                self.shared
                    .set_snr_threshold(self.shared.snr_threshold() + THRESHOLD_STEP);
            }
            Focus::Timeout => {
                self.shared
                    .set_snr_timeout(self.shared.snr_timeout() + TIMEOUT_STEP);
            }
            Focus::Bypass => self.shared.set_bypass(!self.shared.bypass()),
            Focus::DFN => {
                self.shared.set_deepfilternet_enabled(!self.shared.deepfilternet_enabled())
            },
        }
    }

    /// Decrease the focused control's value (Down key).
    pub fn on_down(&mut self) {
        match self.focus {
            Focus::Threshold => {
                self.shared
                    .set_snr_threshold(self.shared.snr_threshold() - THRESHOLD_STEP);
            }
            Focus::Timeout => {
                self.shared
                    .set_snr_timeout(self.shared.snr_timeout() - TIMEOUT_STEP);
            }
            Focus::Bypass => self.shared.set_bypass(!self.shared.bypass()),
            Focus::DFN => {
                self.shared.set_deepfilternet_enabled(!self.shared.deepfilternet_enabled())
            },
        }
    }

    /// Toggle the bypass checkbox (Space/Enter) when it is focused.
    pub fn on_toggle(&mut self) {
        if self.focus == Focus::Bypass {
            self.shared.set_bypass(!self.shared.bypass());
        } else if self.focus == Focus::DFN {
            self.shared.set_deepfilternet_enabled(!self.shared.deepfilternet_enabled());
        }
    }

    /// Request application exit (Ctrl-C / `q`).
    pub fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Drain freshly captured log lines into the local display history.
    pub fn on_tick(&mut self) {
        for line in self.shared.drain_logs() {
            if self.logs.len() == LOG_HISTORY {
                self.logs.pop_front();
            }
            self.logs.push_back(line);
        }
    }
}
