//! The world as ooze knows it: what Vercel said last, folded in a message at
//! a time, and the feed lines and notifications that follow from what
//! changed.

use crate::api::ApiError;
use crate::billing::{self, Spend};
use crate::config::Config;
use crate::feed::{Kind, Line, Tone};
use crate::fmt;
use crate::vercel::{AlertGroup, Anomaly, Budget, Deployment, Event, Project, StatusPage, Team, User};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};

pub const MINUTE: i64 = 60_000;
/// Minutes of page views and requests kept.
pub const MINUTES: usize = 60;
/// Minutes in a project's sparkline.
pub const SHOWN: usize = 30;
const FEED: usize = 600;
/// How long a project's spike keeps it from spiking again.
const SPIKE_QUIET: i64 = 30 * MINUTE;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Source {
    Who,
    Team,
    Projects,
    Traffic,
    Today,
    Health,
    Deployments,
    Billing,
    Ai,
    Budgets,
    Domains,
    Status,
    Events,
    Alerts,
    Attacks,
}

impl Source {
    pub fn name(self) -> &'static str {
        match self {
            Source::Who => "who's signed in",
            Source::Team => "the team",
            Source::Projects => "projects",
            Source::Traffic => "page views",
            Source::Today => "today's visitors",
            Source::Health => "requests",
            Source::Deployments => "deployments",
            Source::Billing => "billing",
            Source::Ai => "AI Gateway",
            Source::Budgets => "AI Gateway budgets",
            Source::Domains => "domains",
            Source::Status => "Vercel's status",
            Source::Events => "team activity",
            Source::Alerts => "alerts",
            Source::Attacks => "the firewall",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Link {
    Connecting,
    Up,
    /// Vercel can't be reached, and why.
    Down(String),
    /// Vercel refused the token, and why.
    Refused(String),
}

/// Page views a minute and who's online, by project id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Traffic {
    /// The minute after the newest bucket.
    pub end: i64,
    /// The last MINUTES minutes, oldest first.
    pub views: BTreeMap<String, Vec<u64>>,
    /// Unique visitors in the last 5 minutes.
    pub online: BTreeMap<String, u64>,
}

/// Requests, errors, cache and firewall, by project id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Health {
    pub end: i64,
    /// Requests a minute, the last MINUTES, oldest first.
    pub requests: BTreeMap<String, Vec<u64>>,
    /// 5xx responses a minute, the last 15, oldest first.
    pub errors: BTreeMap<String, Vec<u64>>,
    /// Cache hits and misses over the last 15 minutes.
    pub cache: BTreeMap<String, (u64, u64)>,
    /// Requests the firewall challenged or denied in the last hour.
    pub blocked: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Ai {
    pub cost_today: f64,
    pub requests_today: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DomainState {
    pub name: String,
    pub expires_at: Option<i64>,
    pub renew: Option<bool>,
    pub misconfigured: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogLine {
    pub at: Option<i64>,
    pub text: String,
    pub error: bool,
}

/// What's open in a project's details: where its visitors come from and go.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Detail {
    pub at: i64,
    pub pages: Vec<(String, u64)>,
    pub referrers: Vec<(String, u64)>,
    pub countries: Vec<(String, u64)>,
    pub errors: Vec<(String, u64)>,
    /// Page views and visitors over 7 and 30 days.
    pub week: Option<(u64, u64)>,
    pub month: Option<(u64, u64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Msg {
    Who { user: User, teams: Vec<Team> },
    Team(Team),
    Projects(Vec<Project>),
    Traffic(Traffic),
    /// Visitors and page views since local midnight, by project id.
    Today(BTreeMap<String, (u64, u64)>),
    Health(Health),
    Deployments(Vec<Deployment>),
    Billing(Spend),
    Ai(Ai),
    Budgets(Vec<Budget>),
    Domains(Vec<DomainState>),
    Status(StatusPage),
    Events(Vec<Event>),
    Alerts(Vec<AlertGroup>),
    Attacks(BTreeMap<String, Vec<Anomaly>>),
    Failed(Source, ApiError),
    Log { deployment: String, lines: Vec<LogLine>, done: bool },
    Detail { project: String, detail: Detail },
    /// What an action did: its words for the feed, or why it couldn't.
    Acted { project: String, deployment: String, said: Result<String, String> },
}

/// Something worth a notification.
#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub key: String,
    pub title: String,
    pub body: String,
}

/// A project's row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub id: String,
    pub name: String,
    pub analytics: bool,
    pub online: u64,
    /// Page views a minute (or requests, without Web Analytics), the last SHOWN.
    pub minutes: Vec<u64>,
    /// Page views (or requests) in the last hour.
    pub hour: u64,
    pub today: Option<u64>,
    /// The share of requests answered with a 5xx in the last 15 minutes.
    pub errors: Option<f64>,
    pub cache: Option<f64>,
    pub blocked: u64,
    pub attack: bool,
    pub production: Option<Prod>,
}

/// A project's production deployment: the latest one, as it stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Prod {
    pub uid: String,
    pub state: String,
    pub at: i64,
    pub since: i64,
}

/// The header's numbers.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Glance {
    pub online: u64,
    pub minutes: Vec<u64>,
    pub visitors: u64,
    pub views: u64,
    pub building: usize,
    pub failed_today: usize,
    pub usage: Option<f64>,
    pub credit: Option<f64>,
}

pub struct World {
    pub cfg: Config,
    pub link: Link,
    pub since: i64,
    pub user: Option<User>,
    pub teams: Vec<Team>,
    pub team: Option<Team>,
    pub projects: Vec<Project>,
    pub traffic: Option<Traffic>,
    pub health: Option<Health>,
    pub today: BTreeMap<String, (u64, u64)>,
    pub deployments: Vec<Deployment>,
    pub spend: Option<Spend>,
    pub ai: Option<Ai>,
    pub budgets: Vec<Budget>,
    pub domains: Vec<DomainState>,
    pub status: Option<StatusPage>,
    pub attacks: BTreeMap<String, Vec<Anomaly>>,
    pub alerts: Vec<AlertGroup>,
    pub feed: VecDeque<Line>,
    /// Lines ever fed, so a paused feed can count what came since.
    pub fed: u64,
    pub updated: BTreeMap<Source, i64>,
    pub failed: BTreeMap<Source, (i64, ApiError)>,
    /// Rate limited until.
    pub limited: Option<i64>,
    /// When something last happened, for the Slime to notice.
    pub stirred: i64,
    pub logs: HashMap<String, (Vec<LogLine>, bool)>,
    pub details: HashMap<String, Detail>,
    pub notices: Vec<Notice>,
    notified: HashSet<String>,
    /// Sources that have answered at least once. What's already so in a
    /// source's first answer goes in the feed but doesn't notify, so starting
    /// ooze doesn't repeat what it said last time.
    heard: HashSet<Source>,
    credit_checked: bool,
    states: HashMap<String, String>,
    seeded: bool,
    events_seen: HashSet<String>,
    alerts_seen: HashMap<String, bool>,
    incidents_seen: HashMap<String, String>,
    indicator: Option<String>,
    attacks_seen: HashSet<String>,
    spikes: HashMap<String, i64>,
    error_spikes: HashMap<String, i64>,
}

impl World {
    pub fn new(now: i64, cfg: Config) -> World {
        World {
            cfg,
            link: Link::Connecting,
            since: now,
            user: None,
            teams: vec![],
            team: None,
            projects: vec![],
            traffic: None,
            health: None,
            today: BTreeMap::new(),
            deployments: vec![],
            spend: None,
            ai: None,
            budgets: vec![],
            domains: vec![],
            status: None,
            attacks: BTreeMap::new(),
            alerts: vec![],
            feed: VecDeque::new(),
            fed: 0,
            updated: BTreeMap::new(),
            failed: BTreeMap::new(),
            limited: None,
            stirred: 0,
            logs: HashMap::new(),
            details: HashMap::new(),
            notices: vec![],
            notified: HashSet::new(),
            heard: HashSet::new(),
            credit_checked: false,
            states: HashMap::new(),
            seeded: false,
            events_seen: HashSet::new(),
            alerts_seen: HashMap::new(),
            incidents_seen: HashMap::new(),
            indicator: None,
            attacks_seen: HashSet::new(),
            spikes: HashMap::new(),
            error_spikes: HashMap::new(),
        }
    }

    /// Forgets the team's numbers, for watching another.
    pub fn switch(&mut self, now: i64) {
        let cfg = self.cfg.clone();
        let (user, teams, status, link) = (self.user.take(), std::mem::take(&mut self.teams), self.status.take(), self.link.clone());
        *self = World::new(now, cfg);
        (self.user, self.teams, self.status, self.link) = (user, teams, status, link);
    }

    pub fn push(&mut self, mut line: Line, now: i64, live: bool) {
        line.arrived = if live { now } else { 0 };
        // Lines that come in late go where they belong, unless they're older than the whole feed.
        let pos = self.feed.iter().rposition(|l| l.at <= line.at).map_or(0, |i| i + 1);
        self.feed.insert(pos, line);
        self.fed += 1;
        while self.feed.len() > FEED {
            self.feed.pop_front();
        }
        if live {
            self.stirred = now;
        }
    }

    fn notify(&mut self, on: bool, key: String, title: impl Into<String>, body: impl Into<String>) {
        if on && self.notified.insert(key.clone()) {
            self.notices.push(Notice { key, title: title.into(), body: body.into() });
        }
    }

    pub fn project_name(&self, id: &str) -> String {
        self.projects.iter().find(|p| p.id == id).map(|p| p.name.clone()).unwrap_or_else(|| id.to_string())
    }

    fn project_id(&self, name: &str) -> Option<String> {
        self.projects.iter().find(|p| p.name == name).map(|p| p.id.clone())
    }

    pub fn apply(&mut self, msg: Msg, now: i64) {
        let source = match &msg {
            Msg::Who { .. } => Some(Source::Who),
            Msg::Team(_) => Some(Source::Team),
            Msg::Projects(_) => Some(Source::Projects),
            Msg::Traffic(_) => Some(Source::Traffic),
            Msg::Today(_) => Some(Source::Today),
            Msg::Health(_) => Some(Source::Health),
            Msg::Deployments(_) => Some(Source::Deployments),
            Msg::Billing(_) => Some(Source::Billing),
            Msg::Ai(_) => Some(Source::Ai),
            Msg::Budgets(_) => Some(Source::Budgets),
            Msg::Domains(_) => Some(Source::Domains),
            Msg::Status(_) => Some(Source::Status),
            Msg::Events(_) => Some(Source::Events),
            Msg::Alerts(_) => Some(Source::Alerts),
            Msg::Attacks(_) => Some(Source::Attacks),
            _ => None,
        };
        let mut first = false;
        if let Some(s) = source {
            self.updated.insert(s, now);
            self.failed.remove(&s);
            first = self.heard.insert(s);
            if s != Source::Status && self.link != Link::Up {
                self.link = Link::Up;
                self.since = now;
            }
        }
        match msg {
            Msg::Who { user, teams } => {
                self.user = Some(user);
                self.teams = teams;
            }
            Msg::Team(team) => {
                self.team = Some(team);
                self.check_credit(now);
            }
            Msg::Projects(mut p) => {
                p.sort_by(|a, b| a.name.cmp(&b.name));
                self.projects = p;
            }
            Msg::Traffic(t) => {
                self.traffic = Some(t);
                self.check_traffic(now, first);
            }
            Msg::Today(t) => self.today = t,
            Msg::Health(h) => {
                self.health = Some(h);
                self.check_errors(now, first);
            }
            Msg::Deployments(d) => self.take_deployments(d, now),
            Msg::Billing(s) => {
                self.spend = Some(s);
                self.check_credit(now);
            }
            Msg::Ai(a) => self.ai = Some(a),
            Msg::Budgets(b) => self.budgets = b,
            Msg::Domains(d) => self.take_domains(d, now, first),
            Msg::Status(s) => self.take_status(s, now, first),
            Msg::Events(e) => self.take_events(e, now, first),
            Msg::Alerts(a) => self.take_alerts(a, now, first),
            Msg::Attacks(a) => self.take_attacks(a, now, first),
            Msg::Failed(source, e) => self.take_failure(source, e, now),
            Msg::Log { deployment, lines, done } => {
                let entry = self.logs.entry(deployment).or_default();
                entry.0.extend(lines);
                let excess = entry.0.len().saturating_sub(2_000);
                entry.0.drain(..excess);
                entry.1 = done;
            }
            Msg::Detail { project, detail } => {
                self.details.insert(project, detail);
            }
            Msg::Acted { project, deployment, said } => {
                let who = self.project_name(&project);
                let line = match said {
                    Ok(text) => Line::new(now, Kind::Deploys, "↺", Tone::Good, who, text),
                    Err(why) => Line::new(now, Kind::Deploys, "✗", Tone::Bad, who, why),
                };
                self.push(line.about(Some(project), Some(deployment)), now, true);
            }
        }
    }

    fn take_failure(&mut self, source: Source, e: ApiError, now: i64) {
        match &e {
            ApiError::Unauthorized(why) => {
                self.link = Link::Refused(why.clone());
                self.since = now;
            }
            ApiError::Network(why) if source != Source::Status => {
                if !matches!(self.link, Link::Down(_)) {
                    self.since = now;
                }
                self.link = Link::Down(why.clone());
            }
            ApiError::Limited(until) => self.limited = Some(*until),
            _ => {}
        }
        self.failed.insert(source, (now, e));
    }

    fn take_deployments(&mut self, mut list: Vec<Deployment>, now: i64) {
        list.sort_by(|a, b| b.created.cmp(&a.created));
        let seeding = !self.seeded;
        self.seeded = true;
        let mut lines = vec![];
        for d in list.iter().rev() {
            let state = d.state().to_string();
            let before = self.states.insert(d.uid.clone(), state.clone());
            if before.as_deref() == Some(&state) {
                continue;
            }
            if seeding {
                // What happened in the last six hours, to start the feed with.
                if now - d.created < 6 * 3600 * 1000 {
                    lines.extend(deployment_lines(d, None, now));
                }
                continue;
            }
            lines.extend(deployment_lines(d, before.as_deref(), now));
            let who = d.name.clone();
            if d.production() && state == "ERROR" {
                let why = d.error_message.clone().unwrap_or_else(|| "the build failed".into());
                self.notify(self.cfg.notify.deploy_failed, format!("failed:{}", d.uid), format!("{who}: production deploy failed"), why);
            }
            if d.production() && state == "READY" && before.is_some() {
                let took = fmt::span(fmt::between(0, d.took(now)));
                self.notify(self.cfg.notify.deploy_ready, format!("ready:{}", d.uid), format!("{who} is live"), format!("Built in {took}"));
            }
        }
        for line in lines {
            self.push(line, now, !seeding);
        }
        self.deployments = list;
    }

    fn take_events(&mut self, events: Vec<Event>, now: i64, first: bool) {
        let mut list: Vec<Event> = events.into_iter().filter(|e| self.events_seen.insert(e.id.clone())).collect();
        list.sort_by_key(|e| e.created_at);
        for e in list {
            // Deployments have their own lines.
            if e.kind == "deployment" || e.kind.starts_with("aliases") || e.text.is_empty() {
                continue;
            }
            let who = e.user.as_ref().and_then(|u| u.username.clone()).unwrap_or_else(|| self.team.as_ref().map(|t| t.slug.clone()).unwrap_or_default());
            let line = Line::new(e.created_at, Kind::Team, "◆", Tone::Plain, who, fmt::one_line(&e.text)).detail("type", e.kind.clone());
            self.push(line, now, !first);
        }
    }

    fn take_alerts(&mut self, groups: Vec<AlertGroup>, now: i64, first: bool) {
        for g in &groups {
            let active = g.active();
            let before = self.alerts_seen.insert(g.id.clone(), active);
            if before == Some(active) || (before.is_none() && !active && first) {
                continue;
            }
            let who = g.project().map(|p| self.project_name(p)).unwrap_or_else(|| "Vercel".into());
            let (tone, text) = if active { (Tone::Warn, g.title()) } else { (Tone::Good, format!("resolved: {}", g.title())) };
            let mut line = Line::new(if active { g.started().unwrap_or(now) } else { now }, Kind::Vercel, "◇", tone, who, text)
                .about(g.project().map(str::to_string), None);
            if let Some(ai) = &g.ai {
                line = line.detail("summary", ai.current_summary.clone().unwrap_or_default());
                for f in &ai.key_findings {
                    line = line.detail("finding", f.clone());
                }
            }
            self.push(line, now, !first);
        }
        self.alerts = groups;
    }

    fn take_status(&mut self, s: StatusPage, now: i64, first: bool) {
        for i in &s.incidents {
            let before = self.incidents_seen.insert(i.id.clone(), i.status.clone());
            if before.as_deref() == Some(&i.status) {
                continue;
            }
            let tone = match i.impact.as_str() {
                "critical" | "major" => Tone::Bad,
                _ => Tone::Warn,
            };
            let line = Line::new(now, Kind::Vercel, "●", tone, "Vercel", format!("{} · {}", i.name, i.status)).detail("link", i.shortlink.clone().unwrap_or_default());
            self.push(line, now, !first);
            if i.status != "resolved" {
                self.notify(self.cfg.notify.incident && !first, format!("incident:{}:{}", i.id, i.status), "Vercel has an incident", format!("{} ({})", i.name, i.status));
            }
        }
        let calm = s.status.indicator == "none";
        if !first && calm && self.indicator.as_deref() != Some("none") {
            self.push(Line::new(now, Kind::Vercel, "●", Tone::Good, "Vercel", "all systems operational again"), now, true);
        }
        self.indicator = Some(s.status.indicator.clone());
        self.status = Some(s);
    }

    fn take_domains(&mut self, list: Vec<DomainState>, now: i64, first: bool) {
        let day = now / (24 * 3600 * 1000);
        let soon = self.cfg.thresholds.domain_days as i64 * 24 * 3600 * 1000;
        for d in &list {
            if d.misconfigured == Some(true) {
                let line = Line::new(now, Kind::Domains, "◎", Tone::Warn, d.name.clone(), "is misconfigured: its DNS doesn't point at Vercel");
                if self.notified.insert(format!("misconfigured:{}:{day}", d.name)) {
                    self.push(line, now, !first);
                }
            }
            if let Some(exp) = d.expires_at.filter(|e| *e - now < soon) {
                let renews = d.renew == Some(true);
                let text = match (exp > now, renews) {
                    (true, true) => format!("renews in {}", fmt::age(fmt::between(now, exp))),
                    (true, false) => format!("expires in {} and won't renew by itself", fmt::age(fmt::between(now, exp))),
                    (false, _) => "has expired".into(),
                };
                if self.notified.insert(format!("expiry-line:{}:{day}", d.name)) {
                    self.push(Line::new(now, Kind::Domains, "◎", if renews { Tone::Plain } else { Tone::Warn }, d.name.clone(), text.clone()), now, !first);
                }
                if !renews {
                    let key = format!("expiring:{}:{day}", d.name);
                    self.notify(self.cfg.notify.domain_expiring && !first, key, format!("{} {text}", d.name), "Renew it in Vercel's dashboard");
                }
            }
        }
        self.domains = list;
    }

    fn take_attacks(&mut self, attacks: BTreeMap<String, Vec<Anomaly>>, now: i64, first: bool) {
        for (project, list) in &attacks {
            for a in list {
                let key = format!("{project}:{}", a.start_time.map(|t| t as i64).unwrap_or(0));
                if !self.attacks_seen.insert(key.clone()) {
                    continue;
                }
                let who = self.project_name(project);
                let line = Line::new(now, Kind::Errors, "⊘", Tone::Bad, who.clone(), "Vercel's firewall is mitigating an attack").about(Some(project.clone()), None);
                self.push(line, now, !first);
                self.notify(self.cfg.notify.attack && !first, format!("attack:{key}"), format!("{who} is under attack"), "Vercel's firewall is mitigating it");
            }
        }
        self.attacks = attacks;
    }

    fn check_traffic(&mut self, now: i64, first: bool) {
        let Some(t) = &self.traffic else { return };
        let th = self.cfg.thresholds.clone();
        let mut found = vec![];
        for (project, minutes) in &t.views {
            if minutes.len() < MINUTES {
                continue;
            }
            let recent: u64 = minutes[MINUTES - 5..].iter().sum();
            let before: u64 = minutes[..MINUTES - 5].iter().sum();
            let usual = before as f64 / ((MINUTES - 5) as f64 / 5.0);
            if recent >= th.spike_min_views && recent as f64 >= th.spike_factor * usual.max(1.0) {
                found.push((project.clone(), recent, usual, t.online.get(project).copied().unwrap_or(0)));
            }
        }
        for (project, recent, usual, online) in found {
            if self.spikes.get(&project).is_some_and(|at| now - at < SPIKE_QUIET) {
                continue;
            }
            self.spikes.insert(project.clone(), now);
            let who = self.project_name(&project);
            let times = if usual >= 1.0 { format!("{:.0}× its usual", recent as f64 / usual) } else { "up from nothing".into() };
            let text = format!("traffic {times} · {} in 5 min, {online} online", fmt::count(recent, "page view", "page views"));
            self.push(Line::new(now, Kind::Traffic, "↑", Tone::Up, who.clone(), text.clone()).about(Some(project.clone()), None), now, !first);
            self.notify(self.cfg.notify.traffic_spike && !first, format!("spike:{project}:{}", now / SPIKE_QUIET), format!("{who}: traffic spike"), text);
        }
    }

    fn check_errors(&mut self, now: i64, first: bool) {
        let Some(h) = &self.health else { return };
        let th = self.cfg.thresholds.clone();
        let mut found = vec![];
        for (project, errors) in &h.errors {
            let failed: u64 = errors.iter().rev().take(5).sum();
            let total: u64 = h.requests.get(project).map(|r| r.iter().rev().take(5).sum()).unwrap_or(0);
            if total >= th.error_min_requests && failed as f64 >= th.error_rate * total as f64 {
                found.push((project.clone(), failed, total));
            }
        }
        for (project, failed, total) in found {
            if self.error_spikes.get(&project).is_some_and(|at| now - at < SPIKE_QUIET) {
                continue;
            }
            self.error_spikes.insert(project.clone(), now);
            let who = self.project_name(&project);
            let text = format!("5xx on {} of {} requests in 5 min", fmt::percent(failed as f64 / total as f64), fmt::num(total));
            self.push(Line::new(now, Kind::Errors, "!", Tone::Bad, who.clone(), text.clone()).about(Some(project.clone()), None), now, !first);
            self.notify(self.cfg.notify.error_spike && !first, format!("errors:{project}:{}", now / SPIKE_QUIET), format!("{who}: errors"), text);
        }
    }

    fn check_credit(&mut self, now: i64) {
        let (Some(team), Some(spend)) = (&self.team, &self.spend) else { return };
        let Some(credit) = billing::credit(team).filter(|c| *c > 0.0) else { return };
        let first = !std::mem::replace(&mut self.credit_checked, true);
        let usage = spend.usage();
        if usage < credit {
            return;
        }
        let key = format!("credit:{}:{}", team.id, spend.period.start);
        let text = format!("usage passed the {} credit: {} so far this period", fmt::money(credit), fmt::money(usage));
        if !self.notified.contains(&key) {
            let team_name = team.slug.clone();
            self.push(Line::new(now, Kind::Spend, "$", Tone::Warn, team_name, text.clone()), now, !first);
            if first {
                self.notified.insert(key);
            } else {
                self.notify(self.cfg.notify.credit_used, key, "Vercel usage passed the credit", text);
            }
        }
    }

    /// Projects as rows, busiest first, and how many quiet ones are folded away.
    pub fn rows(&self, now: i64, all: bool) -> (Vec<Row>, usize) {
        let mut rows: Vec<Row> = self.projects.iter().map(|p| self.row(p, now)).collect();
        rows.sort_by(|a, b| b.online.cmp(&a.online).then(b.hour.cmp(&a.hour)).then(a.name.cmp(&b.name)));
        if all {
            return (rows, 0);
        }
        let quiet = |r: &Row| {
            r.online == 0
                && r.hour < self.cfg.quiet_below.max(1)
                && !r.attack
                && r.production.as_ref().is_none_or(|p| p.state != "BUILDING" && p.state != "QUEUED" && !(p.state == "ERROR" && now - p.at < 3600 * 1000))
        };
        let folded = rows.iter().filter(|r| quiet(r)).count();
        rows.retain(|r| !quiet(r));
        (rows, folded)
    }

    fn row(&self, p: &Project, now: i64) -> Row {
        let analytics = p.analytics();
        let views = self.traffic.as_ref().and_then(|t| t.views.get(&p.id));
        let requests = self.health.as_ref().and_then(|h| h.requests.get(&p.id));
        let series = if analytics { views } else { requests };
        let minutes: Vec<u64> = series.map(|s| s[s.len().saturating_sub(SHOWN)..].to_vec()).unwrap_or_default();
        let hour = series.map(|s| s.iter().sum()).unwrap_or(0);
        let h = self.health.as_ref();
        let errors = h.and_then(|h| {
            let failed: u64 = h.errors.get(&p.id).map(|e| e.iter().sum()).unwrap_or(0);
            let total: u64 = h.requests.get(&p.id).map(|r| r.iter().rev().take(15).sum()).unwrap_or(0);
            (total > 0).then(|| failed as f64 / total as f64)
        });
        let cache = h.and_then(|h| h.cache.get(&p.id)).and_then(|(hit, miss)| (hit + miss > 0).then(|| *hit as f64 / (hit + miss) as f64));
        Row {
            id: p.id.clone(),
            name: p.name.clone(),
            analytics,
            online: self.traffic.as_ref().and_then(|t| t.online.get(&p.id)).copied().unwrap_or(0),
            minutes,
            hour,
            today: self.today.get(&p.id).map(|(visitors, _)| *visitors),
            errors,
            cache,
            blocked: h.and_then(|h| h.blocked.get(&p.id)).copied().unwrap_or(0),
            attack: self.attacks.get(&p.id).is_some_and(|a| a.iter().any(|a| a.end_time.is_none_or(|e| crate::vercel::millis(e) > now))),
            production: self.production(p),
        }
    }

    /// The deployment serving a project's production. A roll back holds until
    /// the next promote; after a promote, a newer production deployment that
    /// went live takes over again. Otherwise it's the newest ready one.
    pub fn serving(&self, p: &Project) -> Option<String> {
        let newest = self
            .deployments
            .iter()
            .filter(|d| d.production() && d.state() == "READY" && d.project_id.as_deref() == Some(p.id.as_str()))
            .filter(|d| d.ready_substate.as_deref() != Some("STAGED"))
            .max_by_key(|d| d.ready.unwrap_or(d.created));
        if let Some(r) = p.last_alias_request.as_ref().filter(|r| r.job_status.as_deref() != Some("failed") && r.to_deployment_id.is_some()) {
            let since_promote = newest.is_some_and(|d| d.ready.unwrap_or(d.created) > r.requested_at.unwrap_or(0));
            if r.kind.as_deref() == Some("rollback") || !since_promote {
                return r.to_deployment_id.clone();
            }
        }
        newest.map(|d| d.uid.clone()).or_else(|| p.production().filter(|c| c.ready_state.as_deref() == Some("READY")).map(|c| c.id.clone()))
    }

    fn production(&self, p: &Project) -> Option<Prod> {
        // A newer production build that's running or failed shows over the one serving.
        if let Some(d) = self.deployments.iter().find(|d| d.production() && d.project_id.as_deref() == Some(p.id.as_str())).filter(|d| d.running() || d.state() == "ERROR") {
            return Some(Prod { uid: d.uid.clone(), state: d.state().into(), at: d.ready.unwrap_or(d.created), since: d.building_at.unwrap_or(d.created) });
        }
        let serving = self.serving(p)?;
        let at = match self.find(&serving) {
            Some(d) => d.ready.unwrap_or(d.created),
            None => p.production().filter(|c| c.id == serving).and_then(|c| c.created_at).unwrap_or(0),
        };
        Some(Prod { uid: serving, state: "READY".into(), at, since: at })
    }

    /// Deployments in the order shown: running ones first, then the newest.
    pub fn deploy_rows(&self) -> Vec<&Deployment> {
        let mut v: Vec<&Deployment> = self.deployments.iter().filter(|d| d.running()).collect();
        v.extend(self.deployments.iter().filter(|d| !d.running()));
        v
    }

    pub fn is_current(&self, d: &Deployment) -> bool {
        self.project_of(d).and_then(|p| self.serving(p)).is_some_and(|id| id == d.uid)
    }

    pub fn glance(&self, now: i64) -> Glance {
        let mut minutes = vec![0u64; SHOWN];
        if let Some(t) = &self.traffic {
            for v in t.views.values() {
                for (i, n) in v[v.len().saturating_sub(SHOWN)..].iter().enumerate() {
                    minutes[i + SHOWN - v.len().min(SHOWN)] += n;
                }
            }
        }
        let today = local_day(now);
        Glance {
            online: self.traffic.as_ref().map(|t| t.online.values().sum()).unwrap_or(0),
            minutes,
            visitors: self.today.values().map(|(v, _)| v).sum(),
            views: self.today.values().map(|(_, v)| v).sum(),
            building: self.deployments.iter().filter(|d| d.running()).count(),
            failed_today: self.deployments.iter().filter(|d| d.production() && d.state() == "ERROR" && local_day(d.created) == today).count(),
            usage: self.spend.as_ref().map(Spend::usage),
            credit: self.team.as_ref().and_then(billing::credit),
        }
    }

    /// Whether something on screen moves: a build's clock, a line easing in.
    pub fn moving(&self, now: i64) -> bool {
        self.deployments.iter().any(|d| d.running()) || self.feed.back().is_some_and(|l| now - l.arrived < 2_000) || now - self.stirred < 1_500
    }

    /// Something needs looking at: the header's border goes orange.
    pub fn troubled(&self, now: i64) -> bool {
        let failed = self.deployments.iter().any(|d| d.production() && d.state() == "ERROR" && now - d.created < 3600 * 1000 && !self.fixed_since(d));
        let incident = self.status.as_ref().is_some_and(|s| !matches!(s.status.indicator.as_str(), "none" | ""));
        failed || incident || self.attacks.values().any(|a| !a.is_empty()) || self.error_spikes.values().any(|at| now - at < SPIKE_QUIET)
    }

    /// A newer production deployment of the same project went live after it.
    fn fixed_since(&self, d: &Deployment) -> bool {
        self.deployments.iter().any(|o| o.production() && o.project_id == d.project_id && o.created > d.created && o.state() == "READY")
    }

    pub fn find(&self, uid: &str) -> Option<&Deployment> {
        self.deployments.iter().find(|d| d.uid == uid)
    }

    pub fn project_of(&self, d: &Deployment) -> Option<&Project> {
        d.project_id.as_deref().and_then(|id| self.projects.iter().find(|p| p.id == id)).or_else(|| self.project_id(&d.name).and_then(|id| self.projects.iter().find(|p| p.id == id)))
    }
}

fn local_day(at: i64) -> Option<chrono::NaiveDate> {
    use chrono::TimeZone;
    chrono::Local.timestamp_millis_opt(at).single().map(|d| d.date_naive())
}

/// The lines a deployment's change of state makes, each at when it happened.
pub fn deployment_lines(d: &Deployment, before: Option<&str>, now: i64) -> Vec<Line> {
    let env = d.environment();
    let git = [d.branch(), d.commit().map(|s| fmt::sha(&s)), d.message().map(|m| format!("\"{}\"", fmt::fit_end(&fmt::one_line(&m), 48)))]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
    let by = d.author().map(|a| format!(" · {a}")).unwrap_or_default();
    let line = |at: i64, glyph, tone, text: String| {
        Line::new(at, Kind::Deploys, glyph, tone, d.name.clone(), text)
            .about(d.project_id.clone(), Some(d.uid.clone()))
            .detail("commit", d.message().unwrap_or_default())
            .detail("url", d.url.clone().map(|u| format!("https://{u}")).unwrap_or_default())
    };
    let started = || {
        let what = if git.is_empty() { format!("{env} build started{by}") } else { format!("{env} build started · {git}{by}") };
        line(d.building_at.unwrap_or(d.created), "▸", Tone::Work, what)
    };
    let took = fmt::span(fmt::between(0, d.took(now)));
    let end = d.ready.unwrap_or(now);
    match d.state() {
        // A running deployment's line is when it's first seen; queued to building says nothing new.
        "BUILDING" | "QUEUED" | "INITIALIZING" => if before.is_none() { vec![started()] } else { vec![] },
        "READY" => {
            let mut v = vec![];
            if before.is_none() && d.building_at.is_some() {
                v.push(started());
            }
            v.push(line(end, "✓", Tone::Good, format!("{env} ready in {took}{}", if git.is_empty() { String::new() } else { format!(" · {git}") })));
            v
        }
        "ERROR" => {
            let why = d.error_message.clone().map(|m| fmt::one_line(&m)).unwrap_or_else(|| d.error_code.clone().unwrap_or_else(|| "it failed".into()));
            let mut v = vec![];
            if before.is_none() && d.building_at.is_some() {
                v.push(started());
            }
            v.push(line(end, "✗", Tone::Bad, format!("{env} build failed: {why}")));
            v
        }
        "CANCELED" => vec![line(end, "■", Tone::Muted, format!("{env} build canceled"))],
        _ => vec![],
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use serde_json::json;

    pub const NOW: i64 = 1_792_086_000_000;

    pub fn deployment(uid: &str, project: &str, state: &str, target: Option<&str>, created: i64) -> Deployment {
        serde_json::from_value(json!({
            "uid": uid, "name": project, "projectId": format!("prj_{project}"), "created": created, "buildingAt": created + 2_000,
            "ready": if state == "READY" || state == "ERROR" { json!(created + 74_000) } else { json!(null) },
            "state": state, "target": target, "meta": {"githubCommitRef": "main", "githubCommitSha": "a1b2c3d4", "githubCommitMessage": "Fix search"},
            "creator": {"uid": "u", "username": "pat"}, "errorMessage": if state == "ERROR" { json!("Command \"npm run build\" exited with 1") } else { json!(null) }
        }))
        .unwrap()
    }

    fn world() -> World {
        let mut w = World::new(NOW, Config::default());
        let projects = ["moth-radio", "cave-notes", "lamp-docs"]
            .iter()
            .map(|n| serde_json::from_value(json!({"id": format!("prj_{n}"), "name": n, "webAnalytics": if *n == "lamp-docs" { json!(null) } else { json!({"enabledAt": 1}) }})).unwrap())
            .collect();
        w.apply(Msg::Projects(projects), NOW);
        w
    }

    #[test]
    fn a_deployment_feeds_a_line_as_it_starts_and_finishes_and_a_failure_notifies() {
        let mut w = world();
        w.apply(Msg::Deployments(vec![deployment("d0", "moth-radio", "READY", Some("production"), NOW - 3600_000)]), NOW);
        let seeded = w.feed.len();
        assert_eq!(seeded, 2, "what happened earlier starts the feed: {:?}", w.feed);
        assert!(w.feed.iter().all(|l| l.arrived == 0) && w.notices.is_empty(), "and doesn't notify");
        w.apply(Msg::Deployments(vec![deployment("d1", "cave-notes", "BUILDING", Some("production"), NOW)]), NOW + 1_000);
        assert!(w.feed.back().unwrap().text.starts_with("production build started · main a1b2c3d \"Fix search\" · pat"), "{:?}", w.feed.back());
        w.apply(Msg::Deployments(vec![deployment("d1", "cave-notes", "ERROR", Some("production"), NOW)]), NOW + 80_000);
        let last = w.feed.back().unwrap();
        assert_eq!(last.text, "production build failed: Command \"npm run build\" exited with 1");
        assert_eq!(w.notices.len(), 1);
        assert_eq!(w.notices[0].title, "cave-notes: production deploy failed");
        w.apply(Msg::Deployments(vec![deployment("d1", "cave-notes", "ERROR", Some("production"), NOW)]), NOW + 90_000);
        assert_eq!(w.notices.len(), 1, "once");
        assert!(w.troubled(NOW + 90_000));
    }

    #[test]
    fn a_spike_is_several_times_the_hour_before_and_said_once() {
        let mut w = world();
        let mut minutes = vec![2u64; MINUTES];
        for m in &mut minutes[MINUTES - 5..] {
            *m = 20;
        }
        let traffic = |m: Vec<u64>| Traffic { end: NOW, views: BTreeMap::from([("prj_moth-radio".to_string(), m)]), online: BTreeMap::from([("prj_moth-radio".to_string(), 18)]) };
        w.apply(Msg::Traffic(traffic(vec![2; MINUTES])), NOW - 10_000);
        w.apply(Msg::Traffic(traffic(minutes.clone())), NOW);
        let line = w.feed.back().unwrap();
        assert_eq!((line.who.as_str(), line.text.as_str()), ("moth-radio", "traffic 10× its usual · 100 page views in 5 min, 18 online"));
        assert_eq!(w.notices.len(), 1);
        w.apply(Msg::Traffic(traffic(minutes)), NOW + 10_000);
        assert_eq!(w.notices.len(), 1, "not again for half an hour");
        let mut w = world();
        w.apply(Msg::Traffic(traffic(vec![1; MINUTES])), NOW);
        assert!(w.feed.is_empty(), "steady traffic is no spike");
    }

    #[test]
    fn rows_put_the_busiest_first_and_fold_the_quiet_ones() {
        let mut w = world();
        let mut views = BTreeMap::new();
        views.insert("prj_moth-radio".to_string(), vec![3; MINUTES]);
        views.insert("prj_cave-notes".to_string(), vec![1; MINUTES]);
        w.apply(Msg::Traffic(Traffic { end: NOW, views, online: BTreeMap::from([("prj_cave-notes".into(), 4), ("prj_moth-radio".into(), 2)]) }), NOW);
        let mut h = Health::default();
        h.requests.insert("prj_moth-radio".into(), vec![10; MINUTES]);
        h.errors.insert("prj_moth-radio".into(), vec![1; 15]);
        h.cache.insert("prj_moth-radio".into(), (90, 10));
        w.apply(Msg::Health(h), NOW);
        let (rows, folded) = w.rows(NOW, false);
        assert_eq!(rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["cave-notes", "moth-radio"], "most online first");
        assert_eq!(folded, 1, "lamp-docs has nothing going on");
        let moth = &rows[1];
        assert_eq!((moth.hour, moth.minutes.len(), moth.errors, moth.cache), (180, SHOWN, Some(0.1), Some(0.9)));
        assert_eq!(w.rows(NOW, true).0.len(), 3);
        assert_eq!(w.glance(NOW).online, 6);
    }

    #[test]
    fn production_is_served_by_the_newest_ready_deployment_unless_rolled_back() {
        let project = |alias: serde_json::Value| -> Project {
            serde_json::from_value(json!({"id": "prj_moth-radio", "name": "moth-radio", "targets": {"production": {"id": "canceled", "readyState": "CANCELED"}}, "lastAliasRequest": alias})).unwrap()
        };
        let mut w = World::new(NOW, Config::default());
        w.apply(Msg::Projects(vec![project(json!(null))]), NOW);
        w.apply(
            Msg::Deployments(vec![
                deployment("canceled", "moth-radio", "CANCELED", Some("production"), NOW - 60_000),
                deployment("newer", "moth-radio", "READY", Some("production"), NOW - 3_600_000),
                deployment("older", "moth-radio", "READY", Some("production"), NOW - 7_200_000),
            ]),
            NOW,
        );
        let serving = |w: &World| w.serving(&w.projects[0].clone());
        assert_eq!(serving(&w).as_deref(), Some("newer"), "a canceled build serves nothing");
        assert_eq!(w.rows(NOW, true).0[0].production.as_ref().map(|p| p.uid.as_str()), Some("newer"));
        w.apply(Msg::Projects(vec![project(json!({"type": "rollback", "toDeploymentId": "older", "jobStatus": "succeeded", "requestedAt": NOW - 1_000}))]), NOW);
        assert_eq!(serving(&w).as_deref(), Some("older"), "a roll back holds");
        assert!(w.is_current(&w.find("older").unwrap().clone()));
        w.apply(Msg::Projects(vec![project(json!({"type": "promote", "toDeploymentId": "older", "jobStatus": "succeeded", "requestedAt": NOW - 6_000_000}))]), NOW);
        assert_eq!(serving(&w).as_deref(), Some("newer"), "a deployment that went live after a promote takes over");
    }

    #[test]
    fn an_error_spike_needs_enough_requests() {
        let mut w = world();
        let mut h = Health::default();
        h.requests.insert("prj_moth-radio".into(), vec![20; MINUTES]);
        h.errors.insert("prj_moth-radio".into(), vec![2; 15]);
        w.apply(Msg::Health(h.clone()), NOW);
        assert_eq!(w.feed.back().unwrap().text, "5xx on 10% of 100 requests in 5 min");
        let mut w = world();
        h.requests.insert("prj_moth-radio".into(), vec![2; MINUTES]);
        w.apply(Msg::Health(h), NOW);
        assert!(w.feed.is_empty(), "10 requests aren't enough to say");
    }

    #[test]
    fn a_refused_token_and_a_lost_network_show_in_the_link() {
        let mut w = world();
        assert_eq!(w.link, Link::Up);
        w.apply(Msg::Failed(Source::Traffic, ApiError::Network("it timed out".into())), NOW);
        assert_eq!(w.link, Link::Down("it timed out".into()));
        w.apply(Msg::Failed(Source::Who, ApiError::Unauthorized("Not authorized".into())), NOW);
        assert_eq!(w.link, Link::Refused("Not authorized".into()));
        w.apply(Msg::Projects(vec![]), NOW + 5_000);
        assert_eq!((w.link.clone(), w.since), (Link::Up, NOW + 5_000));
    }

    #[test]
    fn team_activity_feeds_lines_but_not_deployments_twice() {
        let mut w = world();
        let ev = |id: &str, kind: &str, text: &str| Event { id: id.into(), kind: kind.into(), text: text.into(), created_at: NOW, user: None };
        w.apply(Msg::Events(vec![ev("e1", "deployment", "pat deployed moth-radio"), ev("e2", "env-variable-updated", "pat updated API_URL in cave-notes")]), NOW);
        assert_eq!(w.feed.len(), 1);
        assert_eq!(w.feed[0].text, "pat updated API_URL in cave-notes");
        w.apply(Msg::Events(vec![ev("e2", "env-variable-updated", "pat updated API_URL in cave-notes")]), NOW);
        assert_eq!(w.feed.len(), 1, "an event is fed once");
    }

    #[test]
    fn passing_the_credit_is_said_once_a_period() {
        let mut w = world();
        let team: Team = serde_json::from_value(json!({"id": "team_1", "slug": "night-shift", "billing": {"plan": "pro", "invoiceItems": {"includedAllocationUsd": {"quantity": 20}}}})).unwrap();
        w.apply(Msg::Team(team), NOW);
        let mut spend = Spend { team: "team_1".into(), ..Spend::default() };
        spend.days.insert(0, billing::Day { usage: 12.0, ..billing::Day::default() });
        w.apply(Msg::Billing(spend.clone()), NOW);
        assert!(w.feed.is_empty());
        spend.days.insert(1, billing::Day { usage: 13.31, ..billing::Day::default() });
        w.apply(Msg::Billing(spend.clone()), NOW + 1);
        assert_eq!(w.feed.back().unwrap().text, "usage passed the $20.00 credit: $25.31 so far this period");
        w.apply(Msg::Billing(spend), NOW + 2);
        assert_eq!(w.notices.len(), 1);
        assert_eq!(w.glance(NOW).credit, Some(20.0));
    }

    #[test]
    fn what_was_already_so_at_the_start_is_fed_but_not_notified() {
        let mut w = world();
        let team: Team = serde_json::from_value(json!({"id": "team_1", "slug": "night-shift", "billing": {"invoiceItems": {"includedAllocationUsd": {"quantity": 20}}}})).unwrap();
        w.apply(Msg::Team(team), NOW);
        let mut spend = Spend { team: "team_1".into(), ..Spend::default() };
        spend.days.insert(0, billing::Day { usage: 25.0, ..billing::Day::default() });
        w.apply(Msg::Billing(spend), NOW);
        let incident = crate::vercel::Incident { id: "i1".into(), name: "Builds delayed".into(), status: "investigating".into(), impact: "minor".into(), ..Default::default() };
        w.apply(Msg::Status(StatusPage { status: crate::vercel::Indicator { indicator: "minor".into(), description: String::new() }, incidents: vec![incident] }), NOW);
        assert_eq!(w.feed.len(), 2);
        assert!(w.feed.iter().all(|l| l.arrived == 0));
        assert!(w.notices.is_empty(), "{:?}", w.notices);
        assert!(w.troubled(NOW), "an incident still shows");
    }
}
