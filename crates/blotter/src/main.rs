//! blotter: the work screen. For now, a live list of Claude Code sessions from blotterd.
//!
//! Drawn for e-ink: no colour, no animation, and the only thing that changes on its own is
//! each session's age, once a minute. Everything else redraws only when the board changes.
//!
//! Usage: blotter [--config PATH]    (default ~/.config/blotter/config.toml)
//! Keys:  j/k or arrows to move, q to quit

use blotter_proto::{now_ms, parse_duration, Board, Config, Session, State};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Paragraph, Row, Table, TableState};
use ratatui::{DefaultTerminal, Frame};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

enum Update {
    Board(Board),
    Offline(String),
}

struct App {
    board: Board,
    offline: Option<String>,
    selected: TableState,
    stale_after: u64,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("blotter: {}", e);
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let mut config_path = Config::default_path();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config_path = args.next().ok_or("--config needs a path")?.into(),
            "-h" | "--help" => {
                println!("blotter [--config PATH]");
                return Ok(());
            }
            other => return Err(format!("unknown argument: {}", other)),
        }
    }
    let config = Config::load(&config_path)?;
    let (tx, rx) = mpsc::channel();
    let url = format!("http://{}/state", config.feed.listen);
    let token = config.feed.token.clone();
    thread::spawn(move || follow(&url, &token, tx));

    let mut app = App {
        board: Board::default(),
        offline: Some("connecting".into()),
        selected: TableState::default().with_selected(0),
        stale_after: parse_duration(&config.panels.agents.stale_after)?,
    };
    let mut terminal = ratatui::init();
    let result = app.run(&mut terminal, rx);
    ratatui::restore();
    result
}

/// Long-poll blotterd and pass each new board on. Reconnects after failures.
fn follow(url: &str, token: &str, tx: mpsc::Sender<Update>) {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(3))
        .timeout_read(Duration::from_secs(40))
        .build();
    let mut version: Option<u64> = None;
    loop {
        let request = agent.get(url).set("Authorization", &format!("Bearer {}", token));
        let request = match version {
            Some(v) => request.query("after", &v.to_string()),
            None => request,
        };
        let update = match request.call() {
            Ok(response) => match response.into_json::<Board>() {
                Ok(board) => {
                    version = Some(board.version);
                    Update::Board(board)
                }
                Err(e) => Update::Offline(format!("bad reply: {}", e)),
            },
            Err(ureq::Error::Status(401, _)) => Update::Offline("token rejected".into()),
            Err(e) => Update::Offline(e.kind().to_string()),
        };
        let failed = matches!(update, Update::Offline(_));
        if tx.send(update).is_err() {
            return;
        }
        if failed {
            version = None;
            thread::sleep(Duration::from_secs(5));
        }
    }
}

impl App {
    fn run(&mut self, terminal: &mut DefaultTerminal, rx: mpsc::Receiver<Update>) -> Result<(), String> {
        loop {
            terminal.draw(|f| self.draw(f)).map_err(|e| e.to_string())?;
            // Wake for keys, board updates, or the next minute (for ages).
            if event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())? {
                if let TermEvent::Key(key) = event::read().map_err(|e| e.to_string())? {
                    if key.kind == KeyEventKind::Press && !self.key(key.code) {
                        return Ok(());
                    }
                }
            }
            for update in rx.try_iter() {
                match update {
                    Update::Board(board) => {
                        self.board = board;
                        self.offline = None;
                    }
                    Update::Offline(why) => self.offline = Some(why),
                }
            }
            let rows = self.board.sessions.len();
            if self.selected.selected().is_some_and(|i| i >= rows) {
                self.selected.select(Some(rows.saturating_sub(1)));
            }
        }
    }

    /// Returns false to quit.
    fn key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => return false,
            KeyCode::Char('j') | KeyCode::Down => self.selected.select_next(),
            KeyCode::Char('k') | KeyCode::Up => self.selected.select_previous(),
            KeyCode::Char('g') | KeyCode::Home => self.selected.select_first(),
            KeyCode::Char('G') | KeyCode::End => self.selected.select_last(),
            _ => {}
        }
        true
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [header, body, footer] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
        let now = now_ms();
        let sessions = self.board.sorted();

        let needs = sessions.iter().filter(|s| s.state == State::NeedsYou).count();
        let plural = if sessions.len() == 1 { "" } else { "s" };
        let mut title = vec![Span::from("blotter").bold(), Span::from(format!("   {} session{}", sessions.len(), plural))];
        if needs > 0 {
            let verb = if needs == 1 { "needs" } else { "need" };
            title.push(Span::from(format!("   {} {} you", needs, verb)).bold());
        }
        if let Some(why) = &self.offline {
            title.push(Span::from(format!("   offline: {}", why)).reversed());
        }
        frame.render_widget(Paragraph::new(Line::from(title)), header);

        if sessions.is_empty() {
            frame.render_widget(Paragraph::new("No Claude Code sessions. Start one on the Mac."), body);
        } else {
            let rows = sessions.iter().map(|s| {
                let stale = s.is_stale(now, self.stale_after);
                let row = Row::new([
                    Cell::from(symbol(s, stale)),
                    Cell::from(s.title.clone().unwrap_or_else(|| s.project.clone())),
                    Cell::from(state_word(s, stale)),
                    Cell::from(age(now.saturating_sub(s.since))),
                    Cell::from(detail(s)),
                ]);
                if s.state == State::NeedsYou { row.bold() } else { row }
            });
            let table = Table::new(
                rows,
                [Constraint::Length(1), Constraint::Length(16), Constraint::Length(9), Constraint::Length(4), Constraint::Fill(1)],
            )
            .column_spacing(2)
            .row_highlight_style(Style::new().reversed());
            frame.render_stateful_widget(table, body, &mut self.selected);
        }

        frame.render_widget(Paragraph::new("j/k move   q quit").dim(), footer);
    }
}

fn symbol(s: &Session, stale: bool) -> &'static str {
    match (s.state, stale) {
        (_, true) => "?",
        (State::NeedsYou, _) => "◆",
        (State::Working, _) => "●",
        (State::Idle, _) => "○",
    }
}

fn state_word(s: &Session, stale: bool) -> &'static str {
    match (s.state, stale) {
        (_, true) => "stale",
        (State::NeedsYou, _) => "needs you",
        (State::Working, _) => "working",
        (State::Idle, _) => "idle",
    }
}

/// The one line that says what a session is up to.
fn detail(s: &Session) -> String {
    let line = match s.state {
        State::NeedsYou => s.waiting_on.clone(),
        State::Working => s.tool.as_ref().map(|t| t.to_string()).or(s.prompt.clone()),
        State::Idle => s.error.as_ref().map(|e| format!("failed: {}", e)).or(s.last_message.clone()),
    };
    line.map(|l| l.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_owned()).unwrap_or_default()
}

fn age(ms: u64) -> String {
    let m = ms / 60_000;
    match m {
        0 => "now".into(),
        1..=59 => format!("{}m", m),
        60..=1439 => format!("{}h", m / 60),
        _ => format!("{}d", m / 1440),
    }
}
