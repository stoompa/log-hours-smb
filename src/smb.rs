//! Drives hr.smb.co.uk in a headless Chrome. The site is ASP.NET WebForms with
//! Telerik inputs and a DayPilot week calendar, so instead of clicking through
//! the modal we set the Telerik controls through their client API and click
//! the hidden "Add Entry" button, which fires the same async postback.

use std::time::Duration;

use anyhow::{Context, Result, bail};
use chromiumoxide::{Browser, BrowserConfig, Page};
use chrono::{NaiveDate, NaiveDateTime};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::model::{Entry, week_start};
use crate::secrets;

const BASE: &str = "https://hr.smb.co.uk";
const TIMESHEET: &str = "https://hr.smb.co.uk/HR/ViewEmployeeTimesheet2.aspx";
const CALENDAR: &str = "MainContent_DayPilotCalendarWeek";
const NAVIGATOR: &str = "MainContent_DayPilotNavigator1";
const TIMEOUT: Duration = Duration::from_secs(60);

pub struct Smb {
    browser: Browser,
    handler: JoinHandle<()>,
    page: Page,
}

#[derive(Deserialize)]
struct RawEvent {
    id: String,
    start: String,
    end: String,
}

impl Smb {
    pub async fn launch(headed: bool) -> Result<Self> {
        let mut builder = BrowserConfig::builder()
            .window_size(1400, 1000)
            .request_timeout(Duration::from_secs(90));
        if headed {
            builder = builder.with_head();
        }
        let (browser, mut handler) = Browser::launch(builder.build().map_err(anyhow::Error::msg)?)
            .await
            .context("could not start Chrome")?;
        let handler = tokio::spawn(async move { while handler.next().await.is_some() {} });
        let page = browser.new_page("about:blank").await?;
        Ok(Smb {
            browser,
            handler,
            page,
        })
    }

    pub async fn close(mut self) {
        let _ = self.browser.close().await;
        let _ = self.browser.wait().await;
        self.handler.abort();
    }

    async fn eval<T: serde::de::DeserializeOwned>(&self, js: impl Into<String>) -> Result<T> {
        Ok(self.page.evaluate(js.into()).await?.into_value()?)
    }

    async fn wait_until(&self, condition: &str, what: &str) -> Result<()> {
        let js = format!(
            "(() => {{ try {{ return !!({condition}); }} catch (e) {{ return false; }} }})()"
        );
        let deadline = tokio::time::Instant::now() + TIMEOUT;
        while tokio::time::Instant::now() < deadline {
            if self.eval::<bool>(js.clone()).await.unwrap_or(false) {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        bail!("timed out waiting for {what}")
    }

    /// Starts a navigation without waiting for the page's load event, which on
    /// SMB can take longer than CDP's request timeout. Callers poll for what
    /// they need with `wait_until`.
    async fn navigate(&self, url: &str) -> Result<()> {
        self.eval::<bool>(format!(
            "setTimeout(() => location.href = {}, 0); true",
            json!(url)
        ))
        .await?;
        tokio::time::sleep(Duration::from_millis(500)).await;
        Ok(())
    }

    async fn set_value(&self, id: &str, value: &str) -> Result<()> {
        let js = format!(
            "document.getElementById({}).value = {}; true",
            json!(id),
            json!(value)
        );
        self.eval::<bool>(js).await.map(|_| ())
    }

    async fn submit(&self, id: &str) -> Result<()> {
        self.eval::<bool>(format!(
            "setTimeout(() => document.getElementById({}).click(), 0); true",
            json!(id)
        ))
        .await
        .map(|_| ())
    }

    async fn page_message(&self) -> String {
        self.eval::<String>("(document.querySelector('.alert, .error, .validation-summary-errors, [style*=\"color:Red\"]') || {}).innerText || ''")
            .await
            .unwrap_or_default()
    }

    /// Company tag → username/password → one-time password → timesheet.
    pub async fn login(&self, cfg: &Config, log: &impl Fn(String)) -> Result<()> {
        log("opening SMB login…".into());
        self.navigate(&format!("{BASE}/Login.aspx?id=None")).await?;
        self.wait_until(
            "document.getElementById('txtCompanyName')",
            "the company tag field",
        )
        .await?;
        self.set_value("txtCompanyName", &cfg.company_tag).await?;
        self.submit("btnLogin3").await?;

        self.wait_until(
            "document.getElementById('txtPassword')",
            "the password field",
        )
        .await?;
        let creds = secrets::credentials(cfg).await?;
        self.set_value("txtUsername", &creds.username).await?;
        self.set_value("txtPassword", &creds.password).await?;
        log("signing in…".into());
        self.submit("btnLogin3").await?;

        self.wait_until(
            "document.getElementById('txt2FA') || !location.pathname.toLowerCase().startsWith('/default.aspx')",
            "the 2FA page",
        )
        .await?;
        if self
            .eval::<bool>("!!document.getElementById('txt2FA')")
            .await?
        {
            log("entering one-time password…".into());
            let code = secrets::otp(cfg).await?;
            self.set_value("txt2FA", &code).await?;
            self.submit("btnGo").await?;
            self.wait_until(
                "!location.pathname.toLowerCase().startsWith('/google2fa')",
                "2FA to finish",
            )
            .await?;
        }

        let path: String = self.eval("location.pathname").await?;
        if path.to_lowercase().contains("login") || path.to_lowercase().starts_with("/default.aspx")
        {
            bail!("login failed: {}", self.page_message().await);
        }
        self.open_timesheet().await?;
        log("logged in".into());
        Ok(())
    }

    async fn open_timesheet(&self) -> Result<()> {
        self.navigate(TIMESHEET).await?;
        self.wait_until(
            &format!("window.{CALENDAR} && window.{CALENDAR}.events && window.$find"),
            "the timesheet",
        )
        .await?;
        self.eval::<bool>(
            "(() => { if (!window.__smbHooked) { window.__smbHooked = true; \
             Sys.WebForms.PageRequestManager.getInstance().add_endRequest((s, a) => { \
             window.__smbError = a.get_error() ? a.get_error().message : null; a.set_errorHandled(true); }); } \
             return true; })()",
        )
        .await?;
        Ok(())
    }

    async fn idle(&self) -> Result<()> {
        self.wait_until(
            "!Sys.WebForms.PageRequestManager.getInstance().get_isInAsyncPostBack()",
            "SMB to respond",
        )
        .await
    }

    /// Shows the week containing `date` and returns its logged entries.
    /// Planned-schedule placeholders (ids starting with `W`) are left out.
    pub async fn week(&self, date: NaiveDate) -> Result<Vec<Entry>> {
        let monday = week_start(date);
        let target = format!("{monday}T00:00:00");
        let current: String = self
            .eval(format!(
                "String({CALENDAR}.startDate.value || {CALENDAR}.startDate)"
            ))
            .await?;
        if !current.starts_with(&target) {
            self.eval::<bool>(format!(
                "{NAVIGATOR}.select(new DayPilot.Date({})); true",
                json!(target)
            ))
            .await?;
            self.wait_until(
                &format!(
                    "String({CALENDAR}.startDate.value || {CALENDAR}.startDate).startsWith({})",
                    json!(target)
                ),
                "the week to load",
            )
            .await?;
            self.idle().await?;
        }
        let raw: Vec<RawEvent> = self
            .eval(format!(
                "{CALENDAR}.events.list.map(e => ({{ id: String(e.id), \
                 start: String(e.start.value || e.start), end: String(e.end.value || e.end) }}))"
            ))
            .await?;
        let mut entries = raw
            .into_iter()
            .filter(|e| e.id.starts_with('L'))
            .map(|e| {
                let start = NaiveDateTime::parse_from_str(&e.start[..19], "%Y-%m-%dT%H:%M:%S")?;
                let end = NaiveDateTime::parse_from_str(&e.end[..19], "%Y-%m-%dT%H:%M:%S")?;
                Ok(Entry {
                    date: start.date(),
                    start: start.time(),
                    end: end.time(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        entries.sort_by_key(|e| (e.date, e.start));
        Ok(entries)
    }

    /// Adds one entry through the Add Entry form's postback.
    pub async fn add(&self, entry: &Entry) -> Result<()> {
        let js = format!(
            "(() => {{ \
               const [y, m, d] = {date}.split('-').map(Number); \
               const at = t => {{ const [h, mi] = t.split(':').map(Number); return new Date(y, m - 1, d, h, mi); }}; \
               $find('ctl00_MainContent_txtAddDate').set_selectedDate(at('00:00')); \
               $find('ctl00_MainContent_txtAddStartTime').set_selectedDate(at({start})); \
               $find('ctl00_MainContent_txtAddEndTime').set_selectedDate(at({end})); \
               window.__smbError = null; \
               setTimeout(() => document.getElementById('MainContent_btnAddEntry').click(), 0); \
               return true; \
             }})()",
            date = json!(entry.date.to_string()),
            start = json!(crate::model::fmt_time(entry.start)),
            end = json!(crate::model::fmt_time(entry.end)),
        );
        self.eval::<bool>(js).await?;
        tokio::time::sleep(Duration::from_millis(300)).await;
        self.idle().await?;
        if let Some(err) = self.eval::<Option<String>>("window.__smbError").await? {
            bail!("{err}");
        }
        Ok(())
    }

    /// Adds entries that are not yet in SMB, then reloads and checks each one.
    /// Returns one result per entry, in order.
    pub async fn add_all(
        &self,
        entries: &[Entry],
        log: &impl Fn(String),
    ) -> Result<Vec<Result<()>>> {
        let mut results = Vec::new();
        for entry in entries {
            log(format!("adding {}", entry.to_line()));
            results.push(self.add(entry).await);
        }
        log("verifying…".into());
        self.open_timesheet().await?;
        let mut weeks: Vec<(NaiveDate, Vec<Entry>)> = Vec::new();
        for (entry, result) in entries.iter().zip(results.iter_mut()) {
            if result.is_err() {
                continue;
            }
            let monday = week_start(entry.date);
            if !weeks.iter().any(|(m, _)| *m == monday) {
                weeks.push((monday, self.week(monday).await?));
            }
            let logged = &weeks.iter().find(|(m, _)| *m == monday).unwrap().1;
            if !logged.contains(entry) {
                let msg = self.page_message().await;
                *result = Err(anyhow::anyhow!(if msg.is_empty() {
                    "not found after saving".to_string()
                } else {
                    msg
                }));
            }
        }
        Ok(results)
    }
}
