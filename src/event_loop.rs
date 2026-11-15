use std::io;
use std::mem;

use crossterm::event::Event;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyEventKind;
use crossterm::event::KeyModifiers;
use tokio::sync::mpsc::Receiver;
use tokio::sync::mpsc::UnboundedReceiver;

use crate::abort_on_drop_handle::AbortOnDropHandle;
use crate::help::help_lines;
use crate::mark::MarkOp;
use crate::mark::Marks;
use crate::mark::NOT_SET;
use crate::notify_pair::NotifyTx;
use crate::render::Renderer;
use crate::render::terminal_size;
use crate::search::find::SearchOutcome;
use crate::search::find::search_next;
use crate::search::find::submit_prompt;
use crate::search::history::store::History;
use crate::search::prompt::Prompt;
use crate::search::prompt::PromptOutcome;
use crate::search::state::NO_PREVIOUS_SEARCH;
use crate::search::state::PATTERN_NOT_FOUND;
use crate::search::state::Search;
use crate::search::state::status_after_output;
use crate::size::Size;
use crate::terminal::line::Line;
use crate::terminal_bg::TerminalBg;

enum KeyResult {
    Continue,
    Quit,
    Refresh,
}

enum Mode {
    Output,
    Help,
    Prompt(Prompt),
    /// Waiting for the option letter after `-`.
    Dash,
    /// Waiting for the mark name after `m` or `'`.
    Mark(MarkOp),
}

/// On-screen state for output, help, and the search prompt.
pub(crate) struct EventLoop {
    renderer: Renderer,
    help_renderer: Renderer,
    search: Search,
    history: History,
    marks: Marks,
    mode: Mode,
}

impl EventLoop {
    pub(crate) fn new(terminal_bg: TerminalBg, history: History) -> EventLoop {
        EventLoop {
            renderer: Renderer::new(terminal_bg),
            help_renderer: Renderer::new(terminal_bg),
            search: Search::new(),
            history,
            marks: Marks::default(),
            mode: Mode::Output,
        }
    }

    pub(crate) async fn run(
        &mut self,
        output_rx: Receiver<Vec<Line>>,
        keys: UnboundedReceiver<io::Result<Event>>,
        refresh_tx: NotifyTx,
        capture_task: AbortOnDropHandle<()>,
    ) -> anyhow::Result<()> {
        let result = self.drive(output_rx, keys, refresh_tx, capture_task).await;
        self.history.shutdown().await;
        result
    }

    async fn drive(
        &mut self,
        mut output_rx: Receiver<Vec<Line>>,
        mut keys: UnboundedReceiver<io::Result<Event>>,
        refresh_tx: NotifyTx,
        capture_task: AbortOnDropHandle<()>,
    ) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                msg = output_rx.recv() => {
                    let Some(lines) = msg else {
                        // Capture loop exited (receiver dropped or task ended).
                        capture_task.await?;
                        return Ok(());
                    };
                    self.on_command_output(lines)?;
                }
                key = recv_key_press(&mut keys) => {
                    match self.on_key(key?)? {
                        KeyResult::Continue => {}
                        KeyResult::Quit => {
                            // `capture_task` and the key reader abort on drop.
                            return Ok(());
                        }
                        KeyResult::Refresh => {
                            // Wakes the interval wait. A permit is stored if a
                            // capture is in progress, so the next wait is skipped.
                            refresh_tx.notify_one();
                        }
                    }
                }
            }
        }
    }

    fn resume_search_pattern(&mut self) {
        let entries = self.history.entries();
        self.search.resume_from_history(entries);
    }

    fn on_command_output(&mut self, lines: Vec<Line>) -> anyhow::Result<()> {
        let size = terminal_size()?;
        let anchor = self.search.anchor(self.renderer.lines());
        self.renderer.set_lines(lines, size);
        self.search.reattach(self.renderer.lines(), anchor);
        self.search.apply_highlight(&mut self.renderer);
        if !matches!(self.mode, Mode::Prompt(_) | Mode::Dash | Mode::Mark(_)) {
            let label = status_after_output(
                self.renderer.status_label(),
                self.search.match_label(self.renderer.lines()),
            );
            self.renderer.set_status_label(label);
        }
        if !matches!(self.mode, Mode::Help) {
            self.renderer.draw(&mut io::stdout(), size)?;
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) -> anyhow::Result<KeyResult> {
        if matches!(self.mode, Mode::Prompt(_)) {
            self.on_prompt_key(key)?;
            return Ok(KeyResult::Continue);
        }
        if matches!(self.mode, Mode::Dash) {
            return self.on_dash_key(key);
        }
        if matches!(self.mode, Mode::Mark(_)) {
            return self.on_mark_key(key);
        }
        if matches!(self.mode, Mode::Help) {
            return self.on_help_key(key);
        }
        self.on_output_key(key)
    }

    fn on_dash_key(&mut self, key: KeyEvent) -> anyhow::Result<KeyResult> {
        self.mode = Mode::Output;
        let size = terminal_size()?;
        let option = match key.code {
            KeyCode::Char('i') if !key.modifiers.contains(KeyModifiers::CONTROL) => Some(false),
            KeyCode::Char('I') if !key.modifiers.contains(KeyModifiers::CONTROL) => Some(true),
            _ => None,
        };
        if let Some(always) = option {
            let anchor = self.search.anchor(self.renderer.lines());
            if always {
                self.search.toggle_ignore_case_always();
            } else {
                self.search.toggle_ignore_case();
            }
            self.search.reattach(self.renderer.lines(), anchor);
            self.search.apply_highlight(&mut self.renderer);
            self.renderer
                .set_status_label(Some(self.search.case_mode().label().to_owned()));
            self.renderer.redraw(&mut io::stdout(), size)?;
            return Ok(KeyResult::Continue);
        }
        // Unknown option or cancel: drop the `-` prompt.
        let label = self.search.match_label(self.renderer.lines());
        self.renderer.set_status_label(label);
        self.renderer.redraw(&mut io::stdout(), size)?;
        Ok(KeyResult::Continue)
    }

    fn on_prompt_key(&mut self, key: KeyEvent) -> anyhow::Result<()> {
        let size = terminal_size()?;
        let mut prompt = match mem::replace(&mut self.mode, Mode::Output) {
            Mode::Prompt(prompt) => prompt,
            other => {
                self.mode = other;
                return Ok(());
            }
        };
        match prompt.on_key(&key, self.history.entries()) {
            PromptOutcome::Editing => {
                self.renderer
                    .set_prompt_status(prompt.status_label(), prompt.caret_cols());
                self.mode = Mode::Prompt(prompt);
                self.renderer.draw(&mut io::stdout(), size)?;
            }
            PromptOutcome::Redraw => {
                self.mode = Mode::Prompt(prompt);
                self.renderer.redraw(&mut io::stdout(), size)?;
            }
            PromptOutcome::Ignore => {
                self.mode = Mode::Prompt(prompt);
            }
            PromptOutcome::Cancel => {
                let label = self.search.match_label(self.renderer.lines());
                self.renderer.set_status_label(label);
                self.renderer.redraw(&mut io::stdout(), size)?;
            }
            PromptOutcome::Submit => {
                let forward = prompt.forward();
                let query = prompt.take_query();
                self.history.record(&query);
                if query.is_empty() {
                    self.resume_search_pattern();
                }
                let outcome =
                    submit_prompt(&mut self.renderer, &mut self.search, size, forward, query);
                show_search_outcome(&mut self.renderer, &self.search, outcome, size)?;
            }
        }
        Ok(())
    }

    fn begin_mark(&mut self, op: MarkOp) -> anyhow::Result<KeyResult> {
        let size = terminal_size()?;
        self.mode = Mode::Mark(op);
        self.renderer
            .set_prompt_status(op.prompt_label().to_owned(), 1);
        self.renderer.redraw(&mut io::stdout(), size)?;
        Ok(KeyResult::Continue)
    }

    fn on_mark_key(&mut self, key: KeyEvent) -> anyhow::Result<KeyResult> {
        let op = match mem::replace(&mut self.mode, Mode::Output) {
            Mode::Mark(op) => op,
            other => {
                self.mode = other;
                return Ok(KeyResult::Continue);
            }
        };
        let size = terminal_size()?;
        let name = match key.code {
            KeyCode::Char(c)
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && !key.modifiers.contains(KeyModifiers::ALT) =>
            {
                c
            }
            _ => {
                let label = self.search.match_label(self.renderer.lines());
                self.renderer.set_status_label(label);
                self.renderer.redraw(&mut io::stdout(), size)?;
                return Ok(KeyResult::Continue);
            }
        };
        match op {
            MarkOp::Set => {
                self.marks.rows.insert(name, self.renderer.top_pos(size));
                let label = self.search.match_label(self.renderer.lines());
                self.renderer.set_status_label(label);
                self.renderer.redraw(&mut io::stdout(), size)?;
            }
            MarkOp::Goto => {
                let Some(pos) = self.marks.rows.get(&name).copied() else {
                    self.renderer.set_status_label(Some(NOT_SET.to_owned()));
                    self.renderer.redraw(&mut io::stdout(), size)?;
                    return Ok(KeyResult::Continue);
                };
                self.search.clear_match();
                self.search.apply_highlight(&mut self.renderer);
                self.renderer.set_status_label(None);
                self.renderer.scroll_to_pos(pos, size);
                self.renderer.redraw(&mut io::stdout(), size)?;
            }
        }
        Ok(KeyResult::Continue)
    }

    fn on_help_key(&mut self, key: KeyEvent) -> anyhow::Result<KeyResult> {
        let Some(action) = key_action(&key) else {
            return Ok(KeyResult::Continue);
        };
        match action {
            KeyAction::Quit | KeyAction::Help => {
                self.mode = Mode::Output;
                let size = terminal_size()?;
                self.renderer.redraw(&mut io::stdout(), size)?;
                Ok(KeyResult::Continue)
            }
            KeyAction::DismissMessage => {
                let Some(status) = self.renderer.status_label() else {
                    return Ok(KeyResult::Continue);
                };
                if is_dismissable_status(status) {
                    let label = self.search.match_label(self.renderer.lines());
                    self.renderer.set_status_label(label);
                }
                Ok(KeyResult::Continue)
            }
            KeyAction::Refresh => Ok(KeyResult::Refresh),
            KeyAction::Dash => Ok(KeyResult::Continue),
            KeyAction::SearchForward
            | KeyAction::SearchBackward
            | KeyAction::SearchNext
            | KeyAction::SearchPrev
            | KeyAction::SetMark
            | KeyAction::GotoMark => Ok(KeyResult::Continue),
            action @ (KeyAction::ScrollDown
            | KeyAction::ScrollUp
            | KeyAction::Redraw
            | KeyAction::ScrollTop
            | KeyAction::ScrollBottom
            | KeyAction::ScrollPageDown
            | KeyAction::ScrollPageUp) => {
                apply_view_action(&mut self.help_renderer, action)?;
                Ok(KeyResult::Continue)
            }
        }
    }

    fn on_output_key(&mut self, key: KeyEvent) -> anyhow::Result<KeyResult> {
        let Some(action) = key_action(&key) else {
            return Ok(KeyResult::Continue);
        };
        match action {
            KeyAction::Dash => {
                let size = terminal_size()?;
                self.mode = Mode::Dash;
                self.renderer.set_prompt_status("-".to_owned(), 1);
                self.renderer.redraw(&mut io::stdout(), size)?;
                Ok(KeyResult::Continue)
            }
            KeyAction::DismissMessage => {
                let Some(status) = self.renderer.status_label() else {
                    return Ok(KeyResult::Continue);
                };
                if !is_dismissable_status(status) {
                    return Ok(KeyResult::Continue);
                }
                let size = terminal_size()?;
                let label = self.search.match_label(self.renderer.lines());
                self.renderer.set_status_label(label);
                self.renderer.redraw(&mut io::stdout(), size)?;
                Ok(KeyResult::Continue)
            }
            KeyAction::Quit => Ok(KeyResult::Quit),
            KeyAction::Help => {
                let size = terminal_size()?;
                self.mode = Mode::Help;
                self.help_renderer.set_lines(help_lines(), size);
                self.help_renderer.scroll_top();
                self.help_renderer.redraw(&mut io::stdout(), size)?;
                Ok(KeyResult::Continue)
            }
            KeyAction::Refresh => Ok(KeyResult::Refresh),
            KeyAction::SearchForward | KeyAction::SearchBackward => {
                let size = terminal_size()?;
                let forward = matches!(action, KeyAction::SearchForward);
                let prompt = Prompt::new(forward);
                self.renderer
                    .set_prompt_status(prompt.status_label(), prompt.caret_cols());
                self.mode = Mode::Prompt(prompt);
                self.renderer.redraw(&mut io::stdout(), size)?;
                Ok(KeyResult::Continue)
            }
            KeyAction::SetMark => self.begin_mark(MarkOp::Set),
            KeyAction::GotoMark => self.begin_mark(MarkOp::Goto),
            KeyAction::SearchNext | KeyAction::SearchPrev => {
                let size = terminal_size()?;
                let reverse = matches!(action, KeyAction::SearchPrev);
                self.resume_search_pattern();
                let outcome = search_next(&mut self.renderer, &mut self.search, size, reverse);
                show_search_outcome(&mut self.renderer, &self.search, outcome, size)?;
                Ok(KeyResult::Continue)
            }
            action @ (KeyAction::ScrollDown
            | KeyAction::ScrollUp
            | KeyAction::Redraw
            | KeyAction::ScrollTop
            | KeyAction::ScrollBottom
            | KeyAction::ScrollPageDown
            | KeyAction::ScrollPageUp) => {
                if !matches!(action, KeyAction::Redraw) {
                    // Drop the selected hit so the next n/N searches from
                    // the screen edge, like a fresh / or ?.
                    self.search.clear_match();
                    self.search.apply_highlight(&mut self.renderer);
                    self.renderer.set_status_label(None);
                }
                apply_view_action(&mut self.renderer, action)?;
                Ok(KeyResult::Continue)
            }
        }
    }
}

fn show_search_outcome(
    renderer: &mut Renderer,
    search: &Search,
    outcome: SearchOutcome,
    size: Size,
) -> anyhow::Result<()> {
    match outcome {
        SearchOutcome::Found => {
            renderer.set_status_label(search.match_label(renderer.lines()));
        }
        SearchOutcome::NotFound => match search.match_label(renderer.lines()) {
            Some(label) => {
                let mut msg = String::from(PATTERN_NOT_FOUND);
                msg.push(' ');
                msg.push_str(&label);
                renderer.set_status_label(Some(msg));
            }
            None => {
                renderer.set_status_label(Some(PATTERN_NOT_FOUND.to_owned()));
            }
        },
        SearchOutcome::NoPattern => {
            renderer.set_status_label(Some(NO_PREVIOUS_SEARCH.to_owned()));
        }
    }
    renderer.redraw(&mut io::stdout(), size)?;
    Ok(())
}

fn apply_view_action(view: &mut Renderer, action: KeyAction) -> anyhow::Result<()> {
    let size = terminal_size()?;
    match action {
        KeyAction::ScrollDown => {
            view.scroll_down(size);
            view.draw(&mut io::stdout(), size)?;
        }
        KeyAction::ScrollUp => {
            view.scroll_up(size);
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
        KeyAction::Quit
        | KeyAction::Help
        | KeyAction::DismissMessage
        | KeyAction::Dash
        | KeyAction::Refresh
        | KeyAction::SearchForward
        | KeyAction::SearchBackward
        | KeyAction::SearchNext
        | KeyAction::SearchPrev
        | KeyAction::SetMark
        | KeyAction::GotoMark => {}
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum KeyAction {
    Quit,
    Help,
    DismissMessage,
    Dash,
    Refresh,
    SearchForward,
    SearchBackward,
    SearchNext,
    SearchPrev,
    ScrollDown,
    ScrollUp,
    ScrollTop,
    ScrollBottom,
    ScrollPageDown,
    ScrollPageUp,
    Redraw,
    SetMark,
    GotoMark,
}

/// Receive key presses until one arrives, or the reader ends.
/// Non-key and non-press events are discarded. The caller should race this
/// against other work and drop it to stop waiting.
async fn recv_key_press(keys: &mut UnboundedReceiver<io::Result<Event>>) -> io::Result<KeyEvent> {
    loop {
        match keys.recv().await {
            Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => return Ok(key),
            Some(Ok(_)) => {}
            Some(Err(err)) => return Err(err),
            None => return Err(io::Error::other("key reader stopped")),
        }
    }
}

fn is_dismissable_status(status: &str) -> bool {
    status.starts_with(PATTERN_NOT_FOUND) || status == NO_PREVIOUS_SEARCH || status == NOT_SET
}

fn key_action(key: &KeyEvent) -> Option<KeyAction> {
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
        KeyCode::Esc => Some(KeyAction::DismissMessage),
        KeyCode::Char('-') => Some(KeyAction::Dash),
        KeyCode::Char('q') => Some(KeyAction::Quit),
        KeyCode::Char('h') => Some(KeyAction::Help),
        KeyCode::Char('r') => Some(KeyAction::Refresh),
        KeyCode::Char('/') => Some(KeyAction::SearchForward),
        KeyCode::Char('?') => Some(KeyAction::SearchBackward),
        KeyCode::Char('n') => Some(KeyAction::SearchNext),
        KeyCode::Char('N') => Some(KeyAction::SearchPrev),
        KeyCode::Char('m') => Some(KeyAction::SetMark),
        KeyCode::Char('\'') => Some(KeyAction::GotoMark),
        KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => Some(KeyAction::ScrollDown),
        KeyCode::Char('k') | KeyCode::Up => Some(KeyAction::ScrollUp),
        KeyCode::Char('g') | KeyCode::Home => Some(KeyAction::ScrollTop),
        KeyCode::Char('G') | KeyCode::End => Some(KeyAction::ScrollBottom),
        KeyCode::Char(' ') | KeyCode::PageDown => Some(KeyAction::ScrollPageDown),
        KeyCode::PageUp => Some(KeyAction::ScrollPageUp),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::KeyCode;
    use crossterm::event::KeyEvent;
    use crossterm::event::KeyEventKind;
    use crossterm::event::KeyEventState;
    use crossterm::event::KeyModifiers;

    use crate::event_loop::KeyAction;
    use crate::event_loop::key_action;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
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
    fn test_search_keys() {
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('/'))),
                Some(KeyAction::SearchForward)
            )
        );
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('?'))),
                Some(KeyAction::SearchBackward)
            )
        );
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('n'))),
                Some(KeyAction::SearchNext)
            )
        );
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('N'))),
                Some(KeyAction::SearchPrev)
            )
        );
    }

    #[test]
    fn test_enter_scrolls_down() {
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Enter)),
                Some(KeyAction::ScrollDown)
            )
        );
    }

    #[test]
    fn test_dash_starts_option() {
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('-'))),
                Some(KeyAction::Dash)
            )
        );
    }

    #[test]
    fn test_esc_dismisses_message() {
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Esc)),
                Some(KeyAction::DismissMessage)
            )
        );
    }

    #[test]
    fn test_home_end_scroll() {
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Home)),
                Some(KeyAction::ScrollTop)
            )
        );
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::End)),
                Some(KeyAction::ScrollBottom)
            )
        );
    }

    #[test]
    fn test_mark_keys() {
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('m'))),
                Some(KeyAction::SetMark)
            )
        );
        assert_eq!(
            true,
            matches!(
                key_action(&press(KeyCode::Char('\''))),
                Some(KeyAction::GotoMark)
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
