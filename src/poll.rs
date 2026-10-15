//! Fetching: a thread for each source, each on its own clock, sending what it
//! got to whoever's watching, the view or `ooze watch`. `--once` runs the
//! same fetches once each.
//!
//! The team can change under them: each message carries the generation it
//! was fetched for, and the watcher drops those from before a switch.
//!
//! Vercel caps a team's observability queries a day, so the live numbers (who's
//! online, page views a minute, requests and errors) go through the budget in
//! `budget.rs`, and are shared with every other ooze on the machine. Today's
//! visitors, the hourly figures and a project's details come from the Web
//! Analytics API instead, which the cap doesn't cover.

use crate::actions::{self, Action};
use crate::api::{Api, ApiError};
use crate::billing;
use crate::budget::{Budget, Wait};
use crate::config::{Config, Home};
use crate::model::{
    Analytics, Detail, DomainState, Edge, Health, LogLine, MINUTE, MINUTES, Msg, Source, Traffic, World,
};
use crate::vercel::{self, Deployment, Period, Point, Query, Scope, Team};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const PAGEVIEWS: &str = "vercel.analytics_pageview.count";
const REQUESTS: &str = "vercel.request.count";
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
/// The most projects whose Web Analytics are asked for each round.
const ANALYTICS_PROJECTS: usize = 30;

pub struct Envelope {
    pub generation: u64,
    pub msg: Msg,
}

/// How often each source is asked: the open view wants the live numbers,
/// `ooze watch` only enough to notify.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pace {
    View,
    Watch,
}

#[derive(Clone, Copy, Debug)]
struct Cadence {
    live: Duration,
    health: Duration,
    edge: Option<Duration>,
    analytics: Duration,
}

impl Cadence {
    fn of(pace: Pace, cfg: &Config) -> Cadence {
        let s = Duration::from_secs;
        match pace {
            Pace::View => {
                Cadence { live: s(cfg.refresh.max(60)), health: s(600), edge: Some(s(1800)), analytics: s(900) }
            }
            Pace::Watch => Cadence { live: s(1800), health: s(1800), edge: None, analytics: s(3600) },
        }
    }
}

#[derive(Default)]
struct Clock {
    team: Option<String>,
    generation: u64,
    pokes: HashSet<Source>,
    stop: bool,
}

#[derive(Default)]
struct Shared {
    clock: Mutex<Clock>,
    bell: Condvar,
    busy: AtomicBool,
    period: Mutex<Option<Period>>,
    /// Projects with Web Analytics on, for asking for their figures.
    analytics: Mutex<Vec<String>>,
    /// Projects with requests lately, busiest first, for the firewall's checks.
    active: Mutex<Vec<String>>,
    following: Mutex<Option<String>>,
}

#[derive(Clone)]
pub struct Hub {
    pub api: Api,
    cfg: Config,
    home: Home,
    cadence: Cadence,
    shared: Arc<Shared>,
    tx: Sender<Envelope>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

type Fetch = fn(&Hub, &Api, &str) -> Result<Msg, ApiError>;
/// How long a source waits before fetching again.
type Every = fn(&Hub) -> Duration;

impl Hub {
    /// A hub with nothing running, for fetching once.
    pub fn idle(api: Api, cfg: Config, home: Home, pace: Pace) -> (Hub, Receiver<Envelope>) {
        let (tx, rx) = channel();
        let cadence = Cadence::of(pace, &cfg);
        (Hub { api, cfg, home, cadence, shared: Arc::default(), tx }, rx)
    }

    /// Starts every source fetching, for the team `prefer` names if any.
    pub fn start(api: Api, cfg: Config, home: Home, prefer: Option<String>, pace: Pace) -> (Hub, Receiver<Envelope>) {
        let (hub, rx) = Hub::idle(api, cfg, home, pace);
        hub.spawn_who(prefer);
        let mut sources: Vec<(Source, Every, Fetch)> = vec![
            (Source::Team, |_| Duration::from_secs(600), fetch_team),
            (Source::Projects, |_| Duration::from_secs(300), fetch_projects),
            (Source::Traffic, |h| h.stretched(h.cadence.live), fetch_traffic),
            (Source::Health, |h| h.stretched(h.cadence.health), fetch_health),
            (Source::Analytics, |h| h.cadence.analytics, fetch_analytics),
            (
                Source::Deployments,
                |h| Duration::from_secs(if h.shared.busy.load(Ordering::Relaxed) { 5 } else { 15 }),
                fetch_deployments,
            ),
            (Source::Billing, |_| Duration::from_secs(1800), fetch_billing),
            (Source::Budgets, |_| Duration::from_secs(1800), fetch_budgets),
            (Source::Domains, |_| Duration::from_secs(3600), fetch_domains),
            (Source::Status, |_| Duration::from_secs(120), fetch_status),
            (Source::Events, |_| Duration::from_secs(60), fetch_events),
            (Source::Alerts, |_| Duration::from_secs(120), fetch_alerts),
            (Source::Attacks, |_| Duration::from_secs(300), fetch_attacks),
        ];
        if hub.cadence.edge.is_some() {
            sources.push((Source::Edge, |h| h.stretched(h.edge_every()), fetch_edge));
        }
        for (source, every, fetch) in sources {
            hub.spawn(source, every, fetch);
        }
        (hub, rx)
    }

    pub fn budget(&self, team: &str) -> Budget {
        Budget::new(&self.home.cache, team, self.cfg.daily_queries)
    }

    /// A budgeted source's wait: as set while the budget is plentiful, longer
    /// as it runs down, so it lasts the day.
    fn stretched(&self, every: Duration) -> Duration {
        let Some(team) = self.team() else { return every };
        let b = self.budget(&team);
        every * stretch(b.state().used(now()), b.daily)
    }

    fn edge_every(&self) -> Duration {
        self.cadence.edge.unwrap_or(Duration::from_secs(1800))
    }

    pub fn generation(&self) -> u64 {
        self.shared.clock.lock().unwrap().generation
    }

    pub fn team(&self) -> Option<String> {
        self.shared.clock.lock().unwrap().team.clone()
    }

    /// Watches another team: everything fetches again, for it.
    pub fn switch(&self, team: &str) {
        let mut c = self.shared.clock.lock().unwrap();
        c.team = Some(team.to_string());
        c.generation += 1;
        self.shared.bell.notify_all();
    }

    /// Has a source fetch again now.
    pub fn poke(&self, source: Source) {
        self.shared.clock.lock().unwrap().pokes.insert(source);
        self.shared.bell.notify_all();
    }

    pub fn stop(&self) {
        self.shared.clock.lock().unwrap().stop = true;
        self.shared.bell.notify_all();
    }

    fn stopped(&self) -> bool {
        self.shared.clock.lock().unwrap().stop
    }

    /// Waits, until it's time, the team changes, it's poked, or ooze stops.
    /// Says whether to carry on.
    fn nap(&self, source: Source, d: Duration) -> bool {
        let guard = self.shared.clock.lock().unwrap();
        let generation = guard.generation;
        let (mut guard, _) = self
            .shared
            .bell
            .wait_timeout_while(guard, d, |c| c.generation == generation && !c.pokes.contains(&source) && !c.stop)
            .unwrap();
        guard.pokes.remove(&source);
        !guard.stop
    }

    fn send(&self, generation: u64, msg: Msg) -> bool {
        self.tx.send(Envelope { generation, msg }).is_ok()
    }

    fn spawn(&self, source: Source, every: Every, fetch: Fetch) {
        let hub = self.clone();
        let name = format!("ooze-{source:?}").to_lowercase();
        std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                let mut failures = 0;
                let mut seeded = false;
                loop {
                    let Some(team) = hub.team() else {
                        if hub.stopped() || !hub.nap(source, Duration::from_secs(1)) {
                            return;
                        }
                        continue;
                    };
                    let generation = hub.generation();
                    // What another ooze last fetched shows straight away, however
                    // old, while this one asks for the latest.
                    if !seeded {
                        seeded = true;
                        if let Some(msg) = hub.seed(source, &team)
                            && !hub.send(generation, msg)
                        {
                            return;
                        }
                    }
                    let api = hub.api.for_team(Some(team.clone()));
                    let wait = match fetch(&hub, &api, &team) {
                        Ok(msg) => {
                            failures = 0;
                            if !hub.send(generation, msg) {
                                return;
                            }
                            every(&hub)
                        }
                        Err(e) => {
                            failures += 1;
                            let wait = backoff(&e, failures);
                            if !hub.send(generation, Msg::Failed(source, e)) {
                                return;
                            }
                            wait
                        }
                    };
                    if !hub.nap(source, wait) {
                        return;
                    }
                }
            })
            .expect("a poller's thread");
    }

    /// The last answer any ooze got for a budgeted source, as a message.
    fn seed(&self, source: Source, team: &str) -> Option<Msg> {
        let b = self.budget(team);
        match source {
            Source::Traffic => b.shared::<Traffic>("live").map(|(_, t)| Msg::Traffic(t)),
            Source::Health => b.shared::<Health>("health").map(|(_, h)| Msg::Health(h)),
            Source::Edge => b.shared::<Edge>("edge").map(|(_, e)| Msg::Edge(e)),
            _ => None,
        }
    }

    fn spawn_who(&self, prefer: Option<String>) {
        let hub = self.clone();
        let cli = crate::auth::cli_team(&hub.home);
        std::thread::Builder::new()
            .name("ooze-who".into())
            .spawn(move || {
                let mut failures = 0;
                loop {
                    let wait = match who(&hub.api, prefer.as_deref(), cli.as_deref()) {
                        Ok((user, teams, chosen)) => {
                            failures = 0;
                            if hub.team().is_none_or(|t| !teams.iter().any(|x| x.id == t)) {
                                hub.switch(&chosen);
                            }
                            if !hub.send(hub.generation(), Msg::Who { user, teams }) {
                                return;
                            }
                            Duration::from_secs(600)
                        }
                        Err(e) => {
                            failures += 1;
                            let wait = backoff(&e, failures);
                            if !hub.send(hub.generation(), Msg::Failed(Source::Who, e)) {
                                return;
                            }
                            wait
                        }
                    };
                    if !hub.nap(Source::Who, wait) {
                        return;
                    }
                }
            })
            .expect("a poller's thread");
    }

    /// Fetches a project's details once, off the drawing thread. Its errors by
    /// route cost an observability query, so they're asked for only when the
    /// project has had some.
    pub fn detail(&self, project: &str, with_errors: bool) {
        let (hub, project) = (self.clone(), project.to_string());
        let Some(team) = self.team() else { return };
        std::thread::spawn(move || {
            let api = hub.api.for_team(Some(team.clone()));
            let generation = hub.generation();
            let msg = match fetch_detail(&hub, &api, &team, &project, with_errors) {
                Ok(detail) => Msg::Detail { project, detail },
                Err(e) => Msg::Failed(Source::Analytics, e),
            };
            hub.send(generation, msg);
        });
    }

    /// Streams a deployment's build log while it's the one followed.
    pub fn follow(&self, deployment: &str, running: bool) {
        *self.shared.following.lock().unwrap() = Some(deployment.to_string());
        let (hub, uid) = (self.clone(), deployment.to_string());
        let Some(team) = self.team() else { return };
        std::thread::spawn(move || {
            let api = hub.api.for_team(Some(team));
            let generation = hub.generation();
            let mut batch = vec![];
            let mut last = std::time::Instant::now();
            let result = vercel::build_log(&api, &uid, running, |e| {
                if let Some(text) = e.payload.text.filter(|t| !t.trim().is_empty()) {
                    batch.push(LogLine { at: e.payload.date.or(e.created), text, error: e.kind == "stderr" });
                }
                if running && last.elapsed() > Duration::from_millis(300) && !batch.is_empty() {
                    hub.send(
                        generation,
                        Msg::Log { deployment: uid.clone(), lines: std::mem::take(&mut batch), done: false },
                    );
                    last = std::time::Instant::now();
                }
                hub.shared.following.lock().unwrap().as_deref() == Some(uid.as_str())
            });
            if let Err(e) = result {
                batch.push(LogLine { at: None, text: format!("ooze couldn't read the log: {e}"), error: true });
            }
            hub.send(generation, Msg::Log { deployment: uid, lines: batch, done: true });
        });
    }

    pub fn unfollow(&self) {
        *self.shared.following.lock().unwrap() = None;
    }

    /// Does an action, off the drawing thread, and fetches what it changed.
    pub fn act(&self, action: Action, d: Deployment, project: String) {
        let hub = self.clone();
        let Some(team) = self.team() else { return };
        std::thread::spawn(move || {
            let api = hub.api.for_team(Some(team));
            let said = actions::run(&api, action, &d, &project);
            hub.send(hub.generation(), Msg::Acted { project, deployment: d.uid.clone(), said });
            hub.poke(Source::Deployments);
            hub.poke(Source::Projects);
        });
    }
}

/// Twice as long once half the day's budget is spent, four times once three
/// quarters are.
fn stretch(used: u64, daily: u64) -> u32 {
    match used.saturating_mul(4) / daily.max(1) {
        0 | 1 => 1,
        2 => 2,
        _ => 4,
    }
}

/// How long to wait after a failure: until a limit lifts, or longer each time
/// something keeps failing.
fn backoff(e: &ApiError, failures: u32) -> Duration {
    let until = |t: i64| Duration::from_millis((t - now()).clamp(30_000, 2 * HOUR) as u64);
    match e {
        ApiError::Limited(t, _) => Duration::from_millis((t - now()).clamp(1_000, 600_000) as u64),
        ApiError::Capped(t, _) | ApiError::Budget(t) => until(*t),
        ApiError::Unauthorized(_) => Duration::from_secs(60),
        ApiError::Forbidden(_) | ApiError::NotFound(_) => Duration::from_secs(1800),
        _ => Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300)),
    }
}

/// An observability answer: another ooze's if it's no older than `fresh`, else
/// asked for within the budget. When Vercel says the team's daily cap is
/// reached, every ooze pauses.
fn observed<T: Serialize + DeserializeOwned>(
    hub: &Hub,
    team: &str,
    name: &str,
    cost: u64,
    fresh: Duration,
    ask: impl FnOnce() -> Result<T, ApiError>,
) -> Result<(i64, T), ApiError> {
    let budget = hub.budget(team);
    // A little slack, so a second ooze on the same clock uses the first one's answer.
    let fresh = fresh.as_millis() as i64 - 5_000;
    budget.fetch_or_share(name, fresh, now, || {
        budget.take(cost, now()).map_err(|w| match w {
            Wait::Capped(until, why) => ApiError::Capped(until, why),
            Wait::Budget(until) => ApiError::Budget(until),
        })?;
        ask().map_err(|e| match e {
            ApiError::Limited(_, why) if why.to_lowercase().contains("daily") => {
                ApiError::Capped(budget.pause(now(), &why), why)
            }
            other => other,
        })
    })
}

/// Who's signed in, their teams, and which team to watch: the one asked
/// for, else the Vercel CLI's current one, else their default, else the first.
fn who(api: &Api, prefer: Option<&str>, cli: Option<&str>) -> Result<(vercel::User, Vec<Team>, String), ApiError> {
    let user = vercel::user(api)?;
    let teams = vercel::teams(api)?;
    let chosen = choose(&teams, prefer, cli, user.default_team_id.as_deref()).ok_or_else(|| match prefer {
        Some(p) => ApiError::NotFound(format!("no team called {p}")),
        None => ApiError::NotFound("this account has no team".into()),
    })?;
    Ok((user, teams, chosen))
}

pub fn choose(teams: &[Team], prefer: Option<&str>, cli: Option<&str>, default: Option<&str>) -> Option<String> {
    if let Some(p) = prefer.filter(|p| !p.is_empty()) {
        return teams.iter().find(|t| t.id == p || t.slug == p || t.name.as_deref() == Some(p)).map(|t| t.id.clone());
    }
    [cli, default]
        .into_iter()
        .flatten()
        .find_map(|id| teams.iter().find(|t| t.id == id))
        .or(teams.first())
        .map(|t| t.id.clone())
}

/// Buckets of a minute each, by project, `n` of them from `start`.
fn series(points: &[Point], start: i64, n: usize) -> BTreeMap<String, Vec<u64>> {
    let mut out: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for p in points {
        let (Some(at), project) = (p.at, p.group("project_id")) else { continue };
        let i = (at - start) / MINUTE;
        if project.is_empty() || i < 0 || i as usize >= n {
            continue;
        }
        out.entry(project.to_string()).or_insert_with(|| vec![0; n])[i as usize] += p.value.max(0.0) as u64;
    }
    out
}

fn totals(points: &[Point]) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for p in points {
        let project = p.group("project_id");
        if !project.is_empty() {
            *out.entry(project.to_string()).or_insert(0) += p.value.max(0.0) as u64;
        }
    }
    out
}

fn fetch_team(hub: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    let t = vercel::team(api, team)?;
    *hub.shared.period.lock().unwrap() = t.billing.as_ref().and_then(|b| b.period);
    Ok(Msg::Team(t))
}

fn fetch_projects(hub: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    let projects = vercel::projects(api)?;
    *hub.shared.analytics.lock().unwrap() = projects.iter().filter(|p| p.analytics()).map(|p| p.id.clone()).collect();
    Ok(Msg::Projects(projects))
}

/// Page views a minute over the last hour, and who's online: two queries.
fn fetch_traffic(hub: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    observed(hub, team, "live", 2, hub.stretched(hub.cadence.live), || {
        let end = now() / MINUTE * MINUTE;
        let start = end - MINUTES as i64 * MINUTE;
        let scope = Scope::Team(team.into());
        let views = vercel::observe(api, &Query::new(PAGEVIEWS, "sum", scope.clone(), start, end, 1).by("project_id"))?;
        // Online is over the last five minutes, this one included; one-minute
        // buckets keep the range exactly that.
        let online = vercel::observe(
            api,
            &Query::new(PAGEVIEWS, "unique/visitor_id", scope, end + MINUTE - 5 * MINUTE, end + MINUTE, 1)
                .by("project_id"),
        )?;
        Ok(Traffic { at: now(), end, views: series(&views.data, start, MINUTES), online: totals(&online.summary) })
    })
    .map(|(_, t)| Msg::Traffic(t))
}

/// Requests and 5xx a minute over the last hour, by project, in one query
/// grouped by status.
fn fetch_health(hub: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    let (_, h) = observed(hub, team, "health", 1, hub.stretched(hub.cadence.health), || {
        let end = now() / MINUTE * MINUTE;
        let start = end - MINUTES as i64 * MINUTE;
        let mut q =
            Query::new(REQUESTS, "sum", Scope::Team(team.into()), start, end, 1).by("project_id").by("http_status");
        q.limit = 400;
        let o = vercel::observe(api, &q)?;
        Ok(health_of(&o.data, start, end))
    })?;
    let mut busiest: Vec<(&String, u64)> =
        h.requests.iter().map(|(p, v)| (p, v.iter().sum())).filter(|(_, n)| *n > 0).collect();
    busiest.sort_by_key(|b| std::cmp::Reverse(b.1));
    *hub.shared.active.lock().unwrap() = busiest.into_iter().take(10).map(|(p, _)| p.clone()).collect();
    Ok(Msg::Health(h))
}

/// Requests a minute by project over the hour, and 5xx over its last 15 minutes.
fn health_of(points: &[Point], start: i64, end: i64) -> Health {
    let mut requests: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    let mut errors: BTreeMap<String, Vec<u64>> = BTreeMap::new();
    for p in points {
        let (Some(at), project) = (p.at, p.group("project_id")) else { continue };
        let i = (at - start) / MINUTE;
        if project.is_empty() || i < 0 || i as usize >= MINUTES {
            continue;
        }
        let n = p.value.max(0.0) as u64;
        requests.entry(project.to_string()).or_insert_with(|| vec![0; MINUTES])[i as usize] += n;
        let status: u16 = p.group("http_status").parse().unwrap_or(0);
        let recent = i as usize + 15;
        if status >= 500 && recent >= MINUTES {
            errors.entry(project.to_string()).or_insert_with(|| vec![0; 15])[recent - MINUTES] += n;
        }
    }
    for (project, r) in &requests {
        if r.iter().rev().take(15).any(|n| *n > 0) {
            errors.entry(project.clone()).or_insert_with(|| vec![0; 15]);
        }
    }
    Health { at: now(), end, requests, errors }
}

const HITS: [&str; 4] = ["HIT", "STALE", "PRERENDER", "REVALIDATED"];
const MISSES: [&str; 3] = ["MISS", "BYPASS", "EXPIRED"];
const BLOCKED: [&str; 3] = ["deny", "challenge", "rate_limit"];

/// Cache hits over the last 15 minutes and what the firewall stopped in the
/// last hour: two queries.
fn fetch_edge(hub: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    observed(hub, team, "edge", 2, hub.stretched(hub.edge_every()), || {
        let end = now() / MINUTE * MINUTE;
        let scope = Scope::Team(team.into());
        let cache = vercel::observe(
            api,
            &Query::new(REQUESTS, "sum", scope.clone(), end - 15 * MINUTE, end, 15).by("project_id").by("cache_result"),
        )?;
        let firewall = vercel::observe(
            api,
            &Query::new("vercel.firewall_action.count", "sum", scope, end - HOUR, end, 60)
                .by("project_id")
                .by("waf_action"),
        )?;
        let mut e = Edge { at: now(), ..Edge::default() };
        for p in &cache.summary {
            let entry = e.cache.entry(p.group("project_id").to_string()).or_insert((0, 0));
            let n = p.value.max(0.0) as u64;
            match p.group("cache_result") {
                r if HITS.contains(&r) => entry.0 += n,
                r if MISSES.contains(&r) => entry.1 += n,
                _ => {}
            }
        }
        for p in firewall.summary.iter().filter(|p| BLOCKED.contains(&p.group("waf_action"))) {
            *e.blocked.entry(p.group("project_id").to_string()).or_insert(0) += p.value.max(0.0) as u64;
        }
        Ok(e)
    })
    .map(|(_, e)| Msg::Edge(e))
}

/// Today's visitors and page views and the last 24 hours' page views by the
/// hour, for each project with Web Analytics, from the Web Analytics API.
/// Vercel's days start at midnight UTC.
fn fetch_analytics(hub: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    let mut ids = hub.shared.analytics.lock().unwrap().clone();
    if ids.is_empty() {
        ids = vercel::projects(api)?.into_iter().filter(|p| p.analytics()).map(|p| p.id).collect();
    }
    let n = now();
    let day = n - n % DAY;
    let hour = n - n % HOUR;
    let mut a = Analytics { at: n, ..Analytics::default() };
    for id in ids.iter().take(ANALYTICS_PROJECTS) {
        match vercel::analytics_count(api, id, day, day + DAY) {
            Ok((views, visitors)) => {
                a.today.insert(id.clone(), (visitors, views));
            }
            // A project whose Web Analytics was turned off since.
            Err(ApiError::NotFound(_) | ApiError::Forbidden(_)) => continue,
            Err(e) => return Err(e),
        }
        let mut hours = vec![0u64; 24];
        for (at, views, _) in vercel::analytics_series(api, id, "hour", hour - 23 * HOUR, hour + HOUR)? {
            let i = (at - (hour - 23 * HOUR)) / HOUR;
            if (0..24).contains(&i) {
                hours[i as usize] += views;
            }
        }
        a.hourly.insert(id.clone(), hours);
    }
    Ok(Msg::Analytics(a))
}

fn fetch_deployments(hub: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    let list = vercel::deployments(api, 40)?;
    hub.shared.busy.store(list.iter().any(Deployment::running), Ordering::Relaxed);
    Ok(Msg::Deployments(list))
}

fn fetch_billing(hub: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    let known = *hub.shared.period.lock().unwrap();
    let period = match known {
        Some(p) => p,
        None => vercel::team(api, team)?
            .billing
            .and_then(|b| b.period)
            .ok_or_else(|| ApiError::NotFound("this team has no billing period".into()))?,
    };
    Ok(Msg::Billing(billing::refresh(api, team, period, &hub.home.cache, now())?))
}

fn fetch_budgets(_: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    Ok(Msg::Budgets(vercel::budgets(api)?.into_iter().filter(|b| b.active && !b.archived).collect()))
}

fn fetch_domains(_: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    let list = vercel::domains(api)?;
    let states = list
        .into_iter()
        .enumerate()
        .map(|(i, d)| {
            let misconfigured =
                if i < 25 { vercel::domain_config(api, &d.name).ok().map(|c| c.misconfigured) } else { None };
            DomainState { name: d.name, expires_at: d.expires_at, renew: d.renew, misconfigured }
        })
        .collect();
    Ok(Msg::Domains(states))
}

fn fetch_status(_: &Hub, _: &Api, _: &str) -> Result<Msg, ApiError> {
    Ok(Msg::Status(vercel::status_page()?))
}

fn fetch_events(_: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    Ok(Msg::Events(vercel::events(api, now() - DAY, 50)?))
}

fn fetch_alerts(_: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    let n = now();
    Ok(Msg::Alerts(vercel::alerts(api, n - DAY, n)?))
}

fn fetch_attacks(hub: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    let active = hub.shared.active.lock().unwrap().clone();
    let mut out = BTreeMap::new();
    for project in active {
        let list = vercel::attacks(api, &project)?;
        if !list.is_empty() {
            out.insert(project, list);
        }
    }
    Ok(Msg::Attacks(out))
}

/// Where a project's visitors came from over the last 24 hours, and its
/// visitors over 7 and 30 days, from the Web Analytics API; its 5xx by route
/// from an observability query.
fn fetch_detail(hub: &Hub, api: &Api, team: &str, project: &str, with_errors: bool) -> Result<Detail, ApiError> {
    let n = now();
    let hour = n - n % HOUR;
    let (since, until) = (hour - 23 * HOUR, hour + HOUR);
    let top = |by: &str| vercel::analytics_top(api, project, by, since, until, 8);
    let errors = if with_errors {
        let fresh = Duration::from_secs(300);
        observed(hub, team, &format!("errors-{project}"), 1, fresh, || {
            let end = n / MINUTE * MINUTE;
            let mut q =
                Query::new(REQUESTS, "sum", Scope::Projects(team.into(), vec![project.into()]), end - HOUR, end, 60)
                    .by("route");
            q = q.filter("http_status ge 500");
            q.limit = 8;
            let mut v: Vec<(String, u64)> = vercel::observe(api, &q)?
                .summary
                .iter()
                .map(|p| (p.group("route").to_string(), p.value.max(0.0) as u64))
                .collect();
            v.retain(|(k, n)| !k.is_empty() && *n > 0);
            v.sort_by_key(|r| std::cmp::Reverse(r.1));
            Ok(v)
        })
        .map(|(_, v)| v)
        .unwrap_or_default()
    } else {
        vec![]
    };
    let day = n - n % DAY;
    Ok(Detail {
        at: n,
        pages: top("requestPath")?,
        referrers: top("referrerHostname")?,
        countries: top("country")?,
        errors,
        week: vercel::analytics_count(api, project, day - 6 * DAY, day + DAY).ok(),
        month: vercel::analytics_count(api, project, day - 29 * DAY, day + DAY).ok(),
    })
}

/// Everything fetched once, for `--once` and `--json`. Live numbers another
/// ooze fetched in the last few minutes are used as they are, so asking often
/// costs nothing.
pub fn once(api: Api, cfg: Config, home: Home, prefer: Option<String>) -> Result<World, ApiError> {
    let started = now();
    let (user, teams, team) = who(&api, prefer.as_deref(), crate::auth::cli_team(&home).as_deref())?;
    let (mut hub, _rx) = Hub::idle(api.clone(), cfg.clone(), home, Pace::View);
    hub.cadence = Cadence {
        live: Duration::from_secs(300),
        health: Duration::from_secs(900),
        edge: Some(Duration::from_secs(3600)),
        analytics: Duration::from_secs(900),
    };
    hub.switch(&team);
    let scoped = api.for_team(Some(team.clone()));
    let fetches: [(Source, Fetch); 11] = [
        (Source::Team, fetch_team),
        (Source::Projects, fetch_projects),
        (Source::Traffic, fetch_traffic),
        (Source::Health, fetch_health),
        (Source::Edge, fetch_edge),
        (Source::Analytics, fetch_analytics),
        (Source::Deployments, fetch_deployments),
        (Source::Billing, fetch_billing),
        (Source::Domains, fetch_domains),
        (Source::Status, fetch_status),
        (Source::Alerts, fetch_alerts),
    ];
    let results: Vec<(Source, Result<Msg, ApiError>)> = std::thread::scope(|s| {
        let handles: Vec<_> =
            fetches.iter().map(|(source, fetch)| (*source, s.spawn(|| fetch(&hub, &scoped, &team)))).collect();
        handles
            .into_iter()
            .map(|(source, h)| (source, h.join().unwrap_or_else(|_| Err(ApiError::Decode("a fetch panicked".into())))))
            .collect()
    });
    let mut w = World::new(started, cfg);
    w.apply(Msg::Who { user, teams }, started);
    let mut results = results;
    // The team and projects first, so the rest can say names.
    results.sort_by_key(|(s, _)| !matches!(s, Source::Team | Source::Projects));
    for (source, result) in results {
        match result {
            Ok(msg) => w.apply(msg, started),
            Err(ApiError::Unauthorized(why)) => return Err(ApiError::Unauthorized(why)),
            Err(e) => {
                // Show the last answer anyone got, with its age, rather than nothing.
                if let Some(msg) = hub.seed(source, &team) {
                    w.apply(msg, started);
                }
                w.apply(Msg::Failed(source, e), started)
            }
        }
    }
    w.notices.clear();
    Ok(w)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn teams() -> Vec<Team> {
        serde_json::from_value(json!([{"id": "team_a", "slug": "night-shift", "name": "Night Shift"}, {"id": "team_b", "slug": "day-job"}])).unwrap()
    }

    #[test]
    fn the_team_is_the_one_asked_for_then_the_clis_then_the_default() {
        let t = teams();
        assert_eq!(choose(&t, Some("day-job"), Some("team_a"), None).as_deref(), Some("team_b"));
        assert_eq!(choose(&t, Some("Night Shift"), None, None).as_deref(), Some("team_a"));
        assert_eq!(
            choose(&t, Some("nobody"), None, None),
            None,
            "a team asked for that isn't there is an error, not another team"
        );
        assert_eq!(choose(&t, None, Some("team_b"), Some("team_a")).as_deref(), Some("team_b"));
        assert_eq!(choose(&t, None, Some("team_gone"), Some("team_a")).as_deref(), Some("team_a"));
        assert_eq!(choose(&t, None, None, None).as_deref(), Some("team_a"));
    }

    #[test]
    fn buckets_land_in_their_minute() {
        let start = 1_000 * MINUTE;
        let p = |at: i64, project: &str, v: f64| Point {
            at: Some(at),
            value: v,
            groups: BTreeMap::from([("project_id".to_string(), project.to_string())]),
        };
        let s = series(
            &[p(start, "a", 3.0), p(start + 2 * MINUTE, "a", 1.0), p(start + 9 * MINUTE, "a", 7.0), p(start, "", 1.0)],
            start,
            5,
        );
        assert_eq!(s["a"], [3, 0, 1, 0, 0], "outside the range, or without a project, is left out");
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn one_query_by_status_gives_requests_and_their_5xx() {
        let start = 1_000 * MINUTE;
        let end = start + MINUTES as i64 * MINUTE;
        let p = |minute: i64, status: &str, v: f64| Point {
            at: Some(start + minute * MINUTE),
            value: v,
            groups: BTreeMap::from([
                ("project_id".to_string(), "a".to_string()),
                ("http_status".to_string(), status.to_string()),
            ]),
        };
        let h = health_of(
            &[p(0, "200", 10.0), p(59, "200", 8.0), p(59, "503", 2.0), p(50, "500", 1.0), p(10, "502", 4.0)],
            start,
            end,
        );
        assert_eq!(h.requests["a"].iter().sum::<u64>(), 25);
        assert_eq!((h.requests["a"][0], h.requests["a"][59]), (10, 10));
        assert_eq!(h.errors["a"].iter().sum::<u64>(), 3, "5xx in the last 15 minutes only");
        assert_eq!(h.errors["a"][14], 2);
    }

    #[test]
    fn failing_waits_longer_each_time_and_a_limit_until_it_lifts() {
        let net = ApiError::Network("down".into());
        assert_eq!(backoff(&net, 1), Duration::from_secs(5));
        assert_eq!(backoff(&net, 3), Duration::from_secs(20));
        assert_eq!(backoff(&net, 30), Duration::from_secs(300));
        let soon = backoff(&ApiError::Limited(now() + 20_000, String::new()), 1);
        assert!(soon > Duration::from_secs(15) && soon <= Duration::from_secs(20));
        let capped = backoff(&ApiError::Capped(now() + 40 * MINUTE, "daily".into()), 1);
        assert!(capped > Duration::from_secs(39 * 60) && capped <= Duration::from_secs(40 * 60));
    }

    #[test]
    fn the_live_numbers_slow_down_as_the_days_budget_runs_down() {
        assert_eq!([0, 299, 300, 449, 450, 600, 900].map(|used| stretch(used, 600)), [1, 1, 2, 2, 4, 4, 4]);
        let dir = tempfile::tempdir().unwrap();
        let (hub, _rx) = Hub::idle(
            crate::api::tests::api("http://127.0.0.1:9"),
            Config::default(),
            Home::under(dir.path()),
            Pace::View,
        );
        hub.switch("team_1");
        assert_eq!(hub.stretched(Duration::from_secs(120)), Duration::from_secs(120));
        hub.budget("team_1").take(320, now()).unwrap();
        assert_eq!(hub.stretched(Duration::from_secs(120)), Duration::from_secs(240));
    }

    #[test]
    fn switching_teams_wakes_the_sources_and_moves_the_generation_on() {
        let dir = tempfile::tempdir().unwrap();
        let (hub, _rx) = Hub::idle(
            crate::api::tests::api("http://127.0.0.1:9"),
            Config::default(),
            Home::under(dir.path()),
            Pace::View,
        );
        let napping = {
            let hub = hub.clone();
            std::thread::spawn(move || {
                let t = std::time::Instant::now();
                hub.nap(Source::Traffic, Duration::from_secs(30));
                t.elapsed()
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        hub.switch("team_b");
        assert!(napping.join().unwrap() < Duration::from_secs(5), "a switch wakes it");
        assert_eq!((hub.team().as_deref(), hub.generation()), (Some("team_b"), 1));
    }

    #[test]
    fn the_daily_cap_pauses_every_ooze_and_answers_are_shared() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        let capped = r#"{"error":{"code":"rate_limited","message":"You have reached the daily observability metrics query limit for your team. Request a higher usage limit from your Vercel account team"}}"#;
        let f = crate::api::tests::fake(vec![(429, vec![("retry-after", "86400".into())], capped.into())]);
        let api = crate::api::tests::api(&f.base);
        let (hub, _rx) = Hub::idle(api.clone(), Config::default(), home.clone(), Pace::View);
        let err = fetch_traffic(&hub, &api, "team_1").unwrap_err();
        assert!(matches!(err, ApiError::Capped(until, _) if until > now() + 50 * MINUTE), "{err:?}");
        let (other, _rx2) = Hub::idle(api.clone(), Config::default(), home, Pace::Watch);
        assert!(
            matches!(fetch_health(&other, &api, "team_1"), Err(ApiError::Capped(..))),
            "the other ooze doesn't ask"
        );
        assert_eq!(f.seen.lock().unwrap().len(), 1, "one refused query, then none");
        assert_eq!(hub.budget("team_1").state().used(now()), 2, "the refused live query was counted");
    }
}
