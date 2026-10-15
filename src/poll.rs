//! Fetching: a thread for each source, each on its own clock, sending what it
//! got to whoever's watching, the view or `ooze watch`. `--once` runs the
//! same fetches once each.
//!
//! The team can change under them: each message carries the generation it
//! was fetched for, and the watcher drops those from before a switch.

use crate::actions::{self, Action};
use crate::api::{Api, ApiError};
use crate::billing;
use crate::config::{Config, Home};
use crate::model::{Ai, Detail, DomainState, Health, LogLine, MINUTE, MINUTES, Msg, Source, Traffic, World};
use crate::vercel::{self, Deployment, Period, Point, Query, Scope, Team};
use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

const PAGEVIEWS: &str = "vercel.analytics_pageview.count";
const REQUESTS: &str = "vercel.request.count";
const OBSERVE: &str = "/v2/observability/query";
const DAY: i64 = 86_400_000;

pub struct Envelope {
    pub generation: u64,
    pub msg: Msg,
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
    /// Projects with requests lately, busiest first, for the firewall's checks.
    active: Mutex<Vec<String>>,
    following: Mutex<Option<String>>,
}

#[derive(Clone)]
pub struct Hub {
    pub api: Api,
    cfg: Config,
    home: Home,
    shared: Arc<Shared>,
    tx: Sender<Envelope>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

type Fetch = fn(&Hub, &Api, &str) -> Result<Msg, ApiError>;

impl Hub {
    /// A hub with nothing running, for fetching once.
    pub fn idle(api: Api, cfg: Config, home: Home) -> (Hub, Receiver<Envelope>) {
        let (tx, rx) = channel();
        (Hub { api, cfg, home, shared: Arc::default(), tx }, rx)
    }

    /// Starts every source fetching, for the team `prefer` names if any.
    pub fn start(api: Api, cfg: Config, home: Home, prefer: Option<String>) -> (Hub, Receiver<Envelope>) {
        let (hub, rx) = Hub::idle(api, cfg, home);
        hub.spawn_who(prefer);
        let sources: [(Source, fn(&Hub) -> Duration, Fetch); 14] = [
            (Source::Team, |_| Duration::from_secs(600), fetch_team),
            (Source::Projects, |_| Duration::from_secs(300), fetch_projects),
            (Source::Traffic, |h| h.observing(Duration::from_secs(h.cfg.refresh.max(5))), fetch_traffic),
            (Source::Today, |h| h.observing(Duration::from_secs(300)), fetch_today),
            (Source::Health, |h| h.observing(Duration::from_secs(30)), fetch_health),
            (Source::Deployments, |h| Duration::from_secs(if h.shared.busy.load(Ordering::Relaxed) { 5 } else { 15 }), fetch_deployments),
            (Source::Billing, |_| Duration::from_secs(1800), fetch_billing),
            (Source::Ai, |h| h.observing(Duration::from_secs(300)), fetch_ai),
            (Source::Budgets, |_| Duration::from_secs(1800), fetch_budgets),
            (Source::Domains, |_| Duration::from_secs(3600), fetch_domains),
            (Source::Status, |_| Duration::from_secs(120), fetch_status),
            (Source::Events, |_| Duration::from_secs(60), fetch_events),
            (Source::Alerts, |_| Duration::from_secs(120), fetch_alerts),
            (Source::Attacks, |_| Duration::from_secs(120), fetch_attacks),
        ];
        for (source, every, fetch) in sources {
            hub.spawn(source, every, fetch);
        }
        (hub, rx)
    }

    /// The wait before the next observability query: as asked, or longer
    /// when its rate limit runs low.
    fn observing(&self, every: Duration) -> Duration {
        every.max(self.api.pace(OBSERVE).unwrap_or_default())
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
        let (mut guard, _) =
            self.shared.bell.wait_timeout_while(guard, d, |c| c.generation == generation && !c.pokes.contains(&source) && !c.stop).unwrap();
        guard.pokes.remove(&source);
        !guard.stop
    }

    fn send(&self, generation: u64, msg: Msg) -> bool {
        self.tx.send(Envelope { generation, msg }).is_ok()
    }

    fn spawn(&self, source: Source, every: fn(&Hub) -> Duration, fetch: Fetch) {
        let hub = self.clone();
        let name = format!("ooze-{source:?}").to_lowercase();
        std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                let mut failures = 0;
                loop {
                    let Some(team) = hub.team() else {
                        if hub.stopped() || !hub.nap(source, Duration::from_secs(1)) {
                            return;
                        }
                        continue;
                    };
                    let generation = hub.generation();
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

    /// Fetches a project's details once, off the drawing thread.
    pub fn detail(&self, project: &str) {
        let (hub, project) = (self.clone(), project.to_string());
        let Some(team) = self.team() else { return };
        std::thread::spawn(move || {
            let api = hub.api.for_team(Some(team.clone()));
            let generation = hub.generation();
            let msg = match fetch_detail(&api, &team, &project) {
                Ok(detail) => Msg::Detail { project, detail },
                Err(e) => Msg::Failed(Source::Traffic, e),
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
                    hub.send(generation, Msg::Log { deployment: uid.clone(), lines: std::mem::take(&mut batch), done: false });
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

/// How long to wait after a failure: until a rate limit fills again, or
/// longer each time something keeps failing.
fn backoff(e: &ApiError, failures: u32) -> Duration {
    match e {
        ApiError::Limited(until) => Duration::from_millis((until - now()).clamp(1_000, 600_000) as u64),
        ApiError::Unauthorized(_) => Duration::from_secs(60),
        ApiError::Forbidden(_) | ApiError::NotFound(_) => Duration::from_secs(1800),
        _ => Duration::from_secs((5u64 << failures.saturating_sub(1).min(6)).min(300)),
    }
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
    [cli, default].into_iter().flatten().find_map(|id| teams.iter().find(|t| t.id == id)).or(teams.first()).map(|t| t.id.clone())
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

pub fn local_midnight(now: i64) -> i64 {
    use chrono::{Local, TimeZone};
    let Some(day) = Local.timestamp_millis_opt(now).single().map(|d| d.date_naive()) else { return now - now % DAY };
    day.and_hms_opt(0, 0, 0).and_then(|d| Local.from_local_datetime(&d).earliest()).map(|d| d.timestamp_millis()).unwrap_or(now - now % DAY)
}

fn fetch_team(hub: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    let t = vercel::team(api, team)?;
    *hub.shared.period.lock().unwrap() = t.billing.as_ref().and_then(|b| b.period);
    Ok(Msg::Team(t))
}

fn fetch_projects(_: &Hub, api: &Api, _: &str) -> Result<Msg, ApiError> {
    Ok(Msg::Projects(vercel::projects(api)?))
}

fn fetch_traffic(_: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    let end = now() / MINUTE * MINUTE;
    let start = end - MINUTES as i64 * MINUTE;
    let scope = Scope::Team(team.into());
    let views = vercel::observe(api, &Query::new(PAGEVIEWS, "sum", scope.clone(), start, end, 1).by("project_id"))?;
    // Online is over the last five minutes, this one included; one-minute
    // buckets keep the range exactly that.
    let online = vercel::observe(api, &Query::new(PAGEVIEWS, "unique/visitor_id", scope, end + MINUTE - 5 * MINUTE, end + MINUTE, 1).by("project_id"))?;
    Ok(Msg::Traffic(Traffic { end, views: series(&views.data, start, MINUTES), online: totals(&online.summary) }))
}

fn today_query(metric: &'static str, aggregation: &'static str, team: &str, by: Option<&'static str>) -> Query {
    let midnight = local_midnight(now());
    let mut q = Query::new(metric, aggregation, Scope::Team(team.into()), midnight, midnight + DAY, 1440);
    q.timezone = crate::fmt::local_timezone();
    if let Some(b) = by {
        q = q.by(b);
    }
    q
}

fn fetch_today(_: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    let visitors = vercel::observe(api, &today_query(PAGEVIEWS, "unique/visitor_id", team, Some("project_id")))?;
    let views = vercel::observe(api, &today_query(PAGEVIEWS, "sum", team, Some("project_id")))?;
    let (visitors, views) = (totals(&visitors.summary), totals(&views.summary));
    let mut out = BTreeMap::new();
    for project in visitors.keys().chain(views.keys()) {
        out.insert(project.clone(), (visitors.get(project).copied().unwrap_or(0), views.get(project).copied().unwrap_or(0)));
    }
    Ok(Msg::Today(out))
}

const HITS: [&str; 4] = ["HIT", "STALE", "PRERENDER", "REVALIDATED"];
const MISSES: [&str; 3] = ["MISS", "BYPASS", "EXPIRED"];
const BLOCKED: [&str; 3] = ["deny", "challenge", "rate_limit"];

fn fetch_health(hub: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    let end = now() / MINUTE * MINUTE;
    let start = end - MINUTES as i64 * MINUTE;
    let scope = Scope::Team(team.into());
    let requests = vercel::observe(api, &Query::new(REQUESTS, "sum", scope.clone(), start, end, 1).by("project_id"))?;
    let recent = end - 15 * MINUTE;
    let errors = vercel::observe(api, &Query::new(REQUESTS, "sum", scope.clone(), recent, end, 1).by("project_id").filter("http_status ge 500"))?;
    let cache = vercel::observe(api, &Query::new(REQUESTS, "sum", scope.clone(), recent, end, 15).by("project_id").by("cache_result"))?;
    let firewall = vercel::observe(api, &Query::new("vercel.firewall_action.count", "sum", scope, start, end, 60).by("project_id").by("waf_action"))?;
    let mut h = Health { end, requests: series(&requests.data, start, MINUTES), errors: series(&errors.data, recent, 15), ..Health::default() };
    for p in &cache.summary {
        let e = h.cache.entry(p.group("project_id").to_string()).or_insert((0, 0));
        let n = p.value.max(0.0) as u64;
        match p.group("cache_result") {
            r if HITS.contains(&r) => e.0 += n,
            r if MISSES.contains(&r) => e.1 += n,
            _ => {}
        }
    }
    for p in firewall.summary.iter().filter(|p| BLOCKED.contains(&p.group("waf_action"))) {
        *h.blocked.entry(p.group("project_id").to_string()).or_insert(0) += p.value.max(0.0) as u64;
    }
    let mut busiest: Vec<(&String, u64)> = h.requests.iter().map(|(p, v)| (p, v.iter().sum())).filter(|(_, n)| *n > 0).collect();
    busiest.sort_by(|a, b| b.1.cmp(&a.1));
    *hub.shared.active.lock().unwrap() = busiest.into_iter().take(10).map(|(p, _)| p.clone()).collect();
    Ok(Msg::Health(h))
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
        None => vercel::team(api, team)?.billing.and_then(|b| b.period).ok_or_else(|| ApiError::NotFound("this team has no billing period".into()))?,
    };
    Ok(Msg::Billing(billing::refresh(api, team, period, &hub.home.cache, now())?))
}

fn fetch_ai(_: &Hub, api: &Api, team: &str) -> Result<Msg, ApiError> {
    let cost = vercel::observe(api, &today_query("vercel.ai_gateway_request.cost", "sum", team, None))?;
    let count = vercel::observe(api, &today_query("vercel.ai_gateway_request.count", "sum", team, None))?;
    let sum = |o: &vercel::Observed| o.summary.iter().map(|p| p.value).sum::<f64>().max(0.0);
    Ok(Msg::Ai(Ai { cost_today: sum(&cost), requests_today: sum(&count) as u64 }))
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
            let misconfigured = if i < 25 { vercel::domain_config(api, &d.name).ok().map(|c| c.misconfigured) } else { None };
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

fn fetch_detail(api: &Api, team: &str, project: &str) -> Result<Detail, ApiError> {
    let end = (now() / MINUTE + 1) * MINUTE;
    let start = end - 60 * MINUTE;
    let scope = Scope::Projects(team.into(), vec![project.into()]);
    let top = |metric: &'static str, by: &'static str, filter: Option<&str>| -> Result<Vec<(String, u64)>, ApiError> {
        let mut q = Query::new(metric, "sum", scope.clone(), start, end, 60).by(by);
        q.limit = 8;
        if let Some(f) = filter {
            q = q.filter(f);
        }
        let mut v: Vec<(String, u64)> = vercel::observe(api, &q)?.summary.iter().map(|p| (p.group(by).to_string(), p.value.max(0.0) as u64)).collect();
        v.retain(|(k, n)| !k.is_empty() && *n > 0);
        v.sort_by(|a, b| b.1.cmp(&a.1));
        Ok(v)
    };
    let n = now();
    Ok(Detail {
        at: n,
        pages: top(PAGEVIEWS, "request_path", None)?,
        referrers: top(PAGEVIEWS, "referrer_hostname", None)?,
        countries: top(PAGEVIEWS, "country", None)?,
        errors: top(REQUESTS, "route", Some("http_status ge 500"))?,
        week: vercel::analytics_count(api, project, n - 7 * DAY, n).ok(),
        month: vercel::analytics_count(api, project, n - 30 * DAY, n).ok(),
    })
}

/// Everything fetched once, for `--once` and `--json`.
pub fn once(api: Api, cfg: Config, home: Home, prefer: Option<String>) -> Result<World, ApiError> {
    let started = now();
    let (user, teams, team) = who(&api, prefer.as_deref(), crate::auth::cli_team(&home).as_deref())?;
    let (hub, _rx) = Hub::idle(api.clone(), cfg.clone(), home);
    hub.switch(&team);
    let scoped = api.for_team(Some(team.clone()));
    let fetches: [(Source, Fetch); 11] = [
        (Source::Team, fetch_team),
        (Source::Projects, fetch_projects),
        (Source::Traffic, fetch_traffic),
        (Source::Today, fetch_today),
        (Source::Health, fetch_health),
        (Source::Deployments, fetch_deployments),
        (Source::Billing, fetch_billing),
        (Source::Ai, fetch_ai),
        (Source::Domains, fetch_domains),
        (Source::Status, fetch_status),
        (Source::Alerts, fetch_alerts),
    ];
    let results: Vec<(Source, Result<Msg, ApiError>)> = std::thread::scope(|s| {
        let handles: Vec<_> = fetches.iter().map(|(source, fetch)| (*source, s.spawn(|| fetch(&hub, &scoped, &team)))).collect();
        handles.into_iter().map(|(source, h)| (source, h.join().unwrap_or_else(|_| Err(ApiError::Decode("a fetch panicked".into()))))).collect()
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
            Err(e) => w.apply(Msg::Failed(source, e), started),
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
        assert_eq!(choose(&t, Some("nobody"), None, None), None, "a team asked for that isn't there is an error, not another team");
        assert_eq!(choose(&t, None, Some("team_b"), Some("team_a")).as_deref(), Some("team_b"));
        assert_eq!(choose(&t, None, Some("team_gone"), Some("team_a")).as_deref(), Some("team_a"));
        assert_eq!(choose(&t, None, None, None).as_deref(), Some("team_a"));
    }

    #[test]
    fn buckets_land_in_their_minute() {
        let start = 1_000 * MINUTE;
        let p = |at: i64, project: &str, v: f64| Point { at: Some(at), value: v, groups: BTreeMap::from([("project_id".to_string(), project.to_string())]) };
        let s = series(&[p(start, "a", 3.0), p(start + 2 * MINUTE, "a", 1.0), p(start + 9 * MINUTE, "a", 7.0), p(start, "", 1.0)], start, 5);
        assert_eq!(s["a"], [3, 0, 1, 0, 0], "outside the range, or without a project, is left out");
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn failing_waits_longer_each_time_and_a_limit_until_it_fills() {
        let net = ApiError::Network("down".into());
        assert_eq!(backoff(&net, 1), Duration::from_secs(5));
        assert_eq!(backoff(&net, 3), Duration::from_secs(20));
        assert_eq!(backoff(&net, 30), Duration::from_secs(300));
        let soon = backoff(&ApiError::Limited(now() + 20_000), 1);
        assert!(soon > Duration::from_secs(15) && soon <= Duration::from_secs(20));
    }

    #[test]
    fn switching_teams_wakes_the_sources_and_moves_the_generation_on() {
        let dir = tempfile::tempdir().unwrap();
        let (hub, _rx) = Hub::idle(crate::api::tests::api("http://127.0.0.1:9"), Config::default(), Home::under(dir.path()));
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
}
