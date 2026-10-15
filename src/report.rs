//! `ooze --once`: one glance as text, for agents and scripts to see what's
//! happening on Vercel, and `--json`, the same as JSON.

use crate::actions::short;
use crate::billing;
use crate::fmt;
use crate::model::{Row, Source, World};
use crate::theme::{Paint, hex};
use serde_json::{Value, json};

fn production(r: &Row, w: &World, now: i64) -> String {
    let Some(p) = &r.production else { return "no production deployment".into() };
    match p.state.as_str() {
        "BUILDING" | "QUEUED" | "INITIALIZING" => {
            format!("production building {}", fmt::clock(fmt::between(p.since, now)))
        }
        "ERROR" => {
            let why = w
                .find(&p.uid)
                .and_then(|d| d.error_message.clone())
                .map(|m| format!(": {}", fmt::fit_end(&fmt::one_line(&m), 80)))
                .unwrap_or_default();
            format!("production failed {}{why}", fmt::ago(fmt::between(p.at, now)))
        }
        "CANCELED" => format!("production canceled {}", fmt::ago(fmt::between(p.at, now))),
        _ => format!("production ready {}", fmt::ago(fmt::between(p.at, now))),
    }
}

pub fn text(w: &World, now: i64, p: Paint) -> String {
    let mut out = String::new();
    let mut line = |s: String| {
        out.push_str(&s);
        out.push('\n');
    };
    let label = |s: &str| p.fg(hex::COMMENT, format!("{s:<9}"));
    let team = w
        .team
        .as_ref()
        .map(|t| format!("{}{}", t.title(), t.plan().map(|pl| format!(" ({})", capitalised(pl))).unwrap_or_default()));
    line(format!("{} · {} · {}", p.bold("ooze"), team.unwrap_or_else(|| "no team".into()), fmt::when(now, now)));
    let g = w.glance(now);
    line(format!(
        "{}{} · today {} · {}",
        label("online"),
        p.fg(hex::TEAL, format!("{} now", fmt::num(g.online))),
        fmt::count(g.visitors, "visitor", "visitors"),
        fmt::count(g.views, "page view", "page views")
    ));
    let (rows, folded) = w.rows(now, false);
    line(label("projects").trim_end().to_string());
    let name_w = rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(8).min(24);
    for r in &rows {
        let mut parts = vec![];
        if r.analytics {
            parts.push(format!("{} online", r.online));
            parts.push(format!("{} views/h", fmt::compact(r.hour)));
            if let Some(t) = r.today {
                parts.push(format!("{} today", fmt::compact(t)));
            }
        } else {
            parts.push(format!("{} requests/h", fmt::compact(r.hour)));
        }
        if let Some(e) = r.errors {
            parts.push(format!("5xx {}", fmt::percent(e)));
        }
        if let Some(c) = r.cache {
            parts.push(format!("cache {}", fmt::percent(c)));
        }
        if r.blocked > 0 {
            parts.push(format!("{} blocked", fmt::compact(r.blocked)));
        }
        if r.attack {
            parts.push(p.fg(hex::RED, "under attack"));
        }
        let prod = production(r, w, now);
        let prod = if prod.contains("failed") {
            p.fg(hex::RED, prod)
        } else if prod.contains("building") {
            p.fg(hex::BLUE, prod)
        } else {
            prod
        };
        parts.push(prod);
        line(format!("  {}  {}", fmt::pad(&r.name, name_w), parts.join(" · ")));
    }
    if folded > 0 {
        line(format!(
            "  {}",
            p.fg(hex::COMMENT, format!("+ {}", fmt::count(folded as u64, "quiet project", "quiet projects")))
        ));
    }
    line(format!("{}{} building · {} failed in production today", label("deploys"), g.building, g.failed_today));
    for d in w.deploy_rows().into_iter().take(6) {
        let (glyph, colour) = match d.state() {
            "READY" => ("✓", hex::GREEN),
            "ERROR" => ("✗", hex::RED),
            "CANCELED" => ("■", hex::COMMENT),
            _ => ("▸", hex::BLUE),
        };
        let when = if d.running() {
            fmt::clock(fmt::between(d.building_at.unwrap_or(d.created), now))
        } else {
            fmt::ago(fmt::between(d.ready.unwrap_or(d.created), now))
        };
        let git = [d.branch(), d.message().map(|m| format!("\"{}\"", fmt::fit_end(&fmt::one_line(&m), 50)))]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        let current = if w.is_current(d) { " · current" } else { "" };
        line(format!(
            "  {} {} {} {} {when}{current} · {git}",
            p.fg(colour, glyph),
            d.name,
            d.environment(),
            d.state().to_lowercase()
        ));
    }
    if let (Some(team), Some(spend)) = (&w.team, &w.spend) {
        let usage = spend.usage();
        let credit = billing::credit(team)
            .map(|c| format!(" of the {} credit ({})", fmt::money(c), fmt::percent(usage / c.max(0.01))))
            .unwrap_or_default();
        let projected = spend
            .projected()
            .map(|v| format!(" · projected {} by {}", fmt::money(v), fmt::day(spend.period.end)))
            .unwrap_or_default();
        let through = spend.latest().map(|t| format!(" · charges to {}", fmt::day(t))).unwrap_or_default();
        line(format!("{}usage {}{credit}{projected}{through}", label("spend"), fmt::money(usage)));
        let top: Vec<String> =
            spend.services().into_iter().take(4).map(|(s, c)| format!("{s} {}", fmt::money(c))).collect();
        if !top.is_empty() {
            line(format!("  {}", top.join(" · ")));
        }
        let fixed: Vec<String> =
            billing::fixed(team).into_iter().map(|(n, c)| format!("{n} {}", fmt::money(c))).collect();
        if !fixed.is_empty() {
            line(format!("  {} a month", fixed.join(" · ")));
        }
    } else if let Some((_, e)) = w.failed.get(&Source::Billing) {
        line(format!("{}{e}", label("spend")));
    }
    if let Some(ai) = &w.ai.as_ref().filter(|a| a.requests_today > 0) {
        line(format!(
            "{}{} today · {}",
            label("AI"),
            fmt::money(ai.cost_today),
            fmt::count(ai.requests_today, "request", "requests")
        ));
    }
    if let Some(s) = &w.status {
        let said = if s.incidents.is_empty() {
            s.status.description.to_lowercase()
        } else {
            s.incidents.iter().map(|i| format!("{} ({})", i.name, i.status)).collect::<Vec<_>>().join(" · ")
        };
        line(format!("{}{said}", label("vercel")));
    }
    let warned: Vec<String> = w
        .domains
        .iter()
        .filter_map(|d| {
            if d.misconfigured == Some(true) {
                Some(format!("{} is misconfigured", d.name))
            } else {
                d.expires_at
                    .filter(|e| e - now < w.cfg.thresholds.domain_days as i64 * 86_400_000)
                    .map(|e| format!("{} expires {}", d.name, fmt::day(e)))
            }
        })
        .collect();
    if !warned.is_empty() {
        line(format!("{}{}", label("domains"), warned.join(" · ")));
    }
    for (source, (_, e)) in &w.failed {
        if *source != Source::Billing {
            line(format!("{}{}: {e}", label("couldn't"), source.name()));
        }
    }
    out
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

pub fn json(w: &World, now: i64) -> Value {
    let g = w.glance(now);
    let (rows, folded) = w.rows(now, true);
    let projects: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.id, "name": r.name, "web_analytics": r.analytics, "online": r.online,
                "per_minute": r.minutes, "last_hour": r.hour, "visitors_today": r.today,
                "error_rate": r.errors, "cache_hit_rate": r.cache, "blocked_last_hour": r.blocked, "under_attack": r.attack,
                "production": r.production.as_ref().map(|p| json!({"deployment": p.uid, "state": p.state, "at": p.at})),
            })
        })
        .collect();
    let deployments: Vec<Value> = w
        .deploy_rows()
        .into_iter()
        .take(20)
        .map(|d| {
            json!({
                "id": d.uid, "short": short(&d.uid), "project": d.name, "environment": d.environment(), "state": d.state(),
                "created": d.created, "ready": d.ready, "took_ms": d.took(now), "current": w.is_current(d),
                "branch": d.branch(), "commit": d.commit(), "message": d.message(), "author": d.author(),
                "url": d.url, "error": d.error_message,
            })
        })
        .collect();
    let spend = w.spend.as_ref().map(|s| {
        json!({
            "period": {"start": s.period.start, "end": s.period.end}, "usage": s.usage(), "subscriptions": s.subscriptions(),
            "billed": s.billed(), "other": s.other(),
            "credit": w.team.as_ref().and_then(billing::credit), "projected": s.projected(), "through": s.through(),
            "services": s.services().into_iter().map(|(n, c)| json!({"service": n, "cost": c})).collect::<Vec<_>>(),
            "projects": s.projects().into_iter().map(|(n, c)| json!({"project": n, "cost": c})).collect::<Vec<_>>(),
            "fixed": w.team.as_ref().map(billing::fixed).unwrap_or_default().into_iter().map(|(n, c)| json!({"item": n, "monthly": c})).collect::<Vec<_>>(),
        })
    });
    json!({
        "team": w.team.as_ref().map(|t| json!({"id": t.id, "slug": t.slug, "name": t.name, "plan": t.plan()})),
        "online": g.online, "visitors_today": g.visitors, "page_views_today": g.views,
        "building": g.building, "failed_in_production_today": g.failed_today,
        "projects": projects, "quiet_projects": folded,
        "deployments": deployments,
        "spend": spend,
        "ai_gateway": w.ai.as_ref().map(|a| json!({"cost_today": a.cost_today, "requests_today": a.requests_today})),
        "vercel_status": w.status.as_ref().map(|s| json!({"indicator": s.status.indicator, "description": s.status.description,
            "incidents": s.incidents.iter().map(|i| json!({"name": i.name, "status": i.status, "impact": i.impact})).collect::<Vec<_>>()})),
        "domains": w.domains.iter().map(|d| json!({"name": d.name, "expires_at": d.expires_at, "renews": d.renew, "misconfigured": d.misconfigured})).collect::<Vec<_>>(),
        "errors": w.failed.iter().map(|(s, (_, e))| json!({"source": s.name(), "error": e.to_string()})).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::NOW;

    #[test]
    fn a_glance_in_words_and_as_json() {
        let w = crate::demo::world(NOW);
        let said = text(&w, NOW, Paint::off());
        assert!(said.starts_with("ooze · night-shift (Pro) · "), "{said}");
        assert!(said.contains("moth-radio") && said.contains("production building"), "{said}");
        assert!(said.contains("production failed 5 min ago: Command \"npm run build\" exited with 1"), "{said}");
        assert!(
            said.contains("of the $20.00 credit") && said.contains("Pro plan $20.00 · extra seats $20.00 a month"),
            "{said}"
        );
        assert!(said.contains("+ 7 quiet projects"), "{said}");
        assert!(said.contains("night-market.shop expires"), "{said}");
        let j = json(&w, NOW);
        assert_eq!(j["team"]["slug"], "night-shift");
        assert!(j["deployments"].as_array().unwrap().iter().any(|d| d["state"] == "BUILDING"));
        assert!(j["spend"]["usage"].as_f64().unwrap() > j["spend"]["credit"].as_f64().unwrap());
    }
}
