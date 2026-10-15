//! Vercel's REST API as ooze uses it: the answers it reads, typed loosely so
//! a field Vercel adds or drops doesn't break anything, and the calls.

use crate::api::{Api, ApiError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

type Result<T> = std::result::Result<T, ApiError>;

pub const STATUS_PAGE: &str = "https://www.vercel-status.com/api/v2/summary.json";

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct User {
    pub id: String,
    pub username: String,
    pub name: Option<String>,
    pub default_team_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Team {
    pub id: String,
    pub slug: String,
    pub name: Option<String>,
    pub billing: Option<TeamBilling>,
}

impl Team {
    pub fn title(&self) -> &str {
        self.name.as_deref().filter(|n| !n.is_empty()).unwrap_or(&self.slug)
    }

    pub fn plan(&self) -> Option<&str> {
        self.billing.as_ref().and_then(|b| b.plan.as_deref())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct TeamBilling {
    pub plan: Option<String>,
    pub period: Option<Period>,
    pub status: Option<String>,
    pub invoice_items: BTreeMap<String, InvoiceItem>,
    pub controls: Option<Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct Period {
    pub start: i64,
    pub end: i64,
}

/// A line of the team's plan. Fixed ones (the plan, seats, add-ons) have a
/// quantity and a monthly price in cents; metered ones have a batch.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct InvoiceItem {
    pub price: Option<f64>,
    pub quantity: Option<f64>,
    pub batch: Option<f64>,
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub framework: Option<String>,
    pub web_analytics: Option<Toggle>,
    pub targets: Option<Targets>,
    /// The last promote or roll back: where production's domains were pointed.
    pub last_alias_request: Option<AliasRequest>,
    pub live: bool,
    pub paused: bool,
    pub updated_at: Option<i64>,
}

impl Project {
    pub fn analytics(&self) -> bool {
        self.web_analytics.as_ref().is_some_and(|w| w.enabled_at.is_some() && w.disabled_at.is_none())
    }

    pub fn production(&self) -> Option<&ProjectDeployment> {
        self.targets.as_ref().and_then(|t| t.production.as_ref())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Toggle {
    pub enabled_at: Option<i64>,
    pub disabled_at: Option<i64>,
    pub has_data: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AliasRequest {
    pub to_deployment_id: Option<String>,
    pub from_deployment_id: Option<String>,
    pub job_status: Option<String>,
    pub requested_at: Option<i64>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Targets {
    pub production: Option<ProjectDeployment>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectDeployment {
    pub id: String,
    pub url: Option<String>,
    pub ready_state: Option<String>,
    pub created_at: Option<i64>,
    pub alias: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Deployment {
    pub uid: String,
    /// The project's name.
    pub name: String,
    pub project_id: Option<String>,
    pub url: Option<String>,
    pub created: i64,
    pub building_at: Option<i64>,
    pub ready: Option<i64>,
    pub state: Option<String>,
    pub ready_state: Option<String>,
    pub target: Option<String>,
    pub creator: Option<Creator>,
    pub meta: HashMap<String, Value>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub inspector_url: Option<String>,
    pub ready_substate: Option<String>,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Creator {
    pub uid: String,
    pub username: Option<String>,
    pub github_login: Option<String>,
}

impl Deployment {
    pub fn state(&self) -> &str {
        self.state.as_deref().or(self.ready_state.as_deref()).unwrap_or("QUEUED")
    }

    pub fn production(&self) -> bool {
        self.target.as_deref() == Some("production")
    }

    pub fn environment(&self) -> &str {
        self.target.as_deref().unwrap_or("preview")
    }

    pub fn running(&self) -> bool {
        matches!(self.state(), "BUILDING" | "QUEUED" | "INITIALIZING")
    }

    fn git(&self, what: &str) -> Option<String> {
        ["github", "gitlab", "bitbucket"]
            .iter()
            .find_map(|p| self.meta.get(&format!("{p}{what}")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()))
            .map(str::to_string)
    }

    pub fn branch(&self) -> Option<String> {
        self.git("CommitRef")
    }

    pub fn commit(&self) -> Option<String> {
        self.git("CommitSha")
    }

    pub fn message(&self) -> Option<String> {
        self.git("CommitMessage")
    }

    pub fn author(&self) -> Option<String> {
        self.git("CommitAuthorLogin")
            .or_else(|| self.git("CommitAuthorName"))
            .or_else(|| self.creator.as_ref().and_then(|c| c.username.clone().or(c.github_login.clone())))
    }

    /// How long it built for, or has been building.
    pub fn took(&self, now: i64) -> i64 {
        let start = self.building_at.unwrap_or(self.created);
        let end = if self.running() { now } else { self.ready.unwrap_or(now) };
        (end - start).max(0)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Domain {
    pub name: String,
    pub expires_at: Option<i64>,
    pub renew: Option<bool>,
    pub service_type: Option<String>,
    pub verified: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct DomainConfig {
    pub misconfigured: bool,
    pub configured_by: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Event {
    pub id: String,
    pub text: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub created_at: i64,
    pub user: Option<EventUser>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct EventUser {
    pub username: Option<String>,
}

/// One of Vercel's anomaly alerts, grouped as its CLI shows them.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AlertGroup {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub status: Option<String>,
    pub recorded_started_at: Option<f64>,
    pub project_id: Option<String>,
    pub alerts: Vec<Alert>,
    pub ai: Option<AlertAi>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Alert {
    pub started_at: Option<f64>,
    pub resolved_at: Option<f64>,
    pub project_id: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AlertAi {
    pub title: Option<String>,
    pub current_summary: Option<String>,
    pub key_findings: Vec<String>,
}

/// Seconds or milliseconds, to milliseconds.
pub fn millis(t: f64) -> i64 {
    if t < 1e12 { (t * 1000.0) as i64 } else { t as i64 }
}

impl AlertGroup {
    pub fn title(&self) -> String {
        self.ai.as_ref().and_then(|a| a.title.clone()).unwrap_or_else(|| "An alert".into())
    }

    pub fn started(&self) -> Option<i64> {
        self.recorded_started_at.or(self.alerts.first().and_then(|a| a.started_at)).map(millis)
    }

    pub fn active(&self) -> bool {
        self.status.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("active"))
    }

    pub fn project(&self) -> Option<&str> {
        self.project_id.as_deref().or(self.alerts.iter().find_map(|a| a.project_id.as_deref()))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Anomaly {
    pub project_id: Option<String>,
    pub state: Option<String>,
    pub start_time: Option<f64>,
    pub end_time: Option<f64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Budget {
    pub name: Option<String>,
    pub scope_type: String,
    pub scope_id: String,
    pub current_spend: f64,
    pub limit_amount: f64,
    pub refresh_period: Option<String>,
    pub active: bool,
    pub archived: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct StatusPage {
    pub status: Indicator,
    pub incidents: Vec<Incident>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Indicator {
    pub indicator: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Incident {
    pub id: String,
    pub name: String,
    pub status: String,
    pub impact: String,
    pub shortlink: Option<String>,
    pub updated_at: Option<String>,
}

/// A row of billing charges, FOCUS 1.3.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default, rename_all = "PascalCase")]
pub struct Charge {
    pub billed_cost: f64,
    pub effective_cost: f64,
    pub charge_category: String,
    pub charge_period_start: String,
    pub charge_period_end: String,
    pub service_name: String,
    pub service_category: Option<String>,
    pub service_provider_name: Option<String>,
    pub consumed_quantity: Option<f64>,
    pub consumed_unit: Option<String>,
    pub tags: HashMap<String, String>,
}

/// A line of a build's log.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct LogEvent {
    #[serde(rename = "type")]
    pub kind: String,
    pub created: Option<i64>,
    pub payload: LogPayload,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct LogPayload {
    pub text: Option<String>,
    pub date: Option<i64>,
}

/// What an observability query covers: the whole team, or some projects.
#[derive(Clone, Debug, PartialEq)]
pub enum Scope {
    Team(String),
    Projects(String, Vec<String>),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Query {
    pub metric: &'static str,
    pub aggregation: &'static str,
    pub group_by: Vec<&'static str>,
    pub filter: Option<String>,
    pub start: i64,
    pub end: i64,
    pub minutes: i64,
    pub limit: u32,
    pub scope: Scope,
    /// Aligns day buckets to this time zone's midnight.
    pub timezone: Option<String>,
}

impl Query {
    pub fn new(metric: &'static str, aggregation: &'static str, scope: Scope, start: i64, end: i64, minutes: i64) -> Query {
        Query { metric, aggregation, group_by: vec![], filter: None, start, end, minutes, limit: 100, scope, timezone: None }
    }

    pub fn by(mut self, dimension: &'static str) -> Query {
        self.group_by.push(dimension);
        self
    }

    pub fn filter(mut self, f: impl Into<String>) -> Query {
        self.filter = Some(f.into());
        self
    }

    fn body(&self) -> Value {
        let iso = |ms: i64| chrono::DateTime::from_timestamp_millis(ms).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        let scope = match &self.scope {
            Scope::Team(owner) => json!({ "type": "owner", "ownerId": owner }),
            Scope::Projects(owner, ids) => json!({ "type": "project", "ownerId": owner, "projectIds": ids }),
        };
        let granularity = match self.minutes {
            m if m >= 1440 && m % 1440 == 0 => json!({ "days": m / 1440 }),
            m if m >= 60 && m % 60 == 0 => json!({ "hours": m / 60 }),
            m => json!({ "minutes": m }),
        };
        let mut body = json!({
            "scope": scope,
            "metric": self.metric,
            "aggregation": self.aggregation,
            "startTime": iso(self.start),
            "endTime": iso(self.end),
            "granularity": granularity,
            "limit": self.limit,
        });
        if !self.group_by.is_empty() {
            body["groupBy"] = json!(self.group_by);
        }
        if let Some(f) = &self.filter {
            body["filter"] = json!(f);
        }
        if let Some(tz) = &self.timezone {
            body["bucketTimezone"] = json!(tz);
        }
        body
    }

    /// The column Vercel puts the number in: `unique/visitor_id` on page views
    /// comes back as `vercel_analytics_pageview_count_unique_visitor_id`.
    fn column(&self) -> String {
        format!("{}_{}", self.metric.replace('.', "_"), self.aggregation.replace('/', "_"))
    }
}

/// A number from an observability query: in a time bucket (`at`) or over
/// the whole range (`at` is None), for a group.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Point {
    pub at: Option<i64>,
    pub value: f64,
    pub groups: BTreeMap<String, String>,
}

impl Point {
    pub fn group(&self, dimension: &str) -> &str {
        self.groups.get(dimension).map_or("", String::as_str)
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Observed {
    pub data: Vec<Point>,
    pub summary: Vec<Point>,
}

fn points(rows: &Value, column: &str) -> Vec<Point> {
    let Some(rows) = rows.as_array() else { return vec![] };
    rows.iter()
        .filter_map(|row| {
            let obj = row.as_object()?;
            let mut p = Point::default();
            for (k, v) in obj {
                if k == "timestamp" {
                    p.at = v.as_str().and_then(parse_time).or(v.as_i64());
                } else if k == column {
                    p.value = v.as_f64().or_else(|| v.as_str().and_then(|s| s.parse().ok())).unwrap_or(0.0);
                } else if let Some(s) = v.as_str() {
                    p.groups.insert(k.clone(), s.to_string());
                } else if v.is_number() || v.is_boolean() {
                    p.groups.insert(k.clone(), v.to_string());
                }
            }
            Some(p)
        })
        .collect()
}

pub fn parse_time(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|d| d.timestamp_millis())
}

pub fn iso(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms).map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)).unwrap_or_default()
}

pub fn observe(api: &Api, q: &Query) -> Result<Observed> {
    let v: Value = api.post("/v2/observability/query", &[], &q.body())?;
    let col = q.column();
    Ok(Observed { data: points(&v["data"], &col), summary: points(&v["summary"], &col) })
}

pub fn user(api: &Api) -> Result<User> {
    let v: Value = api.for_team(None).get("/v2/user", &[])?;
    serde_json::from_value(v["user"].clone()).map_err(|e| ApiError::Decode(e.to_string()))
}

pub fn teams(api: &Api) -> Result<Vec<Team>> {
    let v: Value = api.for_team(None).get("/v2/teams", &[("limit", "100".into())])?;
    serde_json::from_value(v["teams"].clone()).map_err(|e| ApiError::Decode(e.to_string()))
}

pub fn team(api: &Api, id: &str) -> Result<Team> {
    api.for_team(None).get(&format!("/v2/teams/{id}"), &[])
}

pub fn projects(api: &Api) -> Result<Vec<Project>> {
    let mut all = Vec::new();
    let mut from: Option<String> = None;
    for _ in 0..10 {
        let mut q = vec![("limit", "100".to_string())];
        if let Some(f) = &from {
            q.push(("from", f.clone()));
        }
        let v: Value = api.get("/v10/projects", &q)?;
        let page: Vec<Project> = serde_json::from_value(v["projects"].clone()).map_err(|e| ApiError::Decode(e.to_string()))?;
        let done = page.len() < 100;
        all.extend(page);
        from = v["pagination"]["next"].as_i64().map(|n| n.to_string()).or(v["pagination"]["next"].as_str().map(str::to_string));
        if done || from.is_none() {
            break;
        }
    }
    Ok(all)
}

pub fn deployments(api: &Api, limit: u32) -> Result<Vec<Deployment>> {
    let v: Value = api.get("/v7/deployments", &[("limit", limit.to_string())])?;
    serde_json::from_value(v["deployments"].clone()).map_err(|e| ApiError::Decode(e.to_string()))
}

pub fn domains(api: &Api) -> Result<Vec<Domain>> {
    let v: Value = api.get("/v5/domains", &[("limit", "100".into())])?;
    serde_json::from_value(v["domains"].clone()).map_err(|e| ApiError::Decode(e.to_string()))
}

pub fn domain_config(api: &Api, name: &str) -> Result<DomainConfig> {
    api.get(&format!("/v6/domains/{name}/config"), &[])
}

pub fn events(api: &Api, since: i64, limit: u32) -> Result<Vec<Event>> {
    let v: Value = api.get("/v3/events", &[("limit", limit.to_string()), ("since", iso(since))])?;
    serde_json::from_value(v["events"].clone()).map_err(|e| ApiError::Decode(e.to_string()))
}

pub fn alerts(api: &Api, from: i64, to: i64) -> Result<Vec<AlertGroup>> {
    let v: Value = api.get("/alerts/v3/groups", &[("from", iso(from)), ("to", iso(to)), ("limit", "20".into())])?;
    let list = if v.is_array() { v } else { v["groups"].clone() };
    serde_json::from_value(list).map_err(|e| ApiError::Decode(e.to_string()))
}

pub fn attacks(api: &Api, project: &str) -> Result<Vec<Anomaly>> {
    let v: Value = api.get("/v1/security/firewall/attack-status", &[("projectId", project.to_string())])?;
    Ok(serde_json::from_value(v["anomalies"].clone()).unwrap_or_default())
}

pub fn budgets(api: &Api) -> Result<Vec<Budget>> {
    let v: Value = api.get("/ai-gateway/budgets/list", &[])?;
    serde_json::from_value(v["budgets"].clone()).map_err(|e| ApiError::Decode(e.to_string()))
}

pub fn status_page() -> Result<StatusPage> {
    crate::api::public(STATUS_PAGE)
}

/// Page views and visitors from Web Analytics, over whole UTC days.
pub fn analytics_count(api: &Api, project: &str, since: i64, until: i64) -> Result<(u64, u64)> {
    let q = [("projectId", project.to_string()), ("since", since.to_string()), ("until", until.to_string())];
    let v: Value = api.get("/v1/query/web-analytics/visits/count", &q)?;
    Ok((v["data"]["pageviews"].as_f64().unwrap_or(0.0) as u64, v["data"]["visitors"].as_f64().unwrap_or(0.0) as u64))
}

/// The billing charges between two times, a row at a time.
pub fn charges(api: &Api, from: i64, to: i64, mut each: impl FnMut(Charge)) -> Result<()> {
    let q = [("from", iso(from)), ("to", iso(to))];
    let mut bad = None;
    api.lines("/v1/billing/charges", &q, Some(std::time::Duration::from_secs(180)), |line| {
        match serde_json::from_str::<Charge>(line) {
            Ok(c) => each(c),
            Err(e) => bad = Some(e.to_string()),
        }
        true
    })?;
    match bad {
        Some(e) => Err(ApiError::Decode(format!("a billing charge: {e}"))),
        None => Ok(()),
    }
}

/// A build's log: the last lines, or every line as it comes while `follow`.
pub fn build_log(api: &Api, deployment: &str, follow: bool, mut each: impl FnMut(LogEvent) -> bool) -> Result<()> {
    let mut q = vec![("builds", "1".to_string()), ("direction", "forward".to_string()), ("limit", "-1".to_string())];
    if follow {
        q.push(("follow", "1".into()));
    }
    let timeout = if follow { None } else { Some(std::time::Duration::from_secs(60)) };
    api.lines(&format!("/v3/deployments/{deployment}/events"), &q, timeout, |line| {
        // Without follow it's one array; with it, an event per line.
        if let Ok(list) = serde_json::from_str::<Vec<LogEvent>>(line) {
            return list.into_iter().all(&mut each);
        }
        match serde_json::from_str::<LogEvent>(line) {
            Ok(e) => each(e),
            Err(_) => true,
        }
    })
}

pub fn cancel(api: &Api, deployment: &str) -> Result<Deployment> {
    api.patch(&format!("/v12/deployments/{deployment}/cancel"), &[], None)
}

pub fn redeploy(api: &Api, d: &Deployment) -> Result<Deployment> {
    let mut body = json!({ "name": d.name, "deploymentId": d.uid });
    if let Some(project) = &d.project_id {
        body["project"] = json!(project);
    }
    if let Some(target) = &d.target {
        body["target"] = json!(target);
    }
    api.post("/v13/deployments", &[("forceNew", "1".into())], &body)
}

pub fn promote(api: &Api, project: &str, deployment: &str) -> Result<()> {
    api.post::<Value>(&format!("/v10/projects/{project}/promote/{deployment}"), &[], &json!({})).map(|_| ())
}

pub fn rollback(api: &Api, project: &str, deployment: &str) -> Result<()> {
    api.post::<Value>(&format!("/v1/projects/{project}/rollback/{deployment}"), &[], &json!({})).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::tests::{api, fake};

    #[test]
    fn an_observability_query_asks_for_what_it_says_and_reads_its_buckets() {
        let answer = r#"{
            "summary": [{"vercel_analytics_pageview_count_unique_visitor_id": 18, "project_id": "prj_a"}],
            "data": [
                {"timestamp": "2026-10-15T17:39:00.000Z", "vercel_analytics_pageview_count_unique_visitor_id": 7, "project_id": "prj_a"},
                {"timestamp": "2026-10-15T17:40:00.000Z", "vercel_analytics_pageview_count_unique_visitor_id": null, "project_id": "prj_a"}
            ],
            "statistics": {"rowsRead": 10}
        }"#;
        let f = fake(vec![(200, vec![], answer.into())]);
        let q = Query::new("vercel.analytics_pageview.count", "unique/visitor_id", Scope::Team("team_1".into()), 1_792_086_000_000, 1_792_086_300_000, 5)
            .by("project_id");
        let got = observe(&api(&f.base), &q).unwrap();
        assert_eq!(got.summary.len(), 1);
        assert_eq!((got.summary[0].value, got.summary[0].group("project_id")), (18.0, "prj_a"));
        assert_eq!(got.data[0].at, parse_time("2026-10-15T17:39:00.000Z"));
        assert_eq!(got.data[1].value, 0.0, "an empty bucket is nothing");
        let seen = f.seen.lock().unwrap()[0].clone();
        let body: Value = serde_json::from_str(seen.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["scope"], json!({"type": "owner", "ownerId": "team_1"}));
        assert_eq!(body["granularity"], json!({"minutes": 5}));
        assert_eq!(body["groupBy"], json!(["project_id"]));
        assert_eq!(body["startTime"], "2026-10-15T17:40:00.000Z");
    }

    #[test]
    fn day_buckets_and_hour_buckets_are_said_as_such() {
        let q = |m| Query::new("m", "sum", Scope::Projects("t".into(), vec!["p".into()]), 0, 1, m).body();
        assert_eq!(q(1440)["granularity"], json!({"days": 1}));
        assert_eq!(q(60)["granularity"], json!({"hours": 1}));
        assert_eq!(q(1)["scope"]["projectIds"], json!(["p"]));
    }

    #[test]
    fn a_deployment_says_its_commit_whichever_git_host_it_came_from() {
        let d: Deployment = serde_json::from_value(json!({
            "uid": "dpl_1", "name": "moth-radio", "created": 1000, "buildingAt": 2000, "ready": 74000,
            "state": "READY", "target": "production",
            "meta": {"gitlabCommitRef": "main", "gitlabCommitSha": "a1b2c3d4e5", "gitlabCommitMessage": "Fix the player\n\nIt skipped",
                     "gitlabCommitAuthorName": "Pat", "something": 1},
            "creator": {"uid": "u1", "username": "pat-v"}
        }))
        .unwrap();
        assert_eq!((d.branch().as_deref(), d.message().as_deref(), d.author().as_deref()), (Some("main"), Some("Fix the player\n\nIt skipped"), Some("Pat")));
        assert_eq!(d.took(99_000), 72_000);
        assert!(d.production() && !d.running());
        let queued: Deployment = serde_json::from_value(json!({"uid": "dpl_2", "name": "x", "created": 5})).unwrap();
        assert_eq!((queued.state(), queued.environment(), queued.author()), ("QUEUED", "preview", None));
    }

    #[test]
    fn projects_know_whether_web_analytics_is_on() {
        let p: Project = serde_json::from_value(json!({"id": "p", "name": "n", "webAnalytics": {"id": "w", "enabledAt": 1}})).unwrap();
        assert!(p.analytics());
        let off: Project = serde_json::from_value(json!({"id": "p", "name": "n", "webAnalytics": {"enabledAt": 1, "disabledAt": 2}})).unwrap();
        assert!(!off.analytics());
    }

    #[test]
    fn a_build_log_reads_as_one_array_or_a_line_at_a_time() {
        let f = fake(vec![
            (200, vec![], r#"[{"type":"stdout","created":1,"payload":{"text":"Cloning"}},{"type":"stdout","payload":{"text":"Done"}}]"#.into()),
            (200, vec![], "{\"type\":\"stdout\",\"payload\":{\"text\":\"one\"}}\n{\"type\":\"stderr\",\"payload\":{\"text\":\"two\"}}\n".into()),
        ]);
        let api = api(&f.base);
        let mut got = vec![];
        build_log(&api, "dpl_1", false, |e| {
            got.push(e.payload.text.unwrap_or_default());
            true
        })
        .unwrap();
        build_log(&api, "dpl_1", true, |e| {
            got.push(e.payload.text.unwrap_or_default());
            true
        })
        .unwrap();
        assert_eq!(got, ["Cloning", "Done", "one", "two"]);
        assert!(f.seen.lock().unwrap()[1].contains("follow=1"));
    }

    #[test]
    fn alerts_come_as_a_list_or_inside_groups() {
        let one = r#"[{"id":"g1","type":"error_anomaly","status":"active","recordedStartedAt":1792080000,"ai":{"title":"5xx spike on /api"}}]"#;
        let f = fake(vec![(200, vec![], one.into()), (200, vec![], format!("{{\"groups\":{one}}}"))]);
        let api = api(&f.base);
        for _ in 0..2 {
            let g = alerts(&api, 0, 1).unwrap();
            assert_eq!((g[0].title(), g[0].active(), g[0].started()), ("5xx spike on /api".into(), true, Some(1_792_080_000_000)));
        }
    }

    #[test]
    fn charges_are_read_a_row_at_a_time() {
        let rows = [
            json!({"BilledCost": 0.0, "EffectiveCost": 1.25, "ChargeCategory": "Usage", "ChargePeriodStart": "2026-10-14T07:00:00Z",
                   "ChargePeriodEnd": "2026-10-15T07:00:00Z", "ServiceName": "Fast Data Transfer", "Tags": {"ProjectName": "moth-radio"}}),
            json!({"BilledCost": 0.0, "EffectiveCost": 0.5, "ChargeCategory": "Usage", "ChargePeriodStart": "2026-10-14T07:00:00Z",
                   "ChargePeriodEnd": "2026-10-15T07:00:00Z", "ServiceName": "Edge Requests", "Tags": {}}),
        ];
        let body = rows.iter().map(|r| r.to_string()).collect::<Vec<_>>().join("\n");
        let f = fake(vec![(200, vec![("content-type", "application/jsonl".into())], body)]);
        let mut got = vec![];
        charges(&api(&f.base), 0, 86_400_000, |c| got.push(c)).unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!((got[0].service_name.as_str(), got[0].effective_cost, got[0].tags["ProjectName"].as_str()), ("Fast Data Transfer", 1.25, "moth-radio"));
    }
}
