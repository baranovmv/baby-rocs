//! Widget rendering for the TUI.

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Bar, BarChart, BarGroup, Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation,
    ScrollbarState, Tabs, Wrap,
};
use ratatui::Frame;

use crate::ui::app::{App, Focus, TAB_TITLES};
use crate::ui::{LEVEL_MAX_DBFS, LEVEL_MIN_DBFS, SNR_MAX, SNR_MIN};

const FOCUS_COLOR: Color = Color::Yellow;
const NORMAL_COLOR: Color = Color::White;

/// Draw the whole interface.
pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let tabs = Tabs::new(TAB_TITLES.iter().map(|t| Line::from(*t)))
        .block(Block::bordered().title("baby_rocs"))
        .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))
        .select(app.tab_index);
    frame.render_widget(tabs, chunks[0]);

    match app.tab_index {
        0 => draw_main_tab(frame, app, chunks[1]),
        1 => draw_logs_tab(frame, app, chunks[1]),
        _ => {}
    }

    let help = Paragraph::new(Line::from(vec![
        Span::styled("Tab", Style::default().fg(Color::Cyan)),
        Span::raw(" switch  "),
        Span::styled("←/→", Style::default().fg(Color::Cyan)),
        Span::raw(" focus  "),
        Span::styled("↑/↓", Style::default().fg(Color::Cyan)),
        Span::raw(" adjust  "),
        Span::styled("Space", Style::default().fg(Color::Cyan)),
        Span::raw(" toggle  "),
        Span::styled("Ctrl-C/q", Style::default().fg(Color::Cyan)),
        Span::raw(" quit"),
    ]));
    frame.render_widget(help, chunks[2]);
}

fn border_style(app: &App, field: Focus) -> Style {
    if app.focus == field {
        Style::default().fg(FOCUS_COLOR).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(NORMAL_COLOR)
    }
}

fn draw_main_tab(frame: &mut Frame, app: &App, area: Rect) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(16),
            Constraint::Length(16),
            Constraint::Length(18),
            Constraint::Min(20),
        ])
        .split(area);

    draw_level_gauge(frame, app, cols[0]);
    draw_snr_gauge(frame, app, cols[1]);
    draw_threshold(frame, app, cols[2]);
    draw_controls(frame, app, cols[3]);
}

fn draw_level_gauge(frame: &mut Frame, app: &App, area: Rect) {
    let dbfs = app.shared.current_level();
    let span = (LEVEL_MAX_DBFS - LEVEL_MIN_DBFS).max(1.0);
    let value = (dbfs - LEVEL_MIN_DBFS).clamp(0.0, span).round() as u64;
    let bar = Bar::default()
        .value(value)
        .text_value(format!("{dbfs:.1}"))
        .style(Style::default().fg(Color::Cyan));
    let chart = BarChart::default()
        .block(Block::bordered().title(format!("Level {dbfs:.1} dBFS")))
        .data(BarGroup::default().bars(&[bar]))
        .max(span as u64)
        .bar_width(8)
        .bar_style(Style::default().fg(Color::Cyan))
        .value_style(Style::default().fg(Color::Black).bg(Color::Cyan));
    frame.render_widget(chart, area);
}

fn draw_snr_gauge(frame: &mut Frame, app: &App, area: Rect) {
    let snr = app.shared.current_snr();
    let value = snr.clamp(SNR_MIN, SNR_MAX).round() as u64;
    let bar = Bar::default()
        .value(value)
        .text_value(format!("{snr:.1}"))
        .style(Style::default().fg(Color::Green));
    let chart = BarChart::default()
        .block(Block::bordered().title(format!("SNR {snr:.1} dB")))
        .data(BarGroup::default().bars(&[bar]))
        .max(SNR_MAX as u64)
        .bar_width(8)
        .bar_style(Style::default().fg(Color::Green))
        .value_style(Style::default().fg(Color::Black).bg(Color::Green));
    frame.render_widget(chart, area);
}

fn draw_threshold(frame: &mut Frame, app: &App, area: Rect) {
    let threshold = app.shared.snr_threshold();
    let block = Block::bordered()
        .title("Threshold")
        .border_style(border_style(app, Focus::Threshold));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);

    // Scrollbar: top = SNR_MAX, bottom = SNR_MIN.
    let span = (SNR_MAX - SNR_MIN).max(1.0);
    let position = (SNR_MAX - threshold).clamp(0.0, span).round() as usize;
    let mut state = ScrollbarState::new(span as usize).position(position);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalLeft)
        .begin_symbol(Some("▲"))
        .end_symbol(Some("▼"))
        .thumb_style(Style::default().fg(FOCUS_COLOR));
    frame.render_stateful_widget(scrollbar, rows[0], &mut state);

    let label = Paragraph::new(Line::from(format!("{threshold:.0} dB")))
        .style(Style::default().fg(Color::Cyan))
        .alignment(ratatui::layout::Alignment::Center);
    frame.render_widget(label, rows[1]);
}

fn draw_controls(frame: &mut Frame, app: &App, area: Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3),  Constraint::Length(3),  Constraint::Length(3),
            Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    // Bypass checkbox.
    let mark = if app.shared.bypass() { "[x]" } else { "[ ]" };
    let bypass = Paragraph::new(Line::from(format!("{mark} Bypass threshold"))).block(
        Block::bordered()
            .title("Bypass")
            .border_style(border_style(app, Focus::Bypass)),
    );
    frame.render_widget(bypass, rows[0]);

    // Timeout edit box.
    let timeout = app.shared.snr_timeout();
    let timeout_box = Paragraph::new(Line::from(format!("{timeout:.1} s  (±0.5)"))).block(
        Block::bordered()
            .title("Silence timeout")
            .border_style(border_style(app, Focus::Timeout)),
    );
    frame.render_widget(timeout_box, rows[1]);

    // Enable/Disable DeepFilerNet
    let dfn_enable = if app.shared.deepfilternet_enabled() { "[x]" } else { "[ ]" };
    let dfn_box = Paragraph::new(Line::from(format!("{dfn_enable} DeepFilterNet"))).block(
        Block::bordered()
            .title("DeepFilterNet status")
            .border_style(border_style(app, Focus::DFN)),
    );
    frame.render_widget(dfn_box, rows[2]);
 
    // Currently sending
    let sending = if app.shared.sending() { "[x]" } else { "[ ]" };
    let sending_box = Paragraph::new(Line::from(format!("{sending} sending"))).block(
        Block::bordered()
            .title("Sending status")
            .border_style(Style::default().fg(NORMAL_COLOR)),
    );
    frame.render_widget(sending_box, rows[3]);

}

fn draw_logs_tab(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title("roc-toolkit logs");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines: Vec<Line> = app
        .logs
        .iter()
        .map(|entry| {
            let color = level_color(entry.level);
            Line::from(vec![
                Span::styled(format!("{:<6} ", entry.level), Style::default().fg(color)),
                Span::raw(entry.text.clone()),
            ])
        })
        .collect();

    // Auto-scroll so the newest lines are visible.
    let height = inner.height as usize;
    let offset = lines.len().saturating_sub(height) as u16;

    let paragraph = Paragraph::new(Text::from(lines))
        .wrap(Wrap { trim: false })
        .scroll((offset, 0));
    frame.render_widget(paragraph, inner);
}

fn level_color(level: roc::log::Level) -> Color {
    match level {
        roc::log::Level::Error => Color::Red,
        roc::log::Level::Info => Color::Cyan,
        roc::log::Level::Note => Color::Yellow,
        roc::log::Level::Debug => Color::Gray,
        roc::log::Level::Trace => Color::DarkGray,
        roc::log::Level::None => Color::White,
    }
}
