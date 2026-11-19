//! Full-screen terminal UI: scrolling chat, multi-line input, status line and approval popup.
//!
//! The harness talks to it through the [`Tui`] frontend; the UI sends prompts back
//! over a channel.

use crate::frontend::{ContextUsage, Event as AgentEvent, Frontend, Question, ToolCall};
use crate::harness::Session;
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, Event,
    EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags,
    MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use futures::StreamExt;
use futures::future::BoxFuture;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use ratatui_textarea::{TextArea, WrapMode};
use std::collections::VecDeque;
use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use unicode_width::UnicodeWidthChar;

/// What the status line shows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Status {
    Idle,
    /// Waiting for the model (also while it reasons).
    Thinking,
    /// A tool is executing.
    Tool,
}

/// Something the UI has to display or ask.
pub enum UiEvent {
    /// A streamed fragment of the assistant's answer.
    Text(String),
    /// A one-line note, e.g. an automatically approved tool call.
    Info(String),
    /// Output of a tool.
    ToolOutput(String),
    Error(String),
    Status(Status),
    /// How full the context is, shown at the right of the status line.
    Context(ContextUsage),
    /// The agent asks a question; the next message the user sends is the answer.
    Question {
        text: String,
        reply: oneshot::Sender<String>,
    },
    /// Ask the user; `true` is sent back on `y`, `false` on any other key.
    Approval {
        prompt: String,
        reply: oneshot::Sender<bool>,
    },
}

static EVENTS: OnceLock<mpsc::UnboundedSender<UiEvent>> = OnceLock::new();
static KEYBOARD_ENHANCED: AtomicBool = AtomicBool::new(false);

/// Sends an event to the UI. Does nothing while the UI is not running (CLI modes, tests).
fn emit(event: UiEvent) {
    if let Some(tx) = EVENTS.get() {
        let _ = tx.send(event);
    }
}

/// Asks the user via the approval popup; anything but `y` (or a closed UI) is `false`.
async fn request_approval(prompt: String) -> bool {
    let (reply, answer) = oneshot::channel();
    emit(UiEvent::Approval { prompt, reply });
    answer.await == Ok(true)
}

/// Shows `question` in the chat; the user's next message answers it. A closed UI is no answer.
async fn request_answer(text: String) -> Option<String> {
    let (reply, answer) = oneshot::channel();
    emit(UiEvent::Question { text, reply });
    answer.await.ok()
}

/// The terminal UI as the harness's frontend.
pub struct Tui;

impl Frontend for Tui {
    fn emit(&self, event: AgentEvent) {
        match event {
            AgentEvent::Text(text) => emit(UiEvent::Text(text)),
            AgentEvent::Note(text) => emit(UiEvent::Info(text)),
            AgentEvent::Reasoning(_) => {}
            AgentEvent::ToolStarted(call) => {
                let text = if call.ask {
                    format!("[approved] {}", call.name)
                } else {
                    call.title
                };
                emit(UiEvent::Info(text));
                emit(UiEvent::Status(Status::Tool));
            }
            AgentEvent::ToolDenied(call) => emit(UiEvent::Info(format!("[denied] {}", call.name))),
            AgentEvent::ToolFinished { kind, output, .. } => {
                // Skill texts and subagent reports are for the model; command output is shown.
                if kind == "execute" {
                    emit(UiEvent::ToolOutput(output));
                }
                emit(UiEvent::Status(Status::Thinking));
            }
            AgentEvent::Context(usage) => emit(UiEvent::Context(usage)),
            // A subagent's answer is for the main agent; only its notes are shown.
            AgentEvent::Sub { event, .. } => {
                if let AgentEvent::Note(text) = *event {
                    emit(UiEvent::Info(text));
                }
            }
            AgentEvent::SubagentStarted { .. } | AgentEvent::SubagentFinished { .. } => {}
        }
    }

    fn approve(&self, call: &ToolCall) -> BoxFuture<'static, bool> {
        Box::pin(request_approval(format!("{}\napprove? [y/N] ", call.title)))
    }

    fn ask(&self, question: &Question) -> BoxFuture<'static, Option<String>> {
        let mut text = format!("? {}", question.question);
        for (i, option) in question.options.iter().enumerate() {
            text.push_str(&format!("\n  {}) {option}", i + 1));
        }
        if !question.options.is_empty() {
            text.push_str("\n(type a number or your own answer)");
        }
        let options = question.options.clone();
        Box::pin(async move {
            let answer = request_answer(text).await?;
            // A number picks the matching option.
            let picked = answer
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|n| options.get(n.checked_sub(1)?));
            Some(picked.cloned().unwrap_or(answer))
        })
    }
}

enum Entry {
    User(String),
    Assistant(String),
    Info(String),
    ToolOutput(String),
    Error(String),
}

#[derive(Debug, PartialEq)]
enum Action {
    None,
    Quit,
    Submit(String),
    /// Switch mouse capture (wheel scrolling) on or off.
    ToggleMouse,
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const SCROLL_STEP: usize = 3;

fn new_input() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_block(Block::default().borders(Borders::ALL).title(" Message "));
    input.set_placeholder_text("Enter sends, Alt+Enter adds a line");
    input.set_cursor_line_style(Style::default());
    input.set_wrap_mode(WrapMode::WordOrGlyph);
    input
}

struct App {
    entries: Vec<Entry>,
    input: TextArea<'static>,
    status: Status,
    /// How full the context was after the latest model call.
    context: Option<ContextUsage>,
    /// Questions of the agent; the next message answers the first one.
    questions: VecDeque<oneshot::Sender<String>>,
    /// Pending approval requests; the first one is shown.
    approvals: VecDeque<(String, oneshot::Sender<bool>)>,
    /// Chat lines hidden below the viewport (0 follows the newest output).
    scroll: usize,
    chat_height: usize,
    tick: usize,
    /// Mouse capture enables wheel scrolling but blocks text selection in the terminal.
    mouse: bool,
}

impl App {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
            input: new_input(),
            status: Status::Idle,
            context: None,
            questions: VecDeque::new(),
            approvals: VecDeque::new(),
            scroll: 0,
            chat_height: 10,
            tick: 0,
            mouse: false,
        }
    }

    fn input_is_empty(&self) -> bool {
        self.input.lines().iter().all(|l| l.is_empty())
    }

    fn handle_event(&mut self, event: Event) -> Action {
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.input.insert_str(text);
                Action::None
            }
            Event::Mouse(mouse) => {
                match mouse.kind {
                    MouseEventKind::ScrollUp => self.scroll += SCROLL_STEP,
                    MouseEventKind::ScrollDown => {
                        self.scroll = self.scroll.saturating_sub(SCROLL_STEP)
                    }
                    _ => {}
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Action {
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if ctrl && key.code == KeyCode::Char('t') {
            return Action::ToggleMouse;
        }
        if let Some((_, reply)) = self.approvals.pop_front() {
            let _ = reply.send(key.code == KeyCode::Char('y'));
            return Action::None;
        }
        let page = self.chat_height.saturating_sub(2).max(1);
        match key.code {
            KeyCode::Esc if self.input_is_empty() => Action::Quit,
            KeyCode::PageUp => {
                self.scroll += page;
                Action::None
            }
            KeyCode::PageDown => {
                self.scroll = self.scroll.saturating_sub(page);
                Action::None
            }
            KeyCode::Enter
                if !key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.submit()
            }
            KeyCode::Enter => {
                self.input.insert_newline();
                Action::None
            }
            KeyCode::Char('j') if ctrl => {
                self.input.insert_newline();
                Action::None
            }
            _ => {
                self.input.input(key);
                Action::None
            }
        }
    }

    fn submit(&mut self) -> Action {
        let text = self.input.lines().join("\n").trim().to_owned();
        if text.is_empty() {
            return Action::None;
        }
        // While the agent waits for an answer, the message is that answer.
        if let Some(reply) = self.questions.pop_front() {
            self.input = new_input();
            self.entries.push(Entry::User(text.clone()));
            self.status = Status::Thinking;
            self.scroll = 0;
            let _ = reply.send(text);
            return Action::None;
        }
        if self.status != Status::Idle {
            return Action::None;
        }
        self.input = new_input();
        self.entries.push(Entry::User(text.clone()));
        self.status = Status::Thinking;
        self.scroll = 0;
        Action::Submit(text)
    }

    fn handle_ui(&mut self, event: UiEvent) {
        match event {
            UiEvent::Text(text) => match self.entries.last_mut() {
                Some(Entry::Assistant(answer)) => answer.push_str(&text),
                _ => {
                    let text = text.trim_start_matches('\n');
                    if !text.is_empty() {
                        self.entries.push(Entry::Assistant(text.to_owned()));
                    }
                }
            },
            UiEvent::Info(text) => self.entries.push(Entry::Info(text)),
            UiEvent::ToolOutput(text) => self.entries.push(Entry::ToolOutput(text)),
            UiEvent::Error(text) => self.entries.push(Entry::Error(text)),
            UiEvent::Status(status) => self.status = status,
            UiEvent::Context(usage) => self.context = Some(usage),
            UiEvent::Question { text, reply } => {
                self.entries.push(Entry::Assistant(text));
                self.questions.push_back(reply);
            }
            UiEvent::Approval { prompt, reply } => self.approvals.push_back((prompt, reply)),
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let input_height = (self.input.lines().len() as u16 + 2).clamp(3, 10);
        let [chat, status, input] = Layout::vertical([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(input_height),
        ])
        .areas(frame.area());

        let lines = render_entries(&self.entries, chat.width as usize);
        self.chat_height = chat.height as usize;
        self.scroll = self
            .scroll
            .min(lines.len().saturating_sub(self.chat_height));
        let end = lines.len() - self.scroll;
        let start = end.saturating_sub(self.chat_height);
        frame.render_widget(Paragraph::new(lines[start..end].to_vec()), chat);

        let context = self.context_line();
        let [status, context_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(context.width() as u16),
        ])
        .spacing(2)
        .areas(status);
        frame.render_widget(Paragraph::new(self.status_line()), status);
        frame.render_widget(Paragraph::new(context), context_area);
        frame.render_widget(&self.input, input);
        if let Some((prompt, _)) = self.approvals.front() {
            render_approval(frame, prompt);
        }
    }

    fn status_line(&self) -> Line<'static> {
        let spinner = SPINNER[self.tick % SPINNER.len()];
        let mut spans = if !self.approvals.is_empty() {
            vec![Span::styled(
                "Waiting for approval",
                Style::new().fg(Color::Yellow),
            )]
        } else {
            match self.status {
                Status::Thinking => vec![Span::styled(
                    format!("{spinner} Thinking…"),
                    Style::new().fg(Color::Cyan),
                )],
                Status::Tool => vec![Span::styled(
                    format!("{spinner} Running tool…"),
                    Style::new().fg(Color::Yellow),
                )],
                Status::Idle => vec![Span::styled(
                    if self.mouse {
                        "Enter send · Alt+Enter newline · PgUp/PgDn scroll · Ctrl+T mouse: scroll (copy off) · Esc quit"
                    } else {
                        "Enter send · Alt+Enter newline · PgUp/PgDn scroll · Ctrl+T mouse: copy (wheel off) · Esc quit"
                    },
                    Style::new().fg(Color::DarkGray),
                )],
            }
        };
        if self.scroll > 0 {
            spans.push(Span::styled(
                "  (scrolled up, PgDn to follow)",
                Style::new().fg(Color::DarkGray),
            ));
        }
        Line::from(spans)
    }

    /// The context usage, yellow from 80 % of the window and red from 95 %.
    fn context_line(&self) -> Line<'static> {
        let Some(usage) = self.context else {
            return Line::default();
        };
        let color = match usage.percent() {
            Some(95..) => Color::Red,
            Some(80..) => Color::Yellow,
            _ => Color::DarkGray,
        };
        Line::styled(usage.to_string(), Style::new().fg(color))
    }
}

/// Chat entries as wrapped, styled lines with a blank line between entries.
fn render_entries(entries: &[Entry], width: usize) -> Vec<Line<'static>> {
    let plain = Style::default();
    let mut lines = Vec::new();
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            lines.push(Line::default());
        }
        let (first, rest, text, prefix_style, text_style) = match entry {
            Entry::User(t) => (
                "› ",
                "  ",
                t,
                Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                plain,
            ),
            Entry::Assistant(t) => ("", "", t, plain, plain),
            Entry::Info(t) => (
                "• ",
                "  ",
                t,
                Style::new().fg(Color::Yellow),
                Style::new().fg(Color::Yellow),
            ),
            Entry::ToolOutput(t) => (
                "│ ",
                "│ ",
                t,
                Style::new().fg(Color::DarkGray),
                Style::new().fg(Color::DarkGray),
            ),
            Entry::Error(t) => (
                "✗ ",
                "  ",
                t,
                Style::new().fg(Color::Red),
                Style::new().fg(Color::Red),
            ),
        };
        let inner = width.saturating_sub(first.chars().count());
        for (n, row) in wrap(text.trim_end(), inner).into_iter().enumerate() {
            lines.push(Line::from(vec![
                Span::styled(if n == 0 { first } else { rest }, prefix_style),
                Span::styled(row, text_style),
            ]));
        }
    }
    lines
}

/// Wraps `text` to `width` columns, preferring to break after spaces. Keeps blank lines.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    for line in text.replace('\t', "    ").lines() {
        let mut cur = String::new();
        let mut cur_width = 0;
        // Byte index in `cur` just after its last space.
        let mut space: Option<usize> = None;
        for ch in line.chars() {
            let w = ch.width().unwrap_or(0);
            if cur_width + w > width && !cur.is_empty() {
                if ch == ' ' {
                    rows.push(std::mem::take(&mut cur));
                    cur_width = 0;
                    space = None;
                    continue;
                }
                match space.take().filter(|&i| !cur[..i].trim().is_empty()) {
                    Some(i) => {
                        let rest = cur.split_off(i);
                        rows.push(cur.trim_end().to_owned());
                        cur = rest;
                    }
                    None => rows.push(std::mem::take(&mut cur)),
                }
                cur_width = cur.chars().map(|c| c.width().unwrap_or(0)).sum();
            }
            if ch == ' ' && !cur.trim().is_empty() {
                space = Some(cur.len() + 1);
            }
            cur.push(ch);
            cur_width += w;
        }
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn render_approval(frame: &mut Frame, prompt: &str) {
    let prompt = prompt.trim_end();
    let prompt = prompt
        .strip_suffix("approve? [y/N]")
        .unwrap_or(prompt)
        .trim_end();
    let area = frame.area();
    let width = (area.width * 4 / 5).max(20).min(area.width);
    let mut rows = wrap(prompt, width.saturating_sub(2) as usize);
    let max_rows = (area.height.saturating_sub(5) as usize).max(1);
    if rows.len() > max_rows {
        rows.truncate(max_rows - 1);
        rows.push("…".to_owned());
    }
    let height = (rows.len() as u16 + 4).min(area.height);
    let popup = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let mut lines: Vec<Line> = rows.into_iter().map(Line::from).collect();
    lines.push(Line::default());
    lines.push(Line::styled(
        "[y] approve    [any other key] deny",
        Style::new().fg(Color::Yellow),
    ));
    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::new().fg(Color::Yellow))
                .title(" Approve tool call? "),
        ),
        popup,
    );
}

/// Runs the UI until the user quits; every submitted prompt goes to `session`, which
/// must have been created with the [`Tui`] frontend.
pub async fn run(mut session: Session) -> io::Result<()> {
    // The agent runs in its own task, so the UI stays responsive while it works.
    let (prompts, mut inbox) = mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(input) = inbox.recv().await {
            if let Err(error) = session.prompt(input).await {
                emit(UiEvent::Error(error));
            }
            emit(UiEvent::Status(Status::Idle));
        }
    });
    let (tx, mut rx) = mpsc::unbounded_channel();
    let _ = EVENTS.set(tx);
    let mut terminal = setup()?;
    let result = event_loop(&mut terminal, &mut rx, &prompts).await;
    restore();
    result
}

async fn event_loop(
    terminal: &mut DefaultTerminal,
    rx: &mut mpsc::UnboundedReceiver<UiEvent>,
    prompts: &mpsc::UnboundedSender<String>,
) -> io::Result<()> {
    let mut app = App::new();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        let action = tokio::select! {
            Some(event) = events.next() => app.handle_event(event?),
            Some(event) = rx.recv() => {
                app.handle_ui(event);
                Action::None
            }
            _ = tick.tick() => {
                app.tick = app.tick.wrapping_add(1);
                Action::None
            }
        };
        match action {
            Action::Quit => return Ok(()),
            Action::Submit(text) => {
                if prompts.send(text).is_err() {
                    return Ok(());
                }
            }
            Action::ToggleMouse => {
                app.mouse = !app.mouse;
                let mut out = io::stdout();
                if app.mouse {
                    execute!(out, EnableMouseCapture)?;
                } else {
                    execute!(out, DisableMouseCapture)?;
                }
            }
            Action::None => {}
        }
    }
}

/// Enters raw mode and the alternate screen; the terminal is also restored on panic.
fn setup() -> io::Result<DefaultTerminal> {
    let terminal = ratatui::try_init()?;
    let mut out = io::stdout();
    execute!(out, EnableBracketedPaste)?;
    // Lets terminals that support it report Shift+Enter separately from Enter.
    if matches!(
        crossterm::terminal::supports_keyboard_enhancement(),
        Ok(true)
    ) {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
        KEYBOARD_ENHANCED.store(true, Ordering::Relaxed);
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
    Ok(terminal)
}

fn restore() {
    let mut out = io::stdout();
    if KEYBOARD_ENHANCED.swap(false, Ordering::Relaxed) {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(out, DisableBracketedPaste, DisableMouseCapture);
    ratatui::restore();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn screen(app: &mut App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn enter_submits_and_alt_enter_adds_a_line() {
        let mut app = App::new();
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        type_text(&mut app, "hi");
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        type_text(&mut app, "there");
        assert_eq!(
            app.handle_key(key(KeyCode::Enter)),
            Action::Submit("hi\nthere".into())
        );
        assert!(app.input_is_empty());
        assert_eq!(app.status, Status::Thinking);
        // Busy: a second prompt is not sent.
        type_text(&mut app, "more");
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
    }

    #[test]
    fn quit_keys() {
        let mut app = App::new();
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::Quit);
        type_text(&mut app, "x");
        assert_eq!(app.handle_key(key(KeyCode::Esc)), Action::None);
        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.handle_key(ctrl_c), Action::Quit);
    }

    #[test]
    fn text_fragments_join_into_one_answer() {
        let mut app = App::new();
        app.handle_ui(UiEvent::Text("\nhel".into()));
        app.handle_ui(UiEvent::Text("lo".into()));
        app.handle_ui(UiEvent::ToolOutput("out".into()));
        app.handle_ui(UiEvent::Text("again".into()));
        assert_eq!(app.entries.len(), 3);
        assert!(matches!(&app.entries[0], Entry::Assistant(t) if t == "hello"));
    }

    #[test]
    fn the_next_message_answers_a_question() {
        let mut app = App::new();
        let (reply, mut answer) = oneshot::channel();
        app.status = Status::Tool;
        app.handle_ui(UiEvent::Question {
            text: "? Which?".into(),
            reply,
        });
        type_text(&mut app, "blue");
        // The prompt is not busy-blocked: it becomes the answer, not a new prompt.
        assert_eq!(app.handle_key(key(KeyCode::Enter)), Action::None);
        assert_eq!(answer.try_recv(), Ok("blue".to_string()));
        assert!(app.questions.is_empty());
    }

    #[test]
    fn only_y_approves() {
        for (code, expected) in [(KeyCode::Char('y'), true), (KeyCode::Enter, false)] {
            let mut app = App::new();
            let (reply, mut answer) = oneshot::channel();
            app.handle_ui(UiEvent::Approval {
                prompt: "execute ls\napprove? [y/N] ".into(),
                reply,
            });
            assert!(screen(&mut app, 60, 20).contains("Approve tool call?"));
            assert_eq!(app.handle_key(key(code)), Action::None);
            assert_eq!(answer.try_recv(), Ok(expected));
            assert!(app.approvals.is_empty());
        }
    }

    #[test]
    fn draws_chat_status_and_scrolls() {
        let mut app = App::new();
        app.handle_ui(UiEvent::Info("[isolated] ls".into()));
        app.handle_ui(UiEvent::ToolOutput("file_a\nfile_b".into()));
        app.handle_ui(UiEvent::Status(Status::Tool));
        let text = screen(&mut app, 60, 12);
        assert!(text.contains("• [isolated] ls"), "{text}");
        assert!(text.contains("│ file_b"), "{text}");
        assert!(text.contains("Running tool…"), "{text}");

        for i in 0..50 {
            app.handle_ui(UiEvent::Info(format!("line {i}")));
        }
        assert!(screen(&mut app, 60, 12).contains("line 49"));
        app.handle_key(key(KeyCode::PageUp));
        let text = screen(&mut app, 60, 12);
        assert!(!text.contains("line 49"), "{text}");
        assert!(text.contains("scrolled up"), "{text}");
    }

    #[test]
    fn status_line_shows_the_context_usage() {
        let mut app = App::new();
        assert!(!screen(&mut app, 120, 8).contains("tokens"));
        let usage = |tokens| ContextUsage {
            tokens,
            window: Some(200_000),
        };
        app.handle_ui(UiEvent::Context(usage(42_000)));
        let text = screen(&mut app, 120, 8);
        let status = text.lines().nth(4).unwrap();
        assert!(status.ends_with("42k / 200k tokens (21%)"), "{text}");
        assert!(status.starts_with("Enter send"), "{text}");
        assert_eq!(app.context_line().style.fg, Some(Color::DarkGray));
        app.handle_ui(UiEvent::Context(usage(190_000)));
        assert_eq!(app.context_line().style.fg, Some(Color::Red));
    }

    #[test]
    fn wraps_words_and_long_tokens() {
        assert_eq!(wrap("aaa bbb ccc", 7), ["aaa bbb", "ccc"]);
        assert_eq!(wrap("abcdefgh", 3), ["abc", "def", "gh"]);
        assert_eq!(wrap("a\n\nb", 5), ["a", "", "b"]);
        assert_eq!(wrap("", 5), [""]);
    }
}
