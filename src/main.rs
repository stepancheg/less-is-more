mod abort_on_drop_handle;
mod capture;
mod debug_or_test;
mod height;
mod help;
mod interval_arg;
mod notify_pair;
mod output;
mod render;
mod size;
mod terminal_bg;
mod terminal_guard;

use std::io;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use clap::Parser;
use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::abort_on_drop_handle::AbortOnDropHandle;
use crate::capture::capture_command_output;
use crate::help::help_lines;
use crate::interval_arg::IntervalArg;
use crate::notify_pair::NotifyRx;
use crate::notify_pair::NotifyTx;
use crate::output::line::Line;
use crate::output::parse::parse_command_output;
use crate::render::Renderer;
use crate::render::terminal_size;
use crate::terminal_bg::query_terminal_bg;
use crate::terminal_guard::TerminalGuard;
use crate::terminal_guard::restore_terminal;

#[derive(clap::Parser, Debug)]
struct Args {
    /// Seconds between command runs.
    #[clap(short = 'n', long, default_value = "2")]
    interval: IntervalArg,

    /// Command to run.
    #[clap(trailing_var_arg = true, required = true, allow_hyphen_values = true)]
    command: Vec<String>,
}

fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original(info);
    }));
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    run(args)?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn run(args: Args) -> anyhow::Result<()> {
    install_panic_hook();
    // Query before raw mode; the color query reads a terminal response.
    let terminal_bg = query_terminal_bg();
    let _guard = TerminalGuard::enter_alt_mode()?;

    // One reader thread: crossterm events must not be polled concurrently.
    // `poll` with a timeout lets the thread exit after `stop` is set.
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_on_drop = StopOnDrop(Arc::clone(&stop));
    let (_key_reader, mut keys) = spawn_key_reader(stop);

    let (output_tx, mut output_rx) = mpsc::channel(1);
    let (refresh_tx, refresh_rx) = NotifyTx::pair();
    let capture_task = AbortOnDropHandle::spawn(capture_command_output_loop(
        args.command,
        args.interval.as_duration(),
        output_tx,
        refresh_rx,
    ));

    let mut renderer = Renderer::new(terminal_bg);
    let mut help_renderer = Renderer::new(terminal_bg);
    let mut showing_help = false;
    loop {
        tokio::select! {
            msg = output_rx.recv() => {
                let Some(lines) = msg else {
                    // Capture loop exited (receiver dropped or task ended).
                    capture_task.await?;
                    return Ok(());
                };
                let size = terminal_size()?;
                renderer.set_lines(lines, size);
                if !showing_help {
                    renderer.draw(&mut io::stdout(), size)?;
                }
            }
            action = recv_key_action(&mut keys) => {
                match action? {
                    KeyAction::Quit => {
                        if showing_help {
                            showing_help = false;
                            let size = terminal_size()?;
                            renderer.redraw(&mut io::stdout(), size)?;
                        } else {
                            // `capture_task` and `_key_reader` abort on drop.
                            return Ok(());
                        }
                    }
                    KeyAction::Help => {
                        let size = terminal_size()?;
                        if showing_help {
                            showing_help = false;
                            renderer.redraw(&mut io::stdout(), size)?;
                        } else {
                            showing_help = true;
                            help_renderer.set_lines(help_lines(), size);
                            help_renderer.scroll_top();
                            help_renderer.redraw(&mut io::stdout(), size)?;
                        }
                    }
                    KeyAction::Refresh => {
                        // Wakes the interval wait. A permit is stored if a
                        // capture is in progress, so the next wait is skipped.
                        refresh_tx.notify_one();
                    }
                    action @ (KeyAction::ScrollDown
                    | KeyAction::ScrollUp
                    | KeyAction::Redraw
                    | KeyAction::ScrollTop
                    | KeyAction::ScrollBottom
                    | KeyAction::ScrollPageDown
                    | KeyAction::ScrollPageUp) => {
                        let view = if showing_help {
                            &mut help_renderer
                        } else {
                            &mut renderer
                        };
                        apply_view_action(view, action)?;
                    }
                }
            }
        }
    }
}

/// Repeatedly capture command output, parse it, and send each snapshot on `tx`.
/// A capture error is parsed as an `error: ...` line and sent the same way.
/// `refresh` ends the interval wait so the next capture starts immediately.
/// Stops when `tx` is closed.
async fn capture_command_output_loop(
    command: Vec<String>,
    interval: Duration,
    tx: Sender<Vec<Line>>,
    refresh: NotifyRx,
) {
    loop {
        let lines = match capture_command_output(&command).await {
            Ok(bytes) => parse_command_output(&bytes),
            Err(err) => {
                let text = format!("error: {err:?}");
                parse_command_output(text.as_bytes())
            }
        };
        if tx.send(lines).await.is_err() {
            return;
        }
        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = refresh.notified() => {}
        }
    }
}

fn apply_view_action(view: &mut Renderer, action: KeyAction) -> io::Result<()> {
    let size = terminal_size()?;
    match action {
        KeyAction::ScrollDown => {
            view.scroll_down(size);
            view.draw(&mut io::stdout(), size)?;
        }
        KeyAction::ScrollUp => {
            view.scroll_up();
            view.draw(&mut io::stdout(), size)?;
        }
        KeyAction::Redraw => {
            view.redraw(&mut io::stdout(), size)?;
        }
        KeyAction::ScrollTop => {
            view.scroll_top();
            view.draw(&mut io::stdout(), size)?;
        }
        KeyAction::ScrollBottom => {
            view.scroll_bottom(size);
            view.draw(&mut io::stdout(), size)?;
        }
        KeyAction::ScrollPageDown => {
            view.scroll_page_down(size);
            view.draw(&mut io::stdout(), size)?;
        }
        KeyAction::ScrollPageUp => {
            view.scroll_page_up(size);
            view.draw(&mut io::stdout(), size)?;
        }
        KeyAction::Quit | KeyAction::Help | KeyAction::Refresh => {}
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum KeyAction {
    Quit,
    Help,
    Refresh,
    ScrollDown,
    ScrollUp,
    ScrollTop,
    ScrollBottom,
    ScrollPageDown,
    ScrollPageUp,
    Redraw,
}

fn key_action(event: &Event) -> Option<KeyAction> {
    let Event::Key(key) = event else {
        return None;
    };
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(
            key.code,
            KeyCode::Char('l') | KeyCode::Char('L') | KeyCode::Char('\u{c}')
        )
    {
        return Some(KeyAction::Redraw);
    }
    match key.code {
        KeyCode::Char('q') => Some(KeyAction::Quit),
        KeyCode::Char('h') => Some(KeyAction::Help),
        KeyCode::Char('r') => Some(KeyAction::Refresh),
        KeyCode::Char('j') | KeyCode::Down => Some(KeyAction::ScrollDown),
        KeyCode::Char('k') | KeyCode::Up => Some(KeyAction::ScrollUp),
        KeyCode::Char('g') => Some(KeyAction::ScrollTop),
        KeyCode::Char('G') => Some(KeyAction::ScrollBottom),
        KeyCode::Char(' ') | KeyCode::PageDown => Some(KeyAction::ScrollPageDown),
        KeyCode::PageUp => Some(KeyAction::ScrollPageUp),
        _ => None,
    }
}

/// Receive key events until one maps to a key action, or the reader ends.
/// Other events are discarded. The caller should race this against other
/// work and drop it to stop waiting.
async fn recv_key_action(keys: &mut UnboundedReceiver<io::Result<Event>>) -> io::Result<KeyAction> {
    loop {
        match keys.recv().await {
            Some(Ok(event)) => {
                if let Some(action) = key_action(&event) {
                    return Ok(action);
                }
            }
            Some(Err(err)) => return Err(err),
            None => return Err(io::Error::other("key reader stopped")),
        }
    }
}

fn spawn_key_reader(
    stop: Arc<AtomicBool>,
) -> (AbortOnDropHandle<()>, UnboundedReceiver<io::Result<Event>>) {
    let (tx, rx) = mpsc::unbounded_channel();
    let handle = AbortOnDropHandle::spawn_blocking(move || {
        while !stop.load(Ordering::SeqCst) {
            match crossterm::event::poll(Duration::from_millis(50)) {
                Ok(false) => {}
                Ok(true) => match crossterm::event::read() {
                    Ok(event) => {
                        if tx.send(Ok(event)).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(Err(err));
                        break;
                    }
                },
                Err(err) => {
                    let _ = tx.send(Err(err));
                    break;
                }
            }
        }
    });
    (handle, rx)
}

/// Sets the key-reader stop flag when the watch loop exits.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::Event;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyEventState;
    use crossterm::event::KeyModifiers;

    use crate::KeyAction;
    use crate::key_action;

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        })
    }

    #[test]
    fn test_h_is_help() {
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('h'))),
                Some(KeyAction::Help)
            )
        );
    }

    #[test]
    fn test_q_is_quit() {
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('q'))),
                Some(KeyAction::Quit)
            )
        );
    }
}
