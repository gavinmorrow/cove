mod config;
mod never;
mod replies;
mod room;
mod ui;

use std::io;

use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
use tui::backend::CrosstermBackend;
use tui::Terminal;
use ui::Ui;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;

    crossterm::terminal::enable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        EnterAlternateScreen,
        EnableMouseCapture
    )?;

    // Defer error handling so the terminal always gets restored properly
    let result = Ui::run(&mut terminal).await;

    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    crossterm::terminal::disable_raw_mode()?;

    result?;

    Ok(())
}
