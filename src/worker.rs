//! Background task that owns the browser session, so the TUI never blocks on SMB.

use chrono::NaiveDate;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::config::Config;
use crate::model::Entry;
use crate::smb::Smb;

pub enum Cmd {
    /// Read the logged entries of the week starting on this Monday.
    LoadWeek(NaiveDate),
    Submit(Vec<Entry>),
}

pub enum Event {
    Log(String),
    Ready,
    LoginFailed(String),
    Week(NaiveDate, Vec<Entry>),
    WeekFailed(NaiveDate, String),
    /// Outcome per submitted entry; `None` means it was saved.
    Submitted(Vec<(Entry, Option<String>)>),
}

pub async fn run(
    cfg: Config,
    headed: bool,
    mut cmds: UnboundedReceiver<Cmd>,
    events: UnboundedSender<Event>,
) {
    let send = |ev: Event| {
        let _ = events.send(ev);
    };
    let log = |msg: String| send(Event::Log(msg));
    let mut smb = connect(&cfg, headed, &send, &log).await;

    while let Some(cmd) = cmds.recv().await {
        if smb.is_none() {
            smb = connect(&cfg, headed, &send, &log).await;
        }
        let Some(session) = smb.as_ref() else {
            if let Cmd::LoadWeek(monday) = cmd {
                send(Event::WeekFailed(monday, "not logged in".into()));
            }
            continue;
        };
        match cmd {
            Cmd::LoadWeek(monday) => match session.week(monday).await {
                Ok(entries) => send(Event::Week(monday, entries)),
                Err(e) => send(Event::WeekFailed(monday, format!("{e:#}"))),
            },
            Cmd::Submit(entries) => match session.add_all(&entries, &log).await {
                Ok((results, weeks)) => {
                    let ok = results.iter().filter(|r| r.is_ok()).count();
                    log(format!("saved {ok}/{} blocks", entries.len()));
                    for (monday, logged) in weeks {
                        send(Event::Week(monday, logged));
                    }
                    let outcome = entries
                        .into_iter()
                        .zip(results)
                        .map(|(e, r)| (e, r.err().map(|err| format!("{err:#}"))))
                        .collect();
                    send(Event::Submitted(outcome));
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    log(format!("submit failed: {msg}"));
                    send(Event::Submitted(
                        entries
                            .into_iter()
                            .map(|e| (e, Some(msg.clone())))
                            .collect(),
                    ));
                }
            },
        }
    }
    if let Some(session) = smb {
        session.close().await;
    }
}

async fn connect(
    cfg: &Config,
    headed: bool,
    send: &impl Fn(Event),
    log: &impl Fn(String),
) -> Option<Smb> {
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
            send(Event::Ready);
            Some(session)
        }
        Err(e) => {
            send(Event::LoginFailed(format!("{e:#}")));
            None
        }
    }
}
