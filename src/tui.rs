//! Week-calendar TUI. Logged entries come from SMB in the background as you
//! move between weeks; new blocks are staged locally until you submit them.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::Result;
use chrono::{Datelike, Local, NaiveDate, NaiveTime, Timelike, Weekday};
use ratatui::Frame;
use ratatui::crossterm::event::{
    self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Alignment, Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph, Wrap};
use tokio::sync::mpsc;

use crate::ai::{self, Parsed};
use crate::config::Config;
use crate::model::{Entry, Status, classify, fmt_time, shift_time, week_start};
use crate::worker::{self, Cmd, Event};

const SMB_BLUE: Color = Color::Rgb(68, 68, 238);
const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

enum Msg {
    Key(KeyEvent),
    Tick,
    Worker(Event),
    Parsed(Result<Parsed, String>),
}

#[derive(PartialEq)]
enum Smb {
    Connecting,
    Ready,
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    Date,
    Start,
    End,
}

struct Editor {
    date: NaiveDate,
    start: NaiveTime,
    end: NaiveTime,
    field: Field,
    /// Index into `pending` when editing an existing block.
    index: Option<usize>,
    digits: String,
}

enum Mode {
    Normal,
    Phrase(String),
    Parsing,
    Editor(Editor),
    Confirm,
    Submitting,
    Help,
}

struct Pending {
    entry: Entry,
    error: Option<String>,
}

#[derive(Clone, PartialEq)]
enum Kind {
    Logged,
    Loading,
    New,
    Exists,
    Conflict(String),
    Failed(String),
}

struct DayBlock {
    start: NaiveTime,
    end: NaiveTime,
    kind: Kind,
    pending: Option<usize>,
}

impl Kind {
    fn style(&self) -> Style {
        match self {
            Kind::Logged => Style::new().bg(SMB_BLUE).fg(Color::White),
            Kind::Loading => Style::new().bg(Color::Yellow).fg(Color::Black),
            Kind::New => Style::new().bg(Color::Green).fg(Color::Black),
            Kind::Exists => Style::new().bg(Color::DarkGray).fg(Color::White),
            Kind::Conflict(_) | Kind::Failed(_) => Style::new().bg(Color::Red).fg(Color::White),
        }
    }

    fn label(&self) -> String {
        match self {
            Kind::Logged => "in SMB".into(),
            Kind::Loading => "new (checking…)".into(),
            Kind::New => "new".into(),
            Kind::Exists => "already in SMB".into(),
            Kind::Conflict(c) => format!("overlaps {c}"),
            Kind::Failed(e) => format!("failed: {e}"),
        }
    }

    fn accent(&self) -> Color {
        match self {
            Kind::Logged => Color::LightBlue,
            Kind::Loading => Color::Yellow,
            Kind::New => Color::Green,
            Kind::Exists => Color::Gray,
            Kind::Conflict(_) | Kind::Failed(_) => Color::Red,
        }
    }

    fn icon(&self) -> &'static str {
        match self {
            Kind::Logged => "✓",
            Kind::Loading | Kind::New => "+",
            Kind::Exists => "=",
            Kind::Conflict(_) | Kind::Failed(_) => "!",
        }
    }
}

struct App {
    cfg: Config,
    api_key: Option<String>,
    tx: mpsc::UnboundedSender<Msg>,
    cmds: mpsc::UnboundedSender<Cmd>,
    mode: Mode,
    smb: Smb,
    today: NaiveDate,
    cursor: NaiveDate,
    sel: usize,
    weeks: HashMap<NaiveDate, Vec<Entry>>,
    loading: HashSet<NaiveDate>,
    pending: Vec<Pending>,
    clipboard: Vec<(NaiveTime, NaiveTime)>,
    summary: String,
    flash: Option<String>,
    log: Vec<String>,
    tick: usize,
    quit_armed: bool,
    quit: bool,
}

pub async fn run(cfg: Config, api_key: Option<String>, headed: bool) -> Result<()> {
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
            let msg = if event::poll(Duration::from_millis(120)).unwrap_or(false) {
                match event::read() {
                    Ok(TermEvent::Key(k)) => Msg::Key(k),
                    _ => Msg::Tick,
                }
            } else {
                Msg::Tick
            };
            if keys.send(msg).is_err() {
                break;
            }
        }
    });

    let today = Local::now().date_naive();
    let mut app = App {
        cfg,
        api_key,
        tx,
        cmds: cmd_tx,
        mode: Mode::Normal,
        smb: Smb::Connecting,
        today,
        cursor: today,
        sel: 0,
        weeks: HashMap::new(),
        loading: HashSet::new(),
        pending: Vec::new(),
        clipboard: Vec::new(),
        summary: String::new(),
        flash: None,
        log: Vec::new(),
        tick: 0,
        quit_armed: false,
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
        app.handle(msg);
        if app.quit {
            break Ok(());
        }
    };
    ratatui::restore();
    result
}

fn days(n: i64) -> chrono::Duration {
    chrono::Duration::days(n)
}

fn duration(start: NaiveTime, end: NaiveTime) -> String {
    hours((end - start).num_minutes())
}

/// Durations read as `4h30` so they never look like a time of day.
fn hours(minutes: i64) -> String {
    match minutes % 60 {
        0 => format!("{}h", minutes / 60),
        m => format!("{}h{m:02}", minutes / 60),
    }
}

/// Pads or truncates to exactly `width` characters.
fn fit(s: &str, width: usize) -> String {
    let mut out: String = s.chars().take(width).collect();
    let len = out.chars().count();
    out.extend(std::iter::repeat_n(' ', width - len));
    out
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [row] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [cell] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(row);
    cell
}

impl App {
    // ---- state -----------------------------------------------------------

    fn push_log(&mut self, line: String) {
        self.log
            .push(format!("{} {line}", Local::now().format("%H:%M:%S")));
        if self.log.len() > 200 {
            self.log.remove(0);
        }
    }

    fn ensure_week(&mut self, date: NaiveDate) {
        let monday = week_start(date);
        if self.smb == Smb::Ready
            && !self.weeks.contains_key(&monday)
            && self.loading.insert(monday)
        {
            let _ = self.cmds.send(Cmd::LoadWeek(monday));
        }
    }

    fn reload_week(&mut self) {
        let monday = week_start(self.cursor);
        self.weeks.remove(&monday);
        self.loading.remove(&monday);
        self.ensure_week(monday);
    }

    fn logged(&self, date: NaiveDate) -> Option<Vec<Entry>> {
        self.weeks
            .get(&week_start(date))
            .map(|w| w.iter().filter(|e| e.date == date).cloned().collect())
    }

    fn pending_kind(&self, index: usize) -> Kind {
        let p = &self.pending[index];
        if let Some(err) = &p.error {
            return Kind::Failed(err.clone());
        }
        let Some(logged) = self.logged(p.entry.date) else {
            return Kind::Loading;
        };
        match classify(&p.entry, &logged) {
            Status::Exists => Kind::Exists,
            Status::Conflict(c) => Kind::Conflict(c),
            Status::New => {
                let clash = self
                    .pending
                    .iter()
                    .enumerate()
                    .any(|(i, other)| i != index && other.entry.overlaps(&p.entry));
                if clash {
                    Kind::Conflict("another new block".into())
                } else {
                    Kind::New
                }
            }
        }
    }

    fn day_blocks(&self, date: NaiveDate) -> Vec<DayBlock> {
        let mut blocks: Vec<DayBlock> = self
            .logged(date)
            .unwrap_or_default()
            .into_iter()
            .map(|e| DayBlock {
                start: e.start,
                end: e.end,
                kind: Kind::Logged,
                pending: None,
            })
            .collect();
        for (i, p) in self.pending.iter().enumerate() {
            if p.entry.date == date {
                blocks.push(DayBlock {
                    start: p.entry.start,
                    end: p.entry.end,
                    kind: self.pending_kind(i),
                    pending: Some(i),
                });
            }
        }
        blocks.sort_by_key(|b| (b.start, b.pending.is_some()));
        blocks
    }

    fn submittable(&self) -> Vec<usize> {
        (0..self.pending.len())
            .filter(|&i| self.pending_kind(i) == Kind::New)
            .collect()
    }

    /// Stages a block unless it would overlap something already on that day.
    fn stage(&mut self, entry: Entry) -> bool {
        let taken = self
            .day_blocks(entry.date)
            .iter()
            .any(|b| b.start < entry.end && entry.start < b.end);
        if taken || entry.validate().is_err() {
            return false;
        }
        self.pending.push(Pending { entry, error: None });
        true
    }

    fn fill_day(&mut self, date: NaiveDate) -> usize {
        self.cfg
            .default_blocks()
            .into_iter()
            .filter(|&(start, end)| self.stage(Entry { date, start, end }))
            .count()
    }

    fn move_cursor(&mut self, to: NaiveDate) {
        self.cursor = to;
        self.sel = 0;
        self.ensure_week(to);
    }

    fn selected_block(&self) -> Option<DayBlock> {
        self.day_blocks(self.cursor).into_iter().nth(self.sel)
    }

    // ---- events ----------------------------------------------------------

    fn handle(&mut self, msg: Msg) {
        match msg {
            Msg::Tick => self.tick = self.tick.wrapping_add(1),
            Msg::Key(key) if key.kind == KeyEventKind::Press => {
                self.flash = None;
                self.key(key);
                if !matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                    self.quit_armed = false;
                }
            }
            Msg::Key(_) => {}
            Msg::Parsed(Ok(parsed)) => {
                self.summary = parsed.summary;
                let first = parsed.entries.first().map(|e| e.date);
                let total = parsed.entries.len();
                let staged = parsed
                    .entries
                    .into_iter()
                    .filter(|e| self.stage(e.clone()))
                    .count();
                if staged < total {
                    self.flash = Some(format!(
                        "{} block(s) skipped: they overlap existing ones",
                        total - staged
                    ));
                }
                self.mode = Mode::Normal;
                if let Some(date) = first {
                    self.move_cursor(date);
                }
            }
            Msg::Parsed(Err(e)) => {
                self.summary = format!("error: {e}");
                self.mode = Mode::Normal;
            }
            Msg::Worker(ev) => self.worker(ev),
        }
    }

    fn worker(&mut self, ev: Event) {
        match ev {
            Event::Log(line) => self.push_log(line),
            Event::Ready => {
                self.smb = Smb::Ready;
                self.ensure_week(self.cursor);
            }
            Event::LoginFailed(e) => {
                self.push_log(format!("login failed: {e}"));
                self.smb = Smb::Failed(e);
                self.loading.clear();
            }
            Event::Week(monday, entries) => {
                if self.smb != Smb::Ready {
                    self.smb = Smb::Ready;
                }
                self.loading.remove(&monday);
                self.weeks.insert(monday, entries);
            }
            Event::WeekFailed(monday, e) => {
                self.loading.remove(&monday);
                self.push_log(format!("could not load week of {monday}: {e}"));
            }
            Event::Submitted(outcome) => {
                for (entry, err) in outcome {
                    if let Some(i) = self.pending.iter().position(|p| p.entry == entry) {
                        match err {
                            None => {
                                self.pending.remove(i);
                            }
                            Some(e) => self.pending[i].error = Some(e),
                        }
                    }
                }
                self.sel = 0;
                self.mode = Mode::Normal;
            }
        }
    }

    fn key(&mut self, key: KeyEvent) {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        match std::mem::replace(&mut self.mode, Mode::Normal) {
            Mode::Normal => self.normal_key(key),
            Mode::Phrase(input) => self.phrase_key(key, input),
            Mode::Editor(editor) => self.editor_key(key, editor),
            Mode::Confirm => {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char('y')) {
                    let entries: Vec<Entry> = self
                        .submittable()
                        .into_iter()
                        .map(|i| self.pending[i].entry.clone())
                        .collect();
                    let _ = self.cmds.send(Cmd::Submit(entries));
                    self.mode = Mode::Submitting;
                }
            }
            Mode::Help => {}
            m @ (Mode::Parsing | Mode::Submitting) => self.mode = m,
        }
    }

    fn normal_key(&mut self, key: KeyEvent) {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Left if shift => self.move_cursor(self.cursor - days(7)),
            KeyCode::Right if shift => self.move_cursor(self.cursor + days(7)),
            KeyCode::Left | KeyCode::Char('h') => self.move_cursor(self.cursor - days(1)),
            KeyCode::Right | KeyCode::Char('l') => self.move_cursor(self.cursor + days(1)),
            KeyCode::Char('[') | KeyCode::PageUp | KeyCode::Char('H') => {
                self.move_cursor(self.cursor - days(7))
            }
            KeyCode::Char(']') | KeyCode::PageDown | KeyCode::Char('L') => {
                self.move_cursor(self.cursor + days(7))
            }
            KeyCode::Char('t') => self.move_cursor(self.today),
            KeyCode::Up | KeyCode::Char('k') => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => {
                let n = self.day_blocks(self.cursor).len();
                self.sel = (self.sel + 1).min(n.saturating_sub(1));
            }
            KeyCode::Char('a') => self.open_editor(None),
            KeyCode::Char('e') | KeyCode::Enter => match self.selected_block() {
                Some(DayBlock {
                    pending: Some(i), ..
                }) => self.open_editor(Some(i)),
                Some(_) => {
                    self.flash =
                        Some("that block is already in SMB — change it on the website".into())
                }
                None => self.open_editor(None),
            },
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace => {
                match self.selected_block() {
                    Some(DayBlock {
                        pending: Some(i), ..
                    }) => {
                        self.pending.remove(i);
                        self.sel = self.sel.saturating_sub(1);
                    }
                    Some(_) => {
                        self.flash =
                            Some("that block is already in SMB — remove it on the website".into())
                    }
                    None => {}
                }
            }
            KeyCode::Char('D') => {
                let day = self.cursor;
                self.pending.retain(|p| p.entry.date != day);
                self.sel = 0;
            }
            KeyCode::Char('n') => {
                let n = self.fill_day(self.cursor);
                self.flash = Some(match n {
                    0 => "this day already has those blocks".into(),
                    n => format!("staged {n} block(s) from your normal day"),
                });
            }
            KeyCode::Char('w') => {
                let monday = week_start(self.cursor);
                let mut n = 0;
                for d in 0..5 {
                    let date = monday + days(d);
                    if self.day_blocks(date).is_empty() {
                        n += self.fill_day(date);
                    }
                }
                self.flash = Some(format!("staged {n} block(s) on empty weekdays"));
            }
            KeyCode::Char('y') => {
                self.clipboard = self
                    .day_blocks(self.cursor)
                    .iter()
                    .map(|b| (b.start, b.end))
                    .collect();
                self.flash = Some(format!("copied {} block(s)", self.clipboard.len()));
            }
            KeyCode::Char('p') => {
                let date = self.cursor;
                let n = self
                    .clipboard
                    .clone()
                    .into_iter()
                    .filter(|&(start, end)| self.stage(Entry { date, start, end }))
                    .count();
                self.flash = Some(format!("pasted {n} block(s)"));
            }
            KeyCode::Char('/') | KeyCode::Char('i') => self.mode = Mode::Phrase(String::new()),
            KeyCode::Char('s') => {
                if self.smb != Smb::Ready {
                    self.flash = Some("SMB is not logged in yet".into());
                } else if self.submittable().is_empty() {
                    self.flash = Some("nothing new to submit".into());
                } else {
                    self.mode = Mode::Confirm;
                }
            }
            KeyCode::Char('r') => {
                if let Smb::Failed(_) = self.smb {
                    // Any command makes the worker try to log in again.
                    self.smb = Smb::Connecting;
                    let monday = week_start(self.cursor);
                    self.weeks.remove(&monday);
                    self.loading.insert(monday);
                    let _ = self.cmds.send(Cmd::LoadWeek(monday));
                } else {
                    self.reload_week();
                }
            }
            KeyCode::Char('?') => self.mode = Mode::Help,
            KeyCode::Char('q') | KeyCode::Esc => {
                let unsaved = !self.submittable().is_empty();
                if unsaved && !self.quit_armed {
                    self.quit_armed = true;
                    self.flash = Some("you have unsubmitted blocks — press q again to quit".into());
                } else {
                    self.quit = true;
                }
            }
            _ => {}
        }
    }

    fn phrase_key(&mut self, key: KeyEvent, mut input: String) {
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter if !input.trim().is_empty() => {
                self.mode = Mode::Parsing;
                self.summary = String::new();
                let (cfg, api_key, tx, today) = (
                    self.cfg.clone(),
                    self.api_key.clone(),
                    self.tx.clone(),
                    self.today,
                );
                tokio::spawn(async move {
                    let result = ai::parse(&cfg, api_key.as_deref(), &input, today)
                        .await
                        .map_err(|e| format!("{e:#}"));
                    let _ = tx.send(Msg::Parsed(result));
                });
                return;
            }
            KeyCode::Char(c) => input.push(c),
            KeyCode::Backspace => {
                input.pop();
            }
            _ => {}
        }
        self.mode = Mode::Phrase(input);
    }

    fn open_editor(&mut self, index: Option<usize>) {
        let (date, start, end) = match index {
            Some(i) => {
                let e = &self.pending[i].entry;
                (e.date, e.start, e.end)
            }
            None => {
                let blocks = self.day_blocks(self.cursor);
                // Suggest the first block of the normal day that is still free,
                // otherwise an hour right after the last block.
                let free = self
                    .cfg
                    .default_blocks()
                    .into_iter()
                    .find(|&(s, e)| !blocks.iter().any(|b| b.start < e && s < b.end));
                let (start, end) = match (free, blocks.last()) {
                    (Some(block), _) => block,
                    (None, Some(last)) => (last.end, shift_time(last.end, 60)),
                    (None, None) => (
                        NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
                        NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
                    ),
                };
                (self.cursor, start, end)
            }
        };
        self.mode = Mode::Editor(Editor {
            date,
            start,
            end,
            field: Field::Start,
            index,
            digits: String::new(),
        });
    }

    fn editor_key(&mut self, key: KeyEvent, mut ed: Editor) {
        let big = key.modifiers.contains(KeyModifiers::SHIFT);
        let step = |ed: &mut Editor, dir: i64, big: bool| match ed.field {
            Field::Date => ed.date += days(dir * if big { 7 } else { 1 }),
            Field::Start => ed.start = shift_time(ed.start, dir * if big { 60 } else { 15 }),
            Field::End => ed.end = shift_time(ed.end, dir * if big { 60 } else { 15 }),
        };
        match key.code {
            KeyCode::Esc => return,
            KeyCode::Enter => {
                let entry = Entry {
                    date: ed.date,
                    start: ed.start,
                    end: ed.end,
                };
                if let Err(e) = entry.validate() {
                    self.flash = Some(format!("{e}"));
                } else {
                    let previous = ed.index.map(|i| self.pending.remove(i));
                    if self.stage(entry) {
                        self.move_cursor(ed.date);
                        return;
                    }
                    if let (Some(p), Some(i)) = (previous, ed.index) {
                        self.pending.insert(i, p);
                    }
                    self.flash = Some("that overlaps a block already on this day".into());
                }
            }
            KeyCode::Tab | KeyCode::Right => {
                ed.digits.clear();
                ed.field = match ed.field {
                    Field::Date => Field::Start,
                    Field::Start => Field::End,
                    Field::End => Field::Date,
                };
            }
            KeyCode::BackTab | KeyCode::Left => {
                ed.digits.clear();
                ed.field = match ed.field {
                    Field::Date => Field::End,
                    Field::Start => Field::Date,
                    Field::End => Field::Start,
                };
            }
            KeyCode::Up | KeyCode::Char('k') => step(&mut ed, 1, big),
            KeyCode::Down | KeyCode::Char('j') => step(&mut ed, -1, big),
            KeyCode::PageUp => step(&mut ed, 1, true),
            KeyCode::PageDown => step(&mut ed, -1, true),
            KeyCode::Char(c) if c.is_ascii_digit() && ed.field != Field::Date => {
                ed.digits.push(c);
                if ed.digits.len() == 4 {
                    let (h, m) = ed.digits.split_at(2);
                    match NaiveTime::from_hms_opt(h.parse().unwrap(), m.parse().unwrap(), 0) {
                        Some(t) if ed.field == Field::Start => {
                            ed.start = t;
                            ed.field = Field::End;
                        }
                        Some(t) => ed.end = t,
                        None => self.flash = Some(format!("{h}:{m} is not a time")),
                    }
                    ed.digits.clear();
                }
            }
            KeyCode::Backspace => {
                ed.digits.pop();
            }
            _ => {}
        }
        self.mode = Mode::Editor(ed);
    }

    // ---- drawing ---------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let [top, body, ask, log, help] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(12),
            Constraint::Length(3),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .areas(f.area());
        let [side, grid] =
            Layout::horizontal([Constraint::Length(26), Constraint::Min(40)]).areas(body);
        let [month, day] =
            Layout::vertical([Constraint::Length(9), Constraint::Min(4)]).areas(side);

        self.draw_top(f, top);
        self.draw_month(f, month);
        self.draw_day(f, day);
        self.draw_week(f, grid);
        self.draw_ask(f, ask);
        self.draw_log(f, log);
        self.draw_hints(f, help);

        match &self.mode {
            Mode::Editor(ed) => self.draw_editor(f, ed),
            Mode::Confirm => self.draw_confirm(f),
            Mode::Help => draw_help(f),
            _ => {}
        }
    }

    fn spinner(&self) -> &'static str {
        SPINNER[self.tick % SPINNER.len()]
    }

    fn draw_top(&self, f: &mut Frame, area: Rect) {
        let monday = week_start(self.cursor);
        let sunday = monday + days(6);
        let (dot, state) = match &self.smb {
            Smb::Connecting => (Span::from(self.spinner()).yellow(), "logging in to SMB"),
            Smb::Ready if !self.loading.is_empty() => {
                (Span::from(self.spinner()).yellow(), "loading week")
            }
            Smb::Ready => (Span::from("●").green(), "connected"),
            Smb::Failed(_) => (Span::from("✗").red(), "login failed — r to retry"),
        };
        let week_logged: i64 = self
            .weeks
            .get(&monday)
            .map(|w| w.iter().map(|e| (e.end - e.start).num_minutes()).sum())
            .unwrap_or(0);
        let week_new: i64 = self
            .submittable()
            .into_iter()
            .map(|i| &self.pending[i].entry)
            .filter(|e| week_start(e.date) == monday)
            .map(|e| (e.end - e.start).num_minutes())
            .sum();
        let target: i64 = self
            .cfg
            .default_blocks()
            .iter()
            .map(|&(s, e)| (e - s).num_minutes())
            .sum::<i64>()
            * 5;
        let left = Line::from(vec![
            " SMB hours ".bold().fg(Color::White).bg(SMB_BLUE),
            "  ".into(),
            dot,
            Span::from(format!(" {state}")).dim(),
            "    ".into(),
            format!("Week {}", self.cursor.iso_week().week()).bold(),
            format!(
                "  {} – {}",
                monday.format("%d %b"),
                sunday.format("%d %b %Y")
            )
            .into(),
        ]);
        let mut right = vec![
            "logged ".dim(),
            hours(week_logged).fg(Color::LightBlue).bold(),
            "  new ".dim(),
            hours(week_new).green().bold(),
        ];
        if target > 0 && area.width >= 100 {
            right.push(format!(" / {}  ", hours(target)).dim());
            right.extend(progress(week_logged, week_new, target, 12));
        }
        right.push(" ".into());
        let right = Line::from(right).right_aligned();
        let [left_area, _] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(right.width() as u16 + 1),
        ])
        .areas(area);
        f.render_widget(left, left_area);
        f.render_widget(right, area);
    }

    fn draw_month(&self, f: &mut Frame, area: Rect) {
        let first = self.cursor.with_day(1).unwrap();
        let cursor_week = week_start(self.cursor);
        let mut lines = vec![Line::from("  Mo Tu We Th Fr Sa Su").dim()];
        let mut date = week_start(first);
        while date.month() == first.month() || date < first {
            let mut spans = vec![if date == cursor_week {
                "▸".cyan()
            } else {
                " ".into()
            }];
            for _ in 0..7 {
                let mut style = Style::new();
                if date.month() != first.month() {
                    style = style.fg(Color::DarkGray);
                } else if matches!(date.weekday(), Weekday::Sat | Weekday::Sun) {
                    style = style.fg(Color::Gray).add_modifier(Modifier::DIM);
                }
                if self.logged(date).is_some_and(|l| !l.is_empty()) {
                    style = style.fg(Color::LightBlue);
                }
                if self.pending.iter().any(|p| p.entry.date == date) {
                    style = style.fg(Color::Green).add_modifier(Modifier::BOLD);
                }
                if date == self.today {
                    style = style.add_modifier(Modifier::UNDERLINED | Modifier::BOLD);
                }
                if date == self.cursor {
                    style = style.add_modifier(Modifier::REVERSED | Modifier::BOLD);
                }
                spans.push(" ".into());
                spans.push(Span::styled(format!("{:>2}", date.day()), style));
                date += days(1);
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(
            Paragraph::new(lines).centered().block(
                panel(format!(" {} ", first.format("%B %Y"))).title_alignment(Alignment::Center),
            ),
            area,
        );
    }

    fn draw_day(&self, f: &mut Frame, area: Rect) {
        let blocks = self.day_blocks(self.cursor);
        let total: i64 = blocks.iter().map(|b| (b.end - b.start).num_minutes()).sum();
        let mut title = vec![Span::from(format!(" {} ", self.cursor.format("%A %d %b")))];
        if self.cursor == self.today {
            title.push("today ".cyan());
        }
        let block = panel(title).title_bottom(
            Line::from(vec![" total ".dim(), hours(total).bold(), " ".into()]).right_aligned(),
        );
        let inner = block.inner(area);
        f.render_widget(block, area);
        let [list, key] = if inner.height >= 6 {
            Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner)
        } else {
            [inner, Rect::default()]
        };

        let mut lines: Vec<Line> = Vec::new();
        if self.logged(self.cursor).is_none() {
            let msg = match self.smb {
                Smb::Ready => format!(" {} loading from SMB…", self.spinner()),
                _ => " waiting for SMB…".to_string(),
            };
            lines.push(Line::from(msg).dim());
        }
        for (i, b) in blocks.iter().enumerate() {
            let selected = i == self.sel;
            let time = Span::from(format!(" {}–{} ", fmt_time(b.start), fmt_time(b.end)));
            let length = Span::from(format!("{:>6} ", duration(b.start, b.end)));
            let (time, length) = if selected {
                (time.reversed().bold(), length.reversed())
            } else {
                (time, length.dim())
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", b.kind.icon()), b.kind.style()),
                time,
                length,
            ]));
            if selected {
                lines.push(Line::from(format!("   ↳ {}", b.kind.label())).fg(b.kind.accent()));
            }
        }
        if blocks.is_empty() && self.logged(self.cursor).is_some() {
            lines.push(Line::from(" nothing logged yet").dim());
            lines.push(Line::from(vec![
                " ".into(),
                "a".cyan().bold(),
                " add  ".dim(),
                "n".cyan().bold(),
                " normal day".dim(),
            ]));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), list);
        f.render_widget(Paragraph::new(legend(" ").to_vec()), key);
    }

    fn draw_week(&self, f: &mut Frame, area: Rect) {
        let outer = panel(" Week ");
        let inner = outer.inner(area);
        f.render_widget(outer, area);

        let monday = week_start(self.cursor);
        let week: Vec<(NaiveDate, Vec<DayBlock>)> = (0..7)
            .map(|d| monday + days(d))
            .map(|d| (d, self.day_blocks(d)))
            .collect();

        let (mut first_hour, mut last_hour) = (8, 18);
        for (_, blocks) in &week {
            for b in blocks {
                first_hour = first_hour.min(b.start.hour());
                last_hour = last_hour.max(b.end.hour() + u32::from(b.end.minute() > 0));
            }
        }
        // Header, totals and a rule take three rows; the day gets the rest.
        let rows = inner.height.saturating_sub(3) as u32;
        let slot_minutes: u32 = [15, 30, 60]
            .into_iter()
            .find(|m| (last_hour - first_hour) * 60 / m <= rows)
            .unwrap_or(60);
        // Show a little more of the morning and evening when rows are spare.
        let per_hour = 60 / slot_minutes;
        let mut later = true;
        while (last_hour - first_hour + 1) * per_hour <= rows && (first_hour > 6 || last_hour < 21)
        {
            if (later && last_hour < 21) || first_hour <= 6 {
                last_hour += 1;
            } else {
                first_hour -= 1;
            }
            later = !later;
        }
        let slots = (last_hour - first_hour) * per_hour;

        let gutter = 6usize;
        let col_w = ((inner.width as usize).saturating_sub(gutter) / 7).max(4);
        let cell_w = col_w - 1;

        let mut header = vec![Span::from(" ".repeat(gutter))];
        let mut totals = vec![Span::from(" ".repeat(gutter))];
        for (date, blocks) in &week {
            let weekend = matches!(date.weekday(), Weekday::Sat | Weekday::Sun);
            let mut style = Style::new().bold();
            if weekend {
                style = style.dim();
            }
            if *date == self.today {
                style = style.cyan();
            }
            if *date == self.cursor {
                style = Style::new().bold().black().on_cyan();
            }
            let mut label = date.format(" %a %d").to_string();
            if *date == self.today {
                label.push_str(" •");
            }
            header.push(Span::styled(fit(&label, cell_w), style));
            header.push(" ".into());

            let minutes: i64 = blocks.iter().map(|b| (b.end - b.start).num_minutes()).sum();
            let text = if minutes > 0 {
                format!(" {}", hours(minutes))
            } else {
                String::new()
            };
            let mut style = if blocks.iter().any(|b| b.pending.is_some()) {
                Style::new().green()
            } else {
                Style::new().fg(Color::LightBlue)
            };
            if weekend {
                style = style.dim();
            }
            totals.push(Span::styled(fit(&text, cell_w), style));
            totals.push(" ".into());
        }
        let rule = Line::from("─".repeat(inner.width as usize)).fg(Color::DarkGray);
        let mut lines = vec![Line::from(header), Line::from(totals), rule];

        let now = Local::now();
        let now = (now.date_naive() == self.today).then(|| now.time());
        let minute_of = |t: NaiveTime| t.hour() * 60 + t.minute();
        for slot in 0..slots {
            let minute = first_hour * 60 + slot * slot_minutes;
            let at = NaiveTime::from_hms_opt(minute / 60, minute % 60, 0).unwrap();
            let slot_end = at + chrono::Duration::minutes(slot_minutes as i64);
            let on_hour = minute.is_multiple_of(60);
            let label = if on_hour { fmt_time(at) } else { String::new() };
            let mut spans = vec![Span::from(fit(&label, gutter)).fg(Color::DarkGray)];
            for (date, blocks) in &week {
                let hit = blocks
                    .iter()
                    .enumerate()
                    .find(|(_, b)| b.start < slot_end && b.end > at);
                let span = match hit {
                    Some((i, b)) => {
                        let row = slot
                            .saturating_sub((minute_of(b.start) - first_hour * 60) / slot_minutes);
                        let (start, end) = (fmt_time(b.start), fmt_time(b.end));
                        let length = duration(b.start, b.end);
                        // Wide columns fit the range on one row; narrow ones stack it.
                        let text = match (row, cell_w) {
                            (0, 13..) => format!("{start}–{end}"),
                            (0, _) => start,
                            (1, 7..=12) => format!("–{end}"),
                            (1, ..=6) => end,
                            (1, 13..) | (2, 7..) => format!("{} {length}", b.kind.icon()),
                            (2, _) => length,
                            _ => String::new(),
                        };
                        let selected = *date == self.cursor && i == self.sel;
                        let mut style = b.kind.style();
                        if row == 0 || selected {
                            style = style.add_modifier(Modifier::BOLD);
                        }
                        let edge = match (cell_w, selected) {
                            (..=6, true) => {
                                style = style.add_modifier(Modifier::UNDERLINED);
                                ""
                            }
                            (..=6, false) => "",
                            (_, true) => "▌",
                            (_, false) => " ",
                        };
                        Span::styled(fit(&format!("{edge}{text}"), cell_w), style)
                    }
                    None if *date == self.today && now.is_some_and(|t| at <= t && t < slot_end) => {
                        Span::from("━".repeat(cell_w)).red()
                    }
                    None if on_hour => {
                        let style = if *date == self.cursor {
                            Style::new().cyan().dim()
                        } else {
                            Style::new().fg(Color::DarkGray)
                        };
                        Span::styled("┈".repeat(cell_w), style)
                    }
                    None => Span::from(" ".repeat(cell_w)),
                };
                spans.push(span);
                spans.push(" ".into());
            }
            lines.push(Line::from(spans));
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn draw_ask(&self, f: &mut Frame, area: Rect) {
        let ai = format!(" {} ", self.cfg.provider.label());
        let (line, title) = match &self.mode {
            Mode::Phrase(input) => (
                Line::from(vec!["› ".cyan().bold(), input.clone().into(), "▏".cyan()]),
                " Describe your hours — e.g. \"normal week but Friday off\" ".to_string(),
            ),
            Mode::Parsing => (
                Line::from(format!("{} thinking…", self.spinner())).dim(),
                ai,
            ),
            Mode::Submitting => (
                Line::from(format!("{} submitting to SMB…", self.spinner())).yellow(),
                " SMB ".to_string(),
            ),
            _ if self.summary.starts_with("error:") => (Line::from(self.summary.clone()).red(), ai),
            _ if !self.summary.is_empty() => (
                Line::from(vec!["✦ ".cyan(), self.summary.clone().italic()]),
                ai,
            ),
            _ => (
                Line::from(vec![
                    "press ".dim(),
                    "/".cyan().bold(),
                    " to describe your hours in plain English".dim(),
                ]),
                ai,
            ),
        };
        let mut block = panel(title).padding(Padding::horizontal(1));
        if matches!(self.mode, Mode::Phrase(_)) {
            block = block.border_style(Style::new().cyan());
        }
        f.render_widget(Paragraph::new(line).block(block), area);
    }

    fn draw_log(&self, f: &mut Frame, area: Rect) {
        let visible = area.height.saturating_sub(2) as usize;
        let lines: Vec<Line> = self
            .log
            .iter()
            .rev()
            .take(visible)
            .rev()
            .map(|l| {
                let (time, msg) = l.split_once(' ').unwrap_or(("", l));
                let style = if msg.contains("fail") || msg.contains("error") {
                    Style::new().red()
                } else {
                    Style::new().fg(Color::Gray)
                };
                Line::from(vec![
                    Span::from(format!("{time}  ")).fg(Color::DarkGray),
                    Span::styled(msg.to_string(), style),
                ])
            })
            .collect();
        f.render_widget(
            Paragraph::new(lines).block(panel(" Log ").padding(Padding::horizontal(1))),
            area,
        );
    }

    fn draw_hints(&self, f: &mut Frame, area: Rect) {
        let line = if let Some(msg) = &self.flash {
            let style = if self.quit_armed {
                Style::new().red().bold()
            } else {
                Style::new().yellow()
            };
            Line::from(Span::styled(format!(" ● {msg}"), style))
        } else {
            match self.mode {
                Mode::Normal => {
                    let mut pairs = vec![
                        ("←→", "day"),
                        ("[ ]", "week"),
                        ("↑↓", "block"),
                        ("a", "add"),
                        ("s", "submit"),
                        ("/", "ask AI"),
                        ("n", "normal day"),
                        ("e", "edit"),
                        ("d", "delete"),
                        ("w", "fill week"),
                        ("y/p", "copy/paste"),
                        ("t", "today"),
                        ("?", "help"),
                    ];
                    // On narrow terminals drop the extras but keep "? help" visible.
                    while pairs.len() > 1 && key_hints(&pairs).width() > area.width as usize {
                        pairs.remove(pairs.len() - 2);
                    }
                    key_hints(&pairs)
                }
                Mode::Phrase(_) => key_hints(&[("Enter", "send"), ("Esc", "cancel")]),
                Mode::Editor(_) => key_hints(&[
                    ("Tab/←→", "field"),
                    ("↑↓", "±15 min"),
                    ("⇧↑↓", "±1 h"),
                    ("0830", "type a time"),
                    ("Enter", "save"),
                    ("Esc", "cancel"),
                ]),
                Mode::Confirm => key_hints(&[("Enter/y", "submit"), ("any key", "cancel")]),
                Mode::Help => key_hints(&[("any key", "close")]),
                Mode::Parsing | Mode::Submitting => Line::default(),
            }
        };
        f.render_widget(line, area);
    }

    fn draw_editor(&self, f: &mut Frame, ed: &Editor) {
        let area = centered(f.area(), 46, 11);
        f.render_widget(Clear, area);
        let field = |name: &str, value: String, this: Field| {
            let active = ed.field == this;
            let value = if active && !ed.digits.is_empty() {
                format!("{:<5}", format!("{}▏", ed.digits))
            } else {
                value
            };
            let shown = if active {
                Span::from(format!(" ‹ {value} › "))
                    .black()
                    .on_cyan()
                    .bold()
            } else {
                Span::from(format!("   {value}   "))
            };
            Line::from(vec![Span::from(format!("  {name:<8}")).dim(), shown])
        };
        let clash = self.day_blocks(ed.date).into_iter().find(|b| {
            !(b.pending.is_some() && b.pending == ed.index) && b.start < ed.end && ed.start < b.end
        });
        let status = if ed.start >= ed.end {
            Span::from("end must be after start").red()
        } else if let Some(b) = clash {
            Span::from(format!(
                "overlaps {}–{}",
                fmt_time(b.start),
                fmt_time(b.end)
            ))
            .red()
        } else {
            Span::from(duration(ed.start, ed.end)).green().bold()
        };
        let lines = vec![
            Line::from(""),
            field(
                "Date",
                ed.date.format("%a %d %b %Y").to_string(),
                Field::Date,
            ),
            field("Start", fmt_time(ed.start), Field::Start),
            field("End", fmt_time(ed.end), Field::End),
            Line::from(""),
            Line::from(vec![Span::from("  Length    ").dim(), status]),
            Line::from(""),
            key_hints(&[("↑↓", "adjust"), ("Tab", "next"), ("Enter", "save")]),
        ];
        let title = if ed.index.is_some() {
            " Edit block "
        } else {
            " New block "
        };
        f.render_widget(
            Paragraph::new(lines).block(panel(title).border_style(Style::new().cyan())),
            area,
        );
    }

    fn draw_confirm(&self, f: &mut Frame) {
        let items = self.submittable();
        let height = (items.len() as u16 + 7).min(f.area().height.saturating_sub(2));
        let area = centered(f.area(), 46, height);
        f.render_widget(Clear, area);
        let total: i64 = items
            .iter()
            .map(|&i| &self.pending[i].entry)
            .map(|e| (e.end - e.start).num_minutes())
            .sum();
        let mut lines = vec![Line::from("")];
        let mut last = None;
        for &i in &items {
            let e = &self.pending[i].entry;
            let day = if last == Some(e.date) {
                String::new()
            } else {
                e.date.format("%a %d %b").to_string()
            };
            last = Some(e.date);
            lines.push(Line::from(vec![
                Span::from(format!("  {day:<12}")).bold(),
                Span::styled(" + ", Kind::New.style()),
                format!(" {}–{} ", fmt_time(e.start), fmt_time(e.end)).into(),
                format!("{:>6}", duration(e.start, e.end)).dim(),
            ]));
        }
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::from(format!("  {:<29}", "Total")).dim(),
            format!("{:>6}", hours(total)).green().bold(),
        ]));
        lines.push(Line::from(""));
        lines.push(key_hints(&[("Enter/y", "submit"), ("any key", "cancel")]));
        f.render_widget(
            Paragraph::new(lines).block(
                panel(format!(" Submit {} block(s) to SMB? ", items.len()))
                    .border_style(Style::new().green()),
            ),
            area,
        );
    }
}

/// A rounded panel with a quiet border, so the content carries the colour.
fn panel<'a>(title: impl Into<Line<'a>>) -> Block<'a> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(Color::DarkGray))
        .title(title.into().bold())
}

/// `key description` pairs with the keys picked out.
fn key_hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::from(" ")];
    for (key, what) in pairs {
        spans.push(Span::from(key.to_string()).cyan().bold());
        spans.push(Span::from(format!(" {what}   ")).dim());
    }
    Line::from(spans)
}

/// What the block colours mean.
fn legend(indent: &str) -> [Line<'static>; 2] {
    let chip = |kind: Kind, text: &'static str| {
        vec![
            Span::styled(format!(" {} ", kind.icon()), kind.style()),
            Span::from(text).dim(),
        ]
    };
    let row = |a: Vec<Span<'static>>, b: Vec<Span<'static>>| {
        Line::from([vec![Span::from(indent.to_string())], a, b].concat())
    };
    [
        row(chip(Kind::Logged, " in SMB  "), chip(Kind::New, " new")),
        row(
            chip(Kind::Exists, " exists  "),
            chip(Kind::Conflict(String::new()), " clash"),
        ),
    ]
}

/// Logged hours in blue, staged ones in green, the rest of the target empty.
fn progress(logged: i64, new: i64, target: i64, width: usize) -> Vec<Span<'static>> {
    let cells = |minutes: i64| ((minutes * width as i64 / target) as usize).min(width);
    let blue = cells(logged);
    let green = cells(logged + new) - blue;
    vec![
        Span::from("━".repeat(blue)).fg(Color::LightBlue),
        Span::from("━".repeat(green)).green(),
        Span::from("─".repeat(width - blue - green)).fg(Color::DarkGray),
    ]
}

fn draw_help(f: &mut Frame) {
    let sections: [(&str, &[(&str, &str)]); 3] = [
        (
            "Move around",
            &[
                ("←/→  h/l", "previous / next day"),
                ("[ ]  ⇧←/→  PgUp/PgDn", "previous / next week"),
                ("t", "jump to today"),
                ("↑/↓  k/j", "select a block in the day"),
            ],
        ),
        (
            "Stage blocks",
            &[
                ("a", "add a block (time picker)"),
                ("e / Enter", "edit the selected new block"),
                ("d / Del", "delete the selected new block"),
                ("D", "clear all new blocks on this day"),
                ("n", "stage your normal day on this day"),
                ("w", "stage your normal day on every empty weekday"),
                ("y / p", "copy this day's blocks / paste onto this day"),
                ("/  i", "describe hours in plain English (AI)"),
            ],
        ),
        (
            "SMB",
            &[
                ("s", "review and submit new blocks to SMB"),
                ("r", "reload this week from SMB (or retry login)"),
                ("q / Esc", "quit"),
            ],
        ),
    ];
    let rows: usize = sections.iter().map(|(_, keys)| keys.len() + 2).sum();
    let area = centered(f.area(), 66, rows as u16 + 5);
    f.render_widget(Clear, area);
    let mut lines = vec![Line::from("")];
    for (title, keys) in sections {
        lines.push(Line::from(format!("  {title}")).bold());
        for (k, v) in keys {
            lines.push(Line::from(vec![
                Span::from(format!("    {k:<22}")).cyan(),
                Span::from(*v),
            ]));
        }
        lines.push(Line::from(""));
    }
    lines.extend(legend("    "));
    f.render_widget(
        Paragraph::new(lines).block(
            panel(" Keys ")
                .border_style(Style::new().cyan())
                .title_bottom(Line::from(" any key to close ").dim().right_aligned()),
        ),
        area,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn app() -> App {
        let (tx, _) = mpsc::unbounded_channel();
        let (cmds, _) = mpsc::unbounded_channel();
        let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let t = |h, m| NaiveTime::from_hms_opt(h, m, 0).unwrap();
        let logged = vec![
            Entry {
                date: today - days(1),
                start: t(8, 0),
                end: t(12, 30),
            },
            Entry {
                date: today - days(1),
                start: t(13, 0),
                end: t(17, 0),
            },
            Entry {
                date: today,
                start: t(8, 0),
                end: t(12, 30),
            },
        ];
        let mut app = App {
            cfg: Config::new("acme".into(), "SMB".into(), None),
            api_key: None,
            tx,
            cmds,
            mode: Mode::Normal,
            smb: Smb::Ready,
            today,
            cursor: today,
            sel: 0,
            weeks: HashMap::from([(week_start(today), logged)]),
            loading: HashSet::new(),
            pending: Vec::new(),
            clipboard: Vec::new(),
            summary: String::new(),
            flash: None,
            log: vec!["10:00:00 logged in".into()],
            tick: 0,
            quit_armed: false,
            quit: false,
        };
        app.fill_day(today);
        app
    }

    fn render(app: &mut App) -> String {
        let mut terminal = Terminal::new(TestBackend::new(150, 44)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        render_buf(&terminal)
    }

    fn render_buf(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn normal_day_only_stages_the_missing_block() {
        let app = app();
        assert_eq!(app.pending.len(), 1);
        assert_eq!(
            app.pending[0].entry.start,
            NaiveTime::from_hms_opt(13, 0, 0).unwrap()
        );
        assert_eq!(app.submittable(), vec![0]);
    }

    #[test]
    fn renders_week_and_editor() {
        let mut app = app();
        let screen = render(&mut app);
        println!("{screen}");
        assert!(screen.contains("Week 43"));
        assert!(screen.contains("13:00–17:00"));
        app.open_editor(None);
        let screen = render(&mut app);
        println!("{screen}");
        assert!(screen.contains("New block"));
    }

    #[test]
    fn renders_in_a_small_terminal() {
        let mut app = app();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| app.draw(f)).unwrap();
        println!("{}", render_buf(&terminal));
        app.mode = Mode::Help;
        terminal.draw(|f| app.draw(f)).unwrap();
        app.mode = Mode::Confirm;
        terminal.draw(|f| app.draw(f)).unwrap();
    }
}
