//! Crossterm backend: terminal setup, event loop and key handling.

use std::io::{self, Stdout};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Error;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;

use crate::ui::app::App;
use crate::ui::{ui, Shared};

/// Interval between ticks (log drain + redraw when idle).
const TICK_RATE: Duration = Duration::from_millis(100);

/// Set up the terminal, run the UI loop and always restore the terminal.
///
/// `running` is the global run flag: the loop exits when the user quits, and
/// clears the flag on exit so the audio pipeline shuts down.
pub fn run(shared: Arc<Shared>, running: Arc<AtomicBool>) -> Result<(), Error> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_app(&mut terminal, App::new(shared), &running);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // The UI is the front-end; leaving it always stops the pipeline.
    running.store(false, Ordering::SeqCst);

    result
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    mut app: App,
    running: &AtomicBool,
) -> Result<(), Error> {
    let mut last_tick = Instant::now();
    loop {
        app.on_tick();
        terminal.draw(|frame| ui::render(frame, &app))?;

        let timeout = TICK_RATE.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    handle_key(&mut app, key.code, key.modifiers);
                }
            }
        }
        if last_tick.elapsed() >= TICK_RATE {
            last_tick = Instant::now();
        }

        if app.should_quit || !running.load(Ordering::SeqCst) {
            return Ok(());
        }
    }
}

fn handle_key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
        app.quit();
        return;
    }
    match code {
        KeyCode::Char('q') => app.quit(),
        KeyCode::Tab => app.on_tab(),
        KeyCode::Left => app.on_left(),
        KeyCode::Right => app.on_right(),
        KeyCode::Up => app.on_up(),
        KeyCode::Down => app.on_down(),
        KeyCode::Char(' ') | KeyCode::Enter => app.on_toggle(),
        _ => {}
    }
}
