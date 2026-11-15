mod abort_on_drop_handle;
mod capture;
mod debug_or_test;
mod event_loop;
mod height;
mod help;
mod interval_arg;
mod mark;
mod notify_pair;
mod render;
mod search;
mod size;
mod terminal;
mod terminal_bg;
mod terminal_guard;
mod theme;
mod wrapped_row;

use std::io;
use std::panic;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use clap::Parser;
use crossterm::event::Event;
use tokio::sync::mpsc;
use tokio::sync::mpsc::Sender;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::abort_on_drop_handle::AbortOnDropHandle;
use crate::capture::capture_command_output;
use crate::event_loop::EventLoop;
use crate::interval_arg::IntervalArg;
use crate::notify_pair::NotifyRx;
use crate::notify_pair::NotifyTx;
use crate::search::history::store::History;
use crate::terminal::line::Line;
use crate::terminal::parse::parse_command_output;
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let history = History::load_from_home().await;
    install_panic_hook();
    // Query before raw mode; the color query reads a terminal response.
    let terminal_bg = query_terminal_bg();
    let _guard = TerminalGuard::enter_alt_mode()?;

    // One reader thread: crossterm events must not be polled concurrently.
    // `poll` with a timeout lets the thread exit after `stop` is set.
    let stop = Arc::new(AtomicBool::new(false));
    let _stop_on_drop = StopOnDrop(Arc::clone(&stop));
    let (_key_reader, keys) = spawn_key_reader(stop);

    let (output_tx, output_rx) = mpsc::channel(1);
    let (refresh_tx, refresh_rx) = NotifyTx::pair();
    let capture_task = AbortOnDropHandle::spawn(capture_command_output_loop(
        args.command,
        args.interval.as_duration(),
        output_tx,
        refresh_rx,
    ));

    EventLoop::new(terminal_bg, history)
        .run(output_rx, keys, refresh_tx, capture_task)
        .await
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
