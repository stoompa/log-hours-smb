use std::time::Duration;

use anyhow::Result;
use chrono::{Local, NaiveDate};
use ratatui::Frame;
use ratatui::crossterm::event::{
    self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Cell, Paragraph, Row, Table, TableState, Wrap};
use tokio::sync::mpsc;

use crate::ai::{self, Parsed};
use crate::config::Config;
use crate::model::{Entry, Status, fmt_time};
use crate::worker::{self, Cmd, Event};

enum Mode {
    Input,
    Parsing,
    Preview,
    Editing { row: Option<usize>, buffer: String },
    Submitting,
}

enum Msg {
    Key(KeyEvent),
    Worker(Event),
    Parsed(Result<Parsed, String>),
}

struct App {
    cfg: Config,
    api_key: String,
    mode: Mode,
    input: String,
    rows: Vec<(Entry, Status)>,
    table: TableState,
    summary: String,
    smb: String,
    log: Vec<String>,
    quit: bool,
}

pub async fn run(cfg: Config, api_key: String, headed: bool) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<Msg>();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<Cmd>();
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Event>();

    tokio::spawn(worker::run(cfg.clone(), headed, cmd_rx, ev_tx));
    let fwd = tx.clone();
    tokio::spawn(async move {
        while let Some(ev) = ev_rx.recv().await {
            let _ = fwd.send(Msg::Worker(ev));
        }
    });
    let keys = tx.clone();
    std::thread::spawn(move || {
        loop {
            if event::poll(Duration::from_millis(200)).unwrap_or(false) {
                if let Ok(TermEvent::Key(k)) = event::read()
                    && keys.send(Msg::Key(k)).is_err()
                {
                    break;
                }
            } else if keys.is_closed() {
                break;
            }
        }
    });

    let mut app = App {
        cfg,
        api_key,
        mode: Mode::Input,
        input: String::new(),
        rows: Vec::new(),
        table: TableState::default(),
        summary: String::new(),
        smb: "logging in…".into(),
        log: Vec::new(),
        quit: false,
    };

    let mut terminal = ratatui::init();
    let result = loop {
        if let Err(e) = terminal.draw(|f| app.draw(f)) {
            break Err(e.into());
        }
        let Some(msg) = rx.recv().await else {
            break Ok(());
        };
        app.handle(msg, &tx, &cmd_tx);
        if app.quit {
            break Ok(());
        }
    };
    ratatui::restore();
    result
}

impl App {
    fn handle(
        &mut self,
        msg: Msg,
        tx: &mpsc::UnboundedSender<Msg>,
        cmds: &mpsc::UnboundedSender<Cmd>,
    ) {
        match msg {
            Msg::Key(key) if key.kind == KeyEventKind::Press => self.key(key, tx, cmds),
            Msg::Key(_) => {}
            Msg::Parsed(Ok(parsed)) => {
                self.summary = parsed.summary;
                self.rows = parsed
                    .entries
                    .into_iter()
                    .map(|e| (e, Status::Unknown))
                    .collect();
                self.table
                    .select(if self.rows.is_empty() { None } else { Some(0) });
                self.mode = if self.rows.is_empty() {
                    Mode::Input
                } else {
                    Mode::Preview
                };
                self.check(cmds);
            }
            Msg::Parsed(Err(e)) => {
                self.summary = format!("error: {e}");
                self.mode = Mode::Input;
            }
            Msg::Worker(Event::Log(line)) => self.push_log(line),
            Msg::Worker(Event::Ready) => {
                self.smb = "ready".into();
                self.check(cmds);
            }
            Msg::Worker(Event::LoginFailed(e)) => {
                self.smb = "login failed (will retry on submit)".into();
                self.push_log(format!("login failed: {e}"));
            }
            Msg::Worker(Event::Statuses(statuses)) => {
                if statuses.len() == self.rows.len() {
                    for (row, status) in self.rows.iter_mut().zip(statuses) {
                        row.1 = status;
                    }
                }
            }
            Msg::Worker(Event::Submitted(results)) => {
                for (i, status) in results {
                    if let Some(row) = self.rows.get_mut(i) {
                        row.1 = status;
                    }
                }
                self.mode = Mode::Preview;
            }
        }
    }

    fn push_log(&mut self, line: String) {
        self.log
            .push(format!("{} {line}", Local::now().format("%H:%M:%S")));
        if self.log.len() > 200 {
            self.log.remove(0);
        }
    }

    fn check(&mut self, cmds: &mpsc::UnboundedSender<Cmd>) {
        if self.smb == "ready" && !self.rows.is_empty() {
            for row in &mut self.rows {
                row.1 = Status::Unknown;
            }
            let _ = cmds.send(Cmd::Check(
                self.rows.iter().map(|(e, _)| e.clone()).collect(),
            ));
        }
    }

    fn key(
        &mut self,
        key: KeyEvent,
        tx: &mpsc::UnboundedSender<Msg>,
        cmds: &mpsc::UnboundedSender<Cmd>,
    ) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match &mut self.mode {
            Mode::Input => match key.code {
                KeyCode::Char(c) => self.input.push(c),
                KeyCode::Backspace => {
                    self.input.pop();
                }
                KeyCode::Enter if !self.input.trim().is_empty() => self.parse(tx),
                KeyCode::Esc if !self.rows.is_empty() => self.mode = Mode::Preview,
                KeyCode::Esc => self.quit = true,
                _ => {}
            },
            Mode::Parsing | Mode::Submitting => {}
            Mode::Preview => match key.code {
                KeyCode::Up | KeyCode::Char('k') => self.table.select_previous(),
                KeyCode::Down | KeyCode::Char('j') => self.table.select_next(),
                KeyCode::Char('d') => {
                    if let Some(i) = self.table.selected().filter(|i| *i < self.rows.len()) {
                        self.rows.remove(i);
                        if self.rows.is_empty() {
                            self.table.select(None);
                        }
                    }
                }
                KeyCode::Char('e') => {
                    if let Some(i) = self.table.selected().filter(|i| *i < self.rows.len()) {
                        self.mode = Mode::Editing {
                            row: Some(i),
                            buffer: self.rows[i].0.to_line(),
                        };
                    }
                }
                KeyCode::Char('a') => {
                    let buffer = self
                        .table
                        .selected()
                        .and_then(|i| self.rows.get(i))
                        .map(|(e, _)| e.to_line())
                        .unwrap_or_else(|| format!("{} 08:00-12:00", Local::now().date_naive()));
                    self.mode = Mode::Editing { row: None, buffer };
                }
                KeyCode::Char('r') => self.check(cmds),
                KeyCode::Enter => self.submit(cmds),
                KeyCode::Esc | KeyCode::Char('n') => self.mode = Mode::Input,
                KeyCode::Char('q') => self.quit = true,
                _ => {}
            },
            Mode::Editing { row, buffer } => match key.code {
                KeyCode::Char(c) => buffer.push(c),
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Enter => match Entry::parse_line(buffer) {
                    Ok(entry) => {
                        match *row {
                            Some(i) => self.rows[i] = (entry, Status::Unknown),
                            None => self.rows.push((entry, Status::Unknown)),
                        }
                        self.rows.sort_by_key(|(e, _)| (e.date, e.start));
                        self.mode = Mode::Preview;
                        self.check(cmds);
                    }
                    Err(e) => self.push_log(format!("edit: {e}")),
                },
                KeyCode::Esc => self.mode = Mode::Preview,
                _ => {}
            },
        }
    }

    fn parse(&mut self, tx: &mpsc::UnboundedSender<Msg>) {
        self.mode = Mode::Parsing;
        self.summary = "thinking…".into();
        let (cfg, key, phrase, tx) = (
            self.cfg.clone(),
            self.api_key.clone(),
            self.input.clone(),
            tx.clone(),
        );
        let today: NaiveDate = Local::now().date_naive();
        tokio::spawn(async move {
            let result = ai::parse(&cfg, &key, &phrase, today)
                .await
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::Parsed(result));
        });
    }

    fn submit(&mut self, cmds: &mpsc::UnboundedSender<Cmd>) {
        if self.smb != "ready" {
            self.push_log("SMB is not logged in yet".into());
            return;
        }
        let todo: Vec<(usize, Entry)> = self
            .rows
            .iter()
            .enumerate()
            .filter(|(_, (_, s))| *s == Status::New)
            .map(|(i, (e, _))| (i, e.clone()))
            .collect();
        if todo.is_empty() {
            self.push_log("nothing new to submit".into());
            return;
        }
        self.mode = Mode::Submitting;
        let _ = cmds.send(Cmd::Submit(todo));
    }

    fn draw(&mut self, f: &mut Frame) {
        let [header, input, summary, table, log, help] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(6),
            Constraint::Length(8),
            Constraint::Length(1),
        ])
        .areas(f.area());

        f.render_widget(
            Line::from(vec![
                " SMB hours ".bold().reversed(),
                format!("  SMB: {}", self.smb).into(),
            ]),
            header,
        );

        let (title, text) = match &self.mode {
            Mode::Editing { buffer, .. } => {
                (" Edit entry (YYYY-MM-DD HH:MM-HH:MM) ", buffer.clone())
            }
            _ => (" What did you work? ", self.input.clone()),
        };
        let typing = matches!(self.mode, Mode::Input | Mode::Editing { .. });
        let cursor = if typing { "▏" } else { "" };
        f.render_widget(
            Paragraph::new(format!("{text}{cursor}")).block(
                Block::bordered().title(title).border_style(if typing {
                    Style::new().fg(Color::Cyan)
                } else {
                    Style::new()
                }),
            ),
            input,
        );

        f.render_widget(
            Paragraph::new(self.summary.as_str())
                .italic()
                .wrap(Wrap { trim: true }),
            summary,
        );

        let rows = self.rows.iter().map(|(e, s)| {
            let minutes = (e.end - e.start).num_minutes();
            let color = match s {
                Status::New => Color::Green,
                Status::Exists | Status::Added => Color::DarkGray,
                Status::Conflict(_) | Status::Failed(_) => Color::Red,
                Status::Unknown => Color::Yellow,
            };
            Row::new(vec![
                Cell::from(e.date.format("%a %d/%m").to_string()),
                Cell::from(fmt_time(e.start)),
                Cell::from(fmt_time(e.end)),
                Cell::from(format!("{}:{:02}", minutes / 60, minutes % 60)),
                Cell::from(s.label()).style(Style::new().fg(color)),
            ])
        });
        let total: i64 = self
            .rows
            .iter()
            .map(|(e, _)| (e.end - e.start).num_minutes())
            .sum();
        let table_widget = Table::new(
            rows,
            [
                Constraint::Length(11),
                Constraint::Length(6),
                Constraint::Length(6),
                Constraint::Length(6),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new(vec!["Date", "Start", "End", "Hours", "Status"])
                .style(Style::new().add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::new().reversed())
        .block(Block::bordered().title(format!(
            " Preview — total {}:{:02} ",
            total / 60,
            total % 60
        )));
        f.render_stateful_widget(table_widget, table, &mut self.table);

        let visible = log.height.saturating_sub(2) as usize;
        let lines: Vec<Line> = self
            .log
            .iter()
            .rev()
            .take(visible)
            .rev()
            .map(|l| Line::from(l.as_str()))
            .collect();
        f.render_widget(
            Paragraph::new(lines)
                .dim()
                .block(Block::bordered().title(" Log ")),
            log,
        );

        let keys = match self.mode {
            Mode::Input => "Enter parse · Esc back/quit · Ctrl-C quit",
            Mode::Parsing => "asking Claude…",
            Mode::Preview => {
                "Enter submit new rows · ↑↓ select · e edit · a add · d delete · r recheck · n new phrase · q quit"
            }
            Mode::Editing { .. } => "Enter save · Esc cancel",
            Mode::Submitting => "submitting to SMB…",
        };
        f.render_widget(Line::from(keys).dim(), help);
    }
}
