//! `ooze demo`: the view on a made-up team, for screenshots that show
//! nothing of a real one and for trying ooze before signing in. Five busy
//! projects and seven quiet ones, a build running and one that failed,
//! a traffic spike, spend past the credit, and a domain about to lapse.

use crate::billing::{Day, Spend};
use crate::config::Config;
use crate::model::{Analytics, DomainState, Edge, Health, MINUTE, MINUTES, Msg, Traffic, World};
use crate::poll::Envelope;
use crate::vercel::{self, Deployment, Event, Period, Project, StatusPage, Team, User};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const DAY: i64 = 86_400_000;

/// A project's name, whether it has Web Analytics, its usual page views (or
/// requests) a minute, and its share of who's online.
const PROJECTS: &[(&str, bool, u64, u64)] = &[
    ("moth-radio", true, 5, 18),
    ("cave-notes", true, 1, 4),
    ("night-market", true, 1, 1),
    ("lamp-docs", false, 4, 0),
    ("grimoire-api", false, 2, 0),
    ("old-portfolio", true, 0, 0),
    ("beacon", false, 0, 0),
    ("tide-tables", true, 0, 0),
    ("ember-blog", true, 0, 0),
    ("fog-lights", false, 0, 0),
    ("kettle", false, 0, 0),
    ("static-sky", true, 0, 0),
];

/// Commit messages, as a busy team writes them.
const COMMITS: &[(&str, &str, &str)] = &[
    ("main", "Fix search on small screens", "pat"),
    ("main", "Player: resume where the episode stopped", "sam"),
    ("feat/tags", "Tags on every note", "jo"),
    ("main", "Checkout: keep the basket across devices", "pat"),
    ("fix/rss", "RSS: escape ampersands in titles", "sam"),
    ("main", "Docs: the new API keys page", "jo"),
    ("main", "Night mode follows the system", "pat"),
];

/// Small, steady and the same every run.
struct Dice(u64);

impl Dice {
    fn roll(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n.max(1)
    }
}

pub struct Demo {
    dice: Dice,
    start: i64,
    views: BTreeMap<String, Vec<u64>>,
    requests: BTreeMap<String, Vec<u64>>,
    deployments: Vec<Deployment>,
    last_traffic: i64,
    last_deploys: i64,
    events: usize,
}

fn id(name: &str) -> String {
    format!("prj_{name}")
}

fn deployment(n: usize, project: &str, state: &str, target: Option<&str>, created: i64, took: i64) -> Deployment {
    let (branch, message, author) = COMMITS[n % COMMITS.len()];
    let done = matches!(state, "READY" | "ERROR" | "CANCELED");
    serde_json::from_value(json!({
        "uid": format!("dpl_{:08x}", 0x5ee1u64 * 7919 + n as u64 * 104_729),
        "name": project, "projectId": id(project), "url": format!("{project}-{:05x}-night-shift.vercel.app", n * 2_011),
        "created": created, "buildingAt": created + 3_000, "ready": if done { json!(created + 3_000 + took) } else { json!(null) },
        "state": state, "target": target,
        "meta": {"githubCommitRef": branch, "githubCommitSha": format!("{:07x}{:033x}", 0xa1b2c3d + n * 77, n), "githubCommitMessage": message, "githubCommitAuthorLogin": author},
        "errorMessage": if state == "ERROR" { json!("Command \"npm run build\" exited with 1") } else { json!(null) },
        "inspectorUrl": format!("https://vercel.com/night-shift/{project}/{n}"),
    }))
    .expect("a made-up deployment")
}

impl Demo {
    pub fn new(now: i64) -> Demo {
        let mut dice = Dice(7);
        let mut views = BTreeMap::new();
        let mut requests = BTreeMap::new();
        for (name, analytics, usual, _) in PROJECTS {
            let mut series: Vec<u64> =
                (0..MINUTES).map(|_| if *usual == 0 { 0 } else { usual + dice.roll(usual + 2) }).collect();
            if *name == "moth-radio" {
                // A link on a busy site, twenty minutes ago.
                for (i, m) in series.iter_mut().enumerate().skip(MINUTES - 22).take(9) {
                    *m += [6, 14, 22, 26, 20, 15, 11, 8, 6][i - (MINUTES - 22)];
                }
            }
            let reqs: Vec<u64> = series.iter().map(|v| v * 9 + dice.roll(20)).collect();
            if *analytics {
                views.insert(id(name), series);
            }
            requests.insert(id(name), if *usual == 0 { vec![0; MINUTES] } else { reqs });
        }
        let m = MINUTE;
        let deployments = vec![
            deployment(0, "cave-notes", "BUILDING", Some("production"), now - 72_000, 0),
            deployment(1, "night-market", "ERROR", Some("production"), now - 5 * m, 64_000),
            deployment(2, "cave-notes", "READY", None, now - 14 * m, 38_000),
            deployment(3, "night-market", "READY", Some("production"), now - 50 * m, 81_000),
            deployment(4, "moth-radio", "READY", Some("production"), now - 2 * 60 * m, 72_000),
            deployment(5, "lamp-docs", "READY", Some("production"), now - 3 * 60 * m, 45_000),
            deployment(6, "cave-notes", "READY", Some("production"), now - 5 * 60 * m, 52_000),
            deployment(7, "moth-radio", "CANCELED", None, now - 6 * 60 * m, 20_000),
            deployment(8, "moth-radio", "READY", Some("production"), now - 26 * 60 * m, 70_000),
        ];
        Demo { dice, start: now, views, requests, deployments, last_traffic: now, last_deploys: now, events: 0 }
    }

    fn team(&self) -> Team {
        let period = Period {
            start: self.start - 12 * DAY - 9 * 3600 * 1000,
            end: self.start - 12 * DAY - 9 * 3600 * 1000 + 31 * DAY,
        };
        serde_json::from_value(json!({
            "id": "team_nightshift", "slug": "night-shift", "name": "night-shift",
            "billing": {"plan": "pro", "period": period, "invoiceItems": {
                "pro": {"price": 2000, "quantity": 1},
                "teamSeats": {"price": 2000, "quantity": 1},
                "includedAllocationUsd": {"price": 0, "quantity": 20},
            }}
        }))
        .expect("a made-up team")
    }

    fn projects(&self) -> Vec<Project> {
        PROJECTS
            .iter()
            .map(|(name, analytics, _, _)| {
                let current = self.deployments.iter().find(|d| d.name == *name && d.production() && d.state() == "READY");
                serde_json::from_value(json!({
                    "id": id(name), "name": name, "framework": "nextjs",
                    "webAnalytics": if *analytics { json!({"enabledAt": 1}) } else { json!(null) },
                    "targets": {"production": current.map(|d| json!({"id": d.uid, "readyState": "READY", "createdAt": d.created, "url": d.url}))
                        .unwrap_or(json!({"id": "dpl_old", "readyState": "READY", "createdAt": self.start - 40 * DAY}))},
                }))
                .expect("a made-up project")
            })
            .collect()
    }

    fn traffic(&self) -> Traffic {
        let mut online = BTreeMap::new();
        for (name, analytics, _, share) in PROJECTS {
            if *analytics && *share > 0 {
                let recent: u64 = self.views[&id(name)].iter().rev().take(5).sum();
                online.insert(id(name), (*share).max(recent / 6).min(share * 2 + 3));
            }
        }
        Traffic { at: self.last_traffic, end: self.last_traffic, views: self.views.clone(), online }
    }

    fn health(&self) -> Health {
        let mut h = Health {
            at: self.last_traffic,
            end: self.last_traffic,
            requests: self.requests.clone(),
            ..Health::default()
        };
        for (name, _, _, _) in PROJECTS.iter().filter(|p| p.2 > 0) {
            let errors =
                if *name == "grimoire-api" { vec![1, 0, 2, 1, 0, 1, 3, 0, 1, 2, 0, 1, 1, 0, 2] } else { vec![0; 15] };
            h.errors.insert(id(name), errors);
        }
        h
    }

    fn edge(&self) -> Edge {
        let mut e = Edge { at: self.start, ..Edge::default() };
        for (name, _, usual, _) in PROJECTS.iter().filter(|p| p.2 > 0) {
            e.cache.insert(id(name), if *name == "grimoire-api" { (12, 88) } else { (90 + usual, 6) });
        }
        e.blocked.insert(id("grimoire-api"), 412);
        e.blocked.insert(id("moth-radio"), 9);
        e
    }

    fn analytics(&self) -> Analytics {
        let today = BTreeMap::from([
            (id("moth-radio"), (3_410, 6_120)),
            (id("cave-notes"), (980, 1_604)),
            (id("night-market"), (142, 391)),
            (id("tide-tables"), (6, 9)),
        ]);
        let mut a = Analytics { at: self.start, today, ..Analytics::default() };
        for (name, _, usual, _) in PROJECTS.iter().filter(|p| p.1 && p.2 > 0) {
            // Quiet at night, busy in the afternoon.
            let hours = (0..24u64).map(|h| usual * 60 * (2 + (h + 6) % 24 * 6 / 24) / 4 + h * 7 % 11).collect();
            a.hourly.insert(id(name), hours);
        }
        a
    }

    fn spend(&self) -> Spend {
        let team = self.team();
        let period = team.billing.and_then(|b| b.period).expect("a period");
        let mut days = BTreeMap::new();
        let services = [
            ("Fast Data Transfer", 0.62),
            ("Fluid Active CPU", 0.48),
            ("Edge Requests", 0.31),
            ("Build CPU Minutes", 0.24),
            ("ISR Writes", 0.11),
            ("Image Optimization", 0.09),
        ];
        for d in 0..12 {
            let start = period.start + d * DAY;
            let mut day = Day { start, end: start + DAY, subscriptions: 0.71, ..Day::default() };
            for (i, (name, cost)) in services.iter().enumerate() {
                let c = cost * (0.8 + ((d as usize * 7 + i * 3) % 5) as f64 / 10.0) * if d >= 9 { 1.4 } else { 1.0 };
                *day.services.entry(name.to_string()).or_default() += c;
                day.usage += c;
            }
            day.services.insert("AI Gateway".into(), 0.18);
            day.usage += 0.18;
            for (p, share) in [("moth-radio", 0.55), ("cave-notes", 0.2), ("night-market", 0.12), ("lamp-docs", 0.08)] {
                day.projects.insert(p.into(), day.usage * share);
            }
            days.insert(start, day);
        }
        Spend { team: team.id, period, days, fetched: self.start }
    }

    fn event(&mut self, at: i64) -> Event {
        const SAID: &[(&str, &str, &str)] = &[
            ("env-variable-updated", "pat", "pat updated the environment variable API_URL in cave-notes"),
            ("project-domain-added", "jo", "jo added the domain notes.night-shift.dev to cave-notes"),
            ("member-joined", "sam", "sam joined the team"),
            ("env-variable-created", "jo", "jo added the environment variable STRIPE_KEY to night-market"),
        ];
        let (kind, who, text) = SAID[self.events % SAID.len()];
        self.events += 1;
        Event {
            id: format!("ev{}", self.events),
            text: text.into(),
            kind: kind.into(),
            created_at: at,
            user: Some(vercel::EventUser { username: Some(who.into()) }),
        }
    }

    /// Everything as it stands at the start.
    pub fn first(&mut self, now: i64) -> Vec<Msg> {
        let user = User {
            id: "u1".into(),
            username: "pat".into(),
            name: Some("Pat".into()),
            default_team_id: Some("team_nightshift".into()),
        };
        let team = self.team();
        let events = vec![self.event(now - 47 * MINUTE)];
        vec![
            Msg::Who { user, teams: vec![team.clone()] },
            Msg::Team(team),
            Msg::Projects(self.projects()),
            Msg::Status(StatusPage {
                status: vercel::Indicator { indicator: "none".into(), description: "All Systems Operational".into() },
                incidents: vec![],
            }),
            Msg::Deployments(self.deployments.clone()),
            Msg::Traffic(self.traffic()),
            Msg::Health(self.health()),
            Msg::Edge(self.edge()),
            Msg::Analytics(self.analytics()),
            Msg::Billing(self.spend()),
            Msg::Events(events),
            Msg::Domains(vec![
                DomainState {
                    name: "night-market.shop".into(),
                    expires_at: Some(now + 9 * DAY),
                    renew: Some(false),
                    misconfigured: Some(false),
                },
                DomainState {
                    name: "moth.radio".into(),
                    expires_at: Some(now + 200 * DAY),
                    renew: Some(true),
                    misconfigured: Some(false),
                },
            ]),
        ]
    }

    /// What changed since the last time.
    pub fn next(&mut self, now: i64) -> Vec<Msg> {
        let mut out = vec![];
        if now - self.last_traffic >= 10_000 {
            self.last_traffic = now;
            let mut dice = Dice(self.dice.roll(1 << 30));
            let mut spike = 0;
            for (name, _, usual, _) in PROJECTS {
                let id = id(name);
                if let Some(v) = self.views.get_mut(&id) {
                    v.remove(0);
                    let mut n = if *usual == 0 { 0 } else { usual + dice.roll(usual + 2) };
                    // Another surge, a minute or so in, for the feed to notice.
                    if *name == "moth-radio" && (60_000..120_000).contains(&(now - self.start)) {
                        n += 80;
                        spike += 1;
                    }
                    v.push(n);
                }
                if let Some(r) = self.requests.get_mut(&id) {
                    r.remove(0);
                    r.push(if *usual == 0 { 0 } else { usual * 9 + dice.roll(20) });
                }
            }
            let _ = spike;
            out.push(Msg::Traffic(self.traffic()));
            out.push(Msg::Health(self.health()));
        }
        if now - self.last_deploys >= 5_000 {
            self.last_deploys = now;
            let age = now - self.start;
            // cave-notes' build finishes half a minute in, and moth-radio's
            // starts a little later and takes a minute.
            let finish = |deployments: &mut Vec<Deployment>, project: &str| {
                if let Some(d) = deployments.iter_mut().find(|d| d.state() == "BUILDING" && d.name == project) {
                    d.state = Some("READY".into());
                    d.ready = Some(now);
                }
            };
            if age >= 30_000 {
                finish(&mut self.deployments, "cave-notes");
            }
            if (45_000..50_000).contains(&age) {
                let n = self.deployments.len();
                self.deployments.insert(0, deployment(n, "moth-radio", "BUILDING", Some("production"), now, 0));
            }
            if age >= 105_000 {
                finish(&mut self.deployments, "moth-radio");
            }
            out.push(Msg::Deployments(self.deployments.clone()));
            if age > 30_000 {
                out.push(Msg::Projects(self.projects()));
            }
        }
        if (now - self.start) / 40_000 > self.events as i64 - 1 {
            let e = self.event(now);
            out.push(Msg::Events(vec![e]));
        }
        out
    }
}

/// Where a project's visitors came from, for its details in the demo.
pub fn detail(project: &str, at: i64) -> crate::model::Detail {
    let pairs = |rows: &[(&str, u64)]| rows.iter().map(|(k, n)| (k.to_string(), *n)).collect::<Vec<_>>();
    match project {
        "moth-radio" => crate::model::Detail {
            at,
            pages: pairs(&[
                ("/", 412),
                ("/episodes/night-shift-42", 186),
                ("/episodes/the-lamp-room", 97),
                ("/about", 31),
                ("/rss.xml", 24),
            ]),
            referrers: pairs(&[
                ("news.ycombinator.com", 318),
                ("google.com", 104),
                ("t.co", 51),
                ("duckduckgo.com", 19),
            ]),
            countries: pairs(&[("US", 287), ("GB", 121), ("DE", 74), ("PT", 41), ("JP", 33)]),
            errors: vec![],
            week: Some((41_220, 12_840)),
            month: Some((168_400, 51_900)),
        },
        "cave-notes" => crate::model::Detail {
            at,
            pages: pairs(&[("/", 64), ("/notes/tags", 38), ("/search", 17)]),
            referrers: pairs(&[("google.com", 41), ("github.com", 12)]),
            countries: pairs(&[("GB", 39), ("US", 30), ("FR", 11)]),
            errors: vec![],
            week: Some((10_240, 6_120)),
            month: Some((44_800, 23_100)),
        },
        "grimoire-api" => crate::model::Detail {
            at,
            errors: pairs(&[("/api/spells/[id]", 14), ("/api/search", 5)]),
            ..Default::default()
        },
        _ => crate::model::Detail { at, ..Default::default() },
    }
}

/// A made-up build log, for a deployment's details in the demo.
pub fn log(d: Option<&Deployment>) -> Vec<crate::model::LogLine> {
    let Some(d) = d else { return vec![] };
    let start = d.building_at.unwrap_or(d.created);
    let mut said = vec![
        format!(
            "Cloning github.com/night-shift/{} (Branch: {}, Commit: {})",
            d.name,
            d.branch().unwrap_or_default(),
            d.commit().map(|c| crate::fmt::sha(&c)).unwrap_or_default()
        ),
        "Cloning completed: 412.000ms".into(),
        "Restored build cache from previous deployment".into(),
        "Running \"vercel build\"".into(),
        "Installing dependencies...".into(),
        "added 412 packages in 9s".into(),
        "Detected Next.js version: 16.0.1".into(),
        "Running \"npm run build\"".into(),
        "Creating an optimized production build ...".into(),
    ];
    match d.state() {
        "READY" => said.extend(
            [
                "Compiled successfully in 18.2s",
                "Generating static pages (34/34)",
                "Build Completed in /vercel/output [41s]",
                "Deploying outputs...",
                "Deployment completed",
            ]
            .map(String::from),
        ),
        "ERROR" => said.extend(
            [
                "Failed to compile.",
                "./app/checkout/page.tsx:42:7",
                "Type error: Property 'basket' does not exist on type 'Session'.",
                "Error: Command \"npm run build\" exited with 1",
            ]
            .map(String::from),
        ),
        _ => {}
    }
    let n = said.len();
    said.into_iter()
        .enumerate()
        .map(|(i, text)| crate::model::LogLine {
            at: Some(start + i as i64 * 2_500),
            error: d.state() == "ERROR" && i + 4 >= n,
            text,
        })
        .collect()
}

/// The demo, a message at a time, as Vercel would say it.
pub fn spawn() -> Receiver<Envelope> {
    let (tx, rx) = channel();
    std::thread::Builder::new()
        .name("ooze-demo".into())
        .spawn(move || {
            let start = chrono::Utc::now().timestamp_millis();
            let mut demo = Demo::new(start);
            for msg in demo.first(start) {
                if tx.send(Envelope { generation: 0, msg }).is_err() {
                    return;
                }
            }
            loop {
                std::thread::sleep(Duration::from_secs(1));
                let now = chrono::Utc::now().timestamp_millis();
                for msg in demo.next(now) {
                    if tx.send(Envelope { generation: 0, msg }).is_err() {
                        return;
                    }
                }
            }
        })
        .expect("the demo's thread");
    rx
}

/// The demo as it stands a while in: a build running, one failed, a spike.
pub fn world(now: i64) -> World {
    let start = now - 75_000;
    let mut demo = Demo::new(start);
    let mut w = World::new(start, Config::default());
    for msg in demo.first(start) {
        w.apply(msg, start);
    }
    let mut t = start;
    while t < now {
        t += 1_000;
        for msg in demo.next(t) {
            w.apply(msg, t);
        }
    }
    w.notices.clear();
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::NOW;

    #[test]
    fn the_demo_has_something_of_everything() {
        let w = world(NOW);
        let (rows, folded) = w.rows(NOW, false);
        assert_eq!(rows[0].name, "moth-radio");
        assert!(folded >= 6, "{folded} quiet projects");
        assert!(w.deployments.iter().any(|d| d.running()), "a build running");
        assert!(w.deployments.iter().any(|d| d.state() == "ERROR"), "and one failed");
        assert!(
            w.feed.iter().any(|l| l.text.starts_with("traffic")),
            "a spike: {:?}",
            w.feed.iter().map(|l| &l.text).collect::<Vec<_>>()
        );
        let g = w.glance(NOW);
        assert!(g.usage.unwrap() > g.credit.unwrap(), "spend past the credit");
        assert!(w.feed.iter().any(|l| l.text.contains("won't renew")));
    }
}
