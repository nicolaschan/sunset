//! ratatui App: terminal setup, input pump, render loop.
//!
//! Lives entirely on the local task-set; no Send bounds. The input
//! pump is `crossterm::event::EventStream`, which uses
//! `tokio::task::spawn_blocking` internally.

pub mod input;
pub mod render;

use std::io::{Stdout, stdout};
use std::rc::Rc;
use std::time::Duration;

use crossterm::ExecutableCommand;
use crossterm::event::{Event, EventStream, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use crate::client::Client;
use crate::dispatch::{DispatchOutcome, dispatch};
use crate::ui::input::{Composer, KeyOutcome, handle_key};
use crate::ui::render::{ComposerState, draw};

pub async fn run_app(client: Rc<Client>) -> std::io::Result<()> {
    enable_raw_mode()?;
    let mut out = stdout();
    out.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(out);
    let mut terminal = Terminal::new(backend)?;

    let result = drive(&mut terminal, &client).await;

    disable_raw_mode()?;
    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    result
}

async fn drive(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &Rc<Client>,
) -> std::io::Result<()> {
    let mut composer = Composer::default();
    let mut events = EventStream::new();
    repaint(terminal, client, &composer)?;

    loop {
        tokio::select! {
            _ = client.notify.notified() => {
                repaint(terminal, client, &composer)?;
            }
            ev = events.next() => {
                let Some(Ok(ev)) = ev else { break };
                if let Event::Key(key) = ev {
                    if key.kind == KeyEventKind::Release { continue; }
                    match handle_key(&mut composer, key) {
                        KeyOutcome::Submit(cmd) => {
                            match dispatch(client, cmd).await {
                                DispatchOutcome::Quit => break,
                                DispatchOutcome::Continue => {}
                            }
                            repaint(terminal, client, &composer)?;
                        }
                        KeyOutcome::Quit => break,
                        KeyOutcome::Redraw => repaint(terminal, client, &composer)?,
                        KeyOutcome::Nothing => {}
                    }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                repaint(terminal, client, &composer)?;
            }
        }
    }
    Ok(())
}

fn repaint(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    client: &Rc<Client>,
    composer: &Composer,
) -> std::io::Result<()> {
    let top = client.snapshot_top();
    let active = top.active_room.clone();
    let room = active.as_deref().and_then(|n| client.snapshot_room(n));
    terminal.draw(|f| {
        let composer_state = ComposerState {
            buffer: &composer.buffer,
            cursor: composer.cursor_col(),
        };
        draw(f, &top, room.as_ref(), &composer_state);
    })?;
    Ok(())
}
