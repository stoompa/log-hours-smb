//! Background task that owns the browser session, so the TUI never blocks on SMB.

use chrono::NaiveDate;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::config::Config;
use crate::model::{Entry, Status, classify, week_start};
use crate::smb::Smb;

pub enum Cmd {
    /// Compare entries with what SMB already holds.
    Check(Vec<Entry>),
    /// Add these entries; `usize` is the preview row each one came from.
    Submit(Vec<(usize, Entry)>),
}

pub enum Event {
    Log(String),
    Ready,
    LoginFailed(String),
    Statuses(Vec<Status>),
    Submitted(Vec<(usize, Status)>),
}

pub async fn run(
    cfg: Config,
    headed: bool,
    mut cmds: UnboundedReceiver<Cmd>,
    events: UnboundedSender<Event>,
) {
    let log = |msg: String| {
        let _ = events.send(Event::Log(msg));
    };
    let mut smb: Option<Smb> = None;
    connect(&cfg, headed, &mut smb, &events, &log).await;

    while let Some(cmd) = cmds.recv().await {
        if smb.is_none() {
            connect(&cfg, headed, &mut smb, &events, &log).await;
        }
        let Some(session) = smb.as_ref() else {
            continue;
        };
        match cmd {
            Cmd::Check(entries) => match check(session, &entries).await {
                Ok(statuses) => {
                    let _ = events.send(Event::Statuses(statuses));
                }
                Err(e) => log(format!("could not read timesheet: {e:#}")),
            },
            Cmd::Submit(rows) => {
                let entries: Vec<Entry> = rows.iter().map(|(_, e)| e.clone()).collect();
                let statuses = match session.add_all(&entries, &log).await {
                    Ok(results) => results
                        .into_iter()
                        .map(|r| match r {
                            Ok(()) => Status::Added,
                            Err(e) => Status::Failed(format!("{e:#}")),
                        })
                        .collect(),
                    Err(e) => vec![Status::Failed(format!("{e:#}")); entries.len()],
                };
                let added = statuses.iter().filter(|s| **s == Status::Added).count();
                log(format!("done: {added}/{} added", entries.len()));
                let _ = events.send(Event::Submitted(
                    rows.into_iter().map(|(i, _)| i).zip(statuses).collect(),
                ));
            }
        }
    }
    if let Some(session) = smb {
        session.close().await;
    }
}

async fn connect(
    cfg: &Config,
    headed: bool,
    smb: &mut Option<Smb>,
    events: &UnboundedSender<Event>,
    log: &impl Fn(String),
) {
    let result = async {
        let session = Smb::launch(headed).await?;
        if let Err(e) = session.login(cfg, log).await {
            session.close().await;
            return Err(e);
        }
        anyhow::Ok(session)
    }
    .await;
    match result {
        Ok(session) => {
            *smb = Some(session);
            let _ = events.send(Event::Ready);
        }
        Err(e) => {
            let _ = events.send(Event::LoginFailed(format!("{e:#}")));
        }
    }
}

async fn check(session: &Smb, entries: &[Entry]) -> anyhow::Result<Vec<Status>> {
    let mut weeks: Vec<(NaiveDate, Vec<Entry>)> = Vec::new();
    let mut statuses = Vec::new();
    for entry in entries {
        let monday = week_start(entry.date);
        if !weeks.iter().any(|(m, _)| *m == monday) {
            weeks.push((monday, session.week(monday).await?));
        }
        let existing = &weeks.iter().find(|(m, _)| *m == monday).unwrap().1;
        statuses.push(classify(entry, existing));
    }
    Ok(statuses)
}
