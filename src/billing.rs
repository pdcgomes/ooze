//! Spend: billing charges folded into days, and the plan's fixed costs and
//! included credit.
//!
//! Charges come a day at a time, the latest being yesterday's, and a month of
//! them is tens of thousands of rows. Days are kept on disk once fetched;
//! each refresh asks again only for the last two, which Vercel may still be
//! adding to.

use crate::api::{Api, ApiError};
use crate::vercel::{self, Charge, Period, Team};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const DAY: i64 = 86_400_000;

/// A day's charges, in dollars.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Day {
    pub start: i64,
    pub end: i64,
    /// What metered usage cost, before credits: everything but the plan and seats.
    pub usage: f64,
    /// The plan and seats, charged a day at a time.
    pub subscriptions: f64,
    /// What Vercel has billed for, all charges.
    pub billed: f64,
    /// Purchases, adjustments and tax.
    pub other: f64,
    /// Credits applied, usually negative.
    pub credit: f64,
    pub services: BTreeMap<String, f64>,
    pub projects: BTreeMap<String, f64>,
    /// Marketplace providers other than Vercel.
    pub providers: BTreeMap<String, f64>,
}

pub fn fold(rows: impl IntoIterator<Item = Charge>) -> BTreeMap<i64, Day> {
    let mut days: BTreeMap<i64, Day> = BTreeMap::new();
    for c in rows {
        let Some(start) = vercel::parse_time(&c.charge_period_start) else { continue };
        let end = vercel::parse_time(&c.charge_period_end).unwrap_or(start + DAY);
        let day = days.entry(start).or_insert_with(|| Day { start, end, ..Day::default() });
        day.billed += c.billed_cost;
        let subscription = c.service_category.as_deref() == Some("Subscription Licenses");
        match c.charge_category.as_str() {
            "Usage" if subscription => day.subscriptions += c.effective_cost,
            "Usage" => {
                day.usage += c.effective_cost;
                *day.services.entry(c.service_name.clone()).or_default() += c.effective_cost;
                if let Some(p) = c.tags.get("ProjectName").filter(|p| !p.is_empty()) {
                    *day.projects.entry(p.clone()).or_default() += c.effective_cost;
                }
            }
            "Credit" => day.credit += c.effective_cost,
            _ => day.other += c.effective_cost,
        }
        if let Some(provider) = c.service_provider_name.filter(|p| !p.is_empty() && p != "Vercel") {
            *day.providers.entry(provider).or_default() += c.effective_cost;
        }
    }
    days
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Spend {
    pub team: String,
    pub period: Period,
    pub days: BTreeMap<i64, Day>,
    pub fetched: i64,
}

fn sorted(totals: BTreeMap<String, f64>) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = totals.into_iter().filter(|(_, c)| c.abs() >= 0.005).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    v
}

impl Spend {
    fn sum(&self, f: impl Fn(&Day) -> f64) -> f64 {
        self.days.values().map(f).sum::<f64>() + 0.0
    }

    pub fn usage(&self) -> f64 {
        self.sum(|d| d.usage)
    }

    pub fn subscriptions(&self) -> f64 {
        self.sum(|d| d.subscriptions)
    }

    pub fn billed(&self) -> f64 {
        self.sum(|d| d.billed)
    }

    pub fn other(&self) -> f64 {
        self.sum(|d| d.other)
    }

    /// The end of the latest day with charges.
    pub fn through(&self) -> Option<i64> {
        self.days.values().map(|d| d.end).max()
    }

    /// The start of the latest day with charges, the day to name.
    pub fn latest(&self) -> Option<i64> {
        self.days.keys().next_back().copied()
    }

    fn by(&self, f: impl Fn(&Day) -> &BTreeMap<String, f64>) -> Vec<(String, f64)> {
        let mut totals: BTreeMap<String, f64> = BTreeMap::new();
        for d in self.days.values() {
            for (k, v) in f(d) {
                *totals.entry(k.clone()).or_default() += v;
            }
        }
        sorted(totals)
    }

    pub fn services(&self) -> Vec<(String, f64)> {
        self.by(|d| &d.services)
    }

    pub fn projects(&self) -> Vec<(String, f64)> {
        self.by(|d| &d.projects)
    }

    pub fn providers(&self) -> Vec<(String, f64)> {
        self.by(|d| &d.providers)
    }

    pub fn ai_gateway(&self) -> f64 {
        self.services().iter().filter(|(s, _)| s.contains("AI Gateway")).map(|(_, c)| c).sum::<f64>() + 0.0
    }

    /// Usage a day, oldest first.
    pub fn daily(&self) -> Vec<f64> {
        self.days.values().map(|d| d.usage).collect()
    }

    /// Usage by the end of the period, at the rate so far.
    pub fn projected(&self) -> Option<f64> {
        let through = self.through()?;
        let elapsed = (through - self.period.start) as f64 / DAY as f64;
        let total = (self.period.end - self.period.start) as f64 / DAY as f64;
        (elapsed >= 1.0).then(|| self.usage() / elapsed * total)
    }
}

/// The plan's included usage credit, in dollars.
pub fn credit(team: &Team) -> Option<f64> {
    team.billing.as_ref()?.invoice_items.get("includedAllocationUsd")?.quantity
}

/// The plan, seats and add-ons the team pays for each month, in dollars.
pub fn fixed(team: &Team) -> Vec<(String, f64)> {
    let Some(b) = &team.billing else { return vec![] };
    let mut v: Vec<(String, f64)> = b
        .invoice_items
        .iter()
        .filter(|(k, i)| k.as_str() != "includedAllocationUsd" && i.batch.is_none() && !i.hidden)
        .filter_map(|(k, i)| {
            let (price, qty) = (i.price?, i.quantity?);
            (price > 0.0 && qty > 0.0).then(|| (item_name(k, b.plan.as_deref()), price / 100.0 * qty))
        })
        .collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v
}

fn item_name(key: &str, plan: Option<&str>) -> String {
    match key {
        k if Some(k) == plan => format!("{} plan", capitalised(k)),
        "teamSeats" => "extra seats".into(),
        "analytics" | "webAnalytics" => "Web Analytics Plus".into(),
        "observabilityPlus" => "Observability Plus".into(),
        "monitoring" => "Monitoring".into(),
        "concurrentBuilds" => "concurrent builds".into(),
        other => {
            let mut words = String::new();
            for c in other.chars() {
                if c.is_uppercase() {
                    words.push(' ');
                }
                words.extend(c.to_lowercase());
            }
            words
        }
    }
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn cache_file(dir: &Path, team: &str) -> PathBuf {
    dir.join(format!("charges-2-{team}.json"))
}

/// Charges for the period so far: the days kept from before, and the last
/// two asked for again.
pub fn refresh(api: &Api, team: &str, period: Period, cache: &Path, now: i64) -> Result<Spend, ApiError> {
    let file = cache_file(cache, team);
    let kept: Option<Spend> = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let mut spend = kept.filter(|s| s.period == period && s.team == team).unwrap_or(Spend { team: team.into(), period, ..Spend::default() });
    let from = spend.days.keys().rev().nth(1).copied().unwrap_or(period.start).max(period.start);
    let mut rows = Vec::new();
    vercel::charges(api, from, now, |c| rows.push(c))?;
    spend.days.retain(|start, _| *start < from);
    spend.days.extend(fold(rows));
    spend.fetched = now;
    let _ = std::fs::create_dir_all(cache);
    if let Ok(bytes) = serde_json::to_vec(&spend) {
        let _ = std::fs::write(&file, bytes);
    }
    Ok(spend)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn charge(day: &str, category: &str, service: &str, project: Option<&str>, effective: f64) -> Charge {
        let licence = ["Pro", "Additional Team Seats"].contains(&service);
        let mut c: Charge = serde_json::from_value(json!({
            "BilledCost": 0.0, "EffectiveCost": effective, "ChargeCategory": category,
            "ChargePeriodStart": format!("{day}T07:00:00.000Z"), "ChargePeriodEnd": "", "ServiceName": service,
            "ServiceCategory": if licence { "Subscription Licenses" } else { "Build & Deploy" },
            "ServiceProviderName": "Vercel", "Tags": {}
        }))
        .unwrap();
        if let Some(p) = project {
            c.tags.insert("ProjectName".into(), p.into());
        }
        c
    }

    pub fn spend() -> Spend {
        let start = vercel::parse_time("2026-10-03T07:00:00.000Z").unwrap();
        let rows = vec![
            charge("2026-10-03", "Usage", "Fast Data Transfer", Some("moth-radio"), 4.0),
            charge("2026-10-03", "Usage", "Fluid Active CPU", Some("cave-notes"), 2.0),
            charge("2026-10-04", "Usage", "Fast Data Transfer", Some("moth-radio"), 3.0),
            charge("2026-10-04", "Usage", "AI Gateway", None, 1.5),
            charge("2026-10-04", "Purchase", "Domain", None, 12.0),
            charge("2026-10-04", "Usage", "Pro", None, 0.65),
        ];
        Spend { team: "team_1".into(), period: Period { start, end: start + 31 * DAY }, days: fold(rows), fetched: 0 }
    }

    #[test]
    fn a_period_adds_up_by_service_project_and_day() {
        let s = spend();
        assert_eq!(s.usage(), 10.5);
        assert_eq!(s.other(), 12.0, "a domain bought isn't usage");
        assert_eq!(s.subscriptions(), 0.65, "nor is the plan, charged a day at a time");
        assert_eq!(s.services()[0], ("Fast Data Transfer".to_string(), 7.0));
        assert_eq!(s.projects(), [("moth-radio".to_string(), 7.0), ("cave-notes".to_string(), 2.0)]);
        assert_eq!(s.ai_gateway(), 1.5);
        assert_eq!(s.daily(), [6.0, 4.5]);
        assert_eq!(s.through(), Some(s.period.start + 2 * DAY));
        assert_eq!(s.projected(), Some(10.5 / 2.0 * 31.0), "two days in, at the same rate for 31");
    }

    #[test]
    fn the_credit_and_the_fixed_costs_come_from_the_plan() {
        let team: Team = serde_json::from_value(json!({
            "id": "team_1", "slug": "night-shift",
            "billing": {"plan": "pro", "invoiceItems": {
                "pro": {"price": 2000, "quantity": 1, "createdAt": 1},
                "teamSeats": {"price": 2000, "quantity": 2},
                "analytics": {"price": 1000, "quantity": 0},
                "includedAllocationUsd": {"price": 0, "quantity": 20},
                "edgeRequest": {"price": 0.0002, "batch": 1, "threshold": 0}
            }}
        }))
        .unwrap();
        assert_eq!(credit(&team), Some(20.0));
        assert_eq!(fixed(&team), [("extra seats".to_string(), 40.0), ("Pro plan".to_string(), 20.0)]);
    }

    #[test]
    fn days_already_fetched_are_kept_and_the_last_two_asked_again() {
        let s = spend();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(cache_file(dir.path(), "team_1"), serde_json::to_vec(&s).unwrap()).unwrap();
        let rows = [charge("2026-10-04", "Usage", "Fast Data Transfer", Some("moth-radio"), 5.0), charge("2026-10-05", "Usage", "Edge Requests", None, 1.0)];
        let body = rows.iter().map(|r| serde_json::to_string(r).unwrap()).collect::<Vec<_>>().join("\n");
        let f = crate::api::tests::fake(vec![(200, vec![], body)]);
        let now = s.period.start + 3 * DAY;
        let got = refresh(&crate::api::tests::api(&f.base), "team_1", s.period, dir.path(), now).unwrap();
        let asked = f.seen.lock().unwrap()[0].clone();
        assert!(asked.contains("from=2026-10-03T07%3A00%3A00.000Z"), "the second-latest day kept: {asked}");
        assert_eq!(got.daily(), [5.0, 1.0], "the refetched days replace what was kept from them");
    }
}
