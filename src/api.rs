//! Talking to Vercel over HTTP: the token on each request, the team on each
//! query, what the rate limits say, and errors in words.

use crate::auth::Auth;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::io::BufRead;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const BASE: &str = "https://api.vercel.com";
const TIMEOUT: Duration = Duration::from_secs(30);
/// Bigger than ureq's 10 MB, for a month of billing charges.
const MOST: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq)]
pub enum ApiError {
    /// Vercel refused the token.
    Unauthorized(String),
    /// The token can't do this.
    Forbidden(String),
    NotFound(String),
    /// Rate limited until this time, in milliseconds, and what Vercel said.
    Limited(i64, String),
    /// The team's daily cap on observability queries is reached: every ooze
    /// waits until this time before one asks again.
    Capped(i64, String),
    /// ooze's own daily budget of observability queries is spent until this time.
    Budget(i64),
    Status(u16, String),
    Network(String),
    Decode(String),
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            ApiError::Unauthorized(why) => write!(f, "Vercel refused the token ({why})"),
            ApiError::Forbidden(why) => write!(f, "not allowed: {why}"),
            ApiError::NotFound(why) => write!(f, "not found: {why}"),
            ApiError::Limited(until, why) => {
                let wait = (until - chrono::Utc::now().timestamp_millis()).max(0) / 1000;
                if why.is_empty() {
                    write!(f, "rate limited for {wait} s more")
                } else {
                    write!(f, "rate limited for {wait} s more: {why}")
                }
            }
            ApiError::Capped(until, _) => {
                write!(
                    f,
                    "Vercel's daily limit on this team's observability queries is reached; trying again at {}",
                    crate::fmt::time(*until)
                )
            }
            ApiError::Budget(until) => {
                write!(f, "ooze's daily budget of observability queries is spent; more at {}", crate::fmt::time(*until))
            }
            ApiError::Status(code, why) => write!(f, "Vercel answered {code}: {why}"),
            ApiError::Network(why) => write!(f, "can't reach Vercel: {why}"),
            ApiError::Decode(why) => write!(f, "Vercel answered something ooze can't read: {why}"),
        }
    }
}

impl std::error::Error for ApiError {}

/// What a kind of request's rate limit said last.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Limit {
    pub limit: u64,
    pub remaining: u64,
    /// When it fills up again, in milliseconds.
    pub reset: i64,
}

#[derive(Clone)]
pub struct Api {
    agent: ureq::Agent,
    auth: Arc<Auth>,
    base: String,
    /// The team every request is on behalf of.
    pub team: Option<String>,
    limits: Arc<Mutex<BTreeMap<String, Limit>>>,
}

/// An HTTP agent going through the proxy the environment names, except to
/// this machine.
pub fn agent_for(base: &str) -> ureq::Agent {
    let local = ["://127.0.0.1", "://localhost", "://[::1]"].iter().any(|h| base.contains(h));
    let config = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(concat!("ooze/", env!("CARGO_PKG_VERSION"), " (+https://github.com/pdcgomes/ooze)"));
    let config = if local { config.proxy(None) } else { config };
    config.build().into()
}

pub fn agent() -> ureq::Agent {
    agent_for(BASE)
}

/// Requests are counted against limits by kind: the first two parts of the path.
pub fn bucket(path: &str) -> String {
    path.trim_start_matches('/').split('/').take(2).collect::<Vec<_>>().join("/")
}

impl Api {
    pub fn new(auth: Arc<Auth>, base: &str) -> Api {
        Api {
            agent: agent_for(base),
            auth,
            base: base.trim_end_matches('/').to_string(),
            team: None,
            limits: Arc::default(),
        }
    }

    pub fn for_team(&self, team: Option<String>) -> Api {
        Api { team, ..self.clone() }
    }

    pub fn auth(&self) -> &Auth {
        &self.auth
    }

    pub fn limits(&self) -> BTreeMap<String, Limit> {
        self.limits.lock().unwrap().clone()
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        timeout: Option<Duration>,
    ) -> Result<ureq::http::Response<ureq::Body>, ApiError> {
        let token = self.auth.token().map_err(|e| ApiError::Unauthorized(format!("{e:#}")))?;
        let resp = self.send(method, path, query, body, timeout, &token)?;
        if resp.status().as_u16() == 401
            && let Some(renewed) = self.auth.renew_after_refusal()
        {
            return self.send(method, path, query, body, timeout, &renewed).and_then(|r| self.check(path, r));
        }
        self.check(path, resp)
    }

    fn send(
        &self,
        method: &str,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        timeout: Option<Duration>,
        token: &str,
    ) -> Result<ureq::http::Response<ureq::Body>, ApiError> {
        let url = format!("{}{path}", self.base);
        let mut pairs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (*k, v.as_str())).collect();
        if let Some(team) = &self.team {
            pairs.push(("teamId", team));
        }
        let bearer = format!("Bearer {token}");
        let result = match method {
            "GET" => self
                .agent
                .get(&url)
                .header("Authorization", &bearer)
                .query_pairs(pairs)
                .config()
                .timeout_global(timeout)
                .build()
                .call(),
            other => {
                let req = match other {
                    "POST" => self.agent.post(&url),
                    "PATCH" => self.agent.patch(&url),
                    _ => self.agent.put(&url),
                };
                let req =
                    req.header("Authorization", &bearer).query_pairs(pairs).config().timeout_global(timeout).build();
                match body {
                    Some(b) => req.send_json(b),
                    None => req.send_empty(),
                }
            }
        };
        result.map_err(|e| ApiError::Network(network(&e)))
    }

    /// Notes the rate limit, and turns an unhappy answer into an error.
    fn check(
        &self,
        path: &str,
        mut resp: ureq::http::Response<ureq::Body>,
    ) -> Result<ureq::http::Response<ureq::Body>, ApiError> {
        let header = |name: &str| {
            resp.headers().get(name).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<i64>().ok())
        };
        let limit = match (header("x-ratelimit-limit"), header("x-ratelimit-remaining"), header("x-ratelimit-reset")) {
            (Some(limit), Some(remaining), Some(reset)) => {
                let l = Limit { limit: limit.max(0) as u64, remaining: remaining.max(0) as u64, reset: reset * 1000 };
                self.limits.lock().unwrap().insert(bucket(path), l);
                Some(l)
            }
            _ => None,
        };
        let status = resp.status().as_u16();
        if status < 400 {
            return Ok(resp);
        }
        let retry = header("retry-after");
        let text = resp.body_mut().read_to_string().unwrap_or_default();
        let why = message(&text, status);
        Err(match status {
            401 => ApiError::Unauthorized(why),
            403 => ApiError::Forbidden(why),
            404 => ApiError::NotFound(why),
            429 => {
                // Vercel says when to try again, or its per-minute count has run out
                // and fills again at its reset. A 429 with that count not run out
                // comes from some other limit, so it's tried again sooner.
                let now = chrono::Utc::now().timestamp_millis();
                let spent = limit.filter(|l| l.remaining == 0 && l.reset > now).map(|l| l.reset);
                let until = retry.map(|s| now + s * 1000).or(spent).unwrap_or(now + 20_000);
                ApiError::Limited(until, said(&text).unwrap_or_default())
            }
            _ => ApiError::Status(status, why),
        })
    }

    pub fn get<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)]) -> Result<T, ApiError> {
        let mut resp = self.call("GET", path, query, None, Some(TIMEOUT))?;
        read_json(&mut resp)
    }

    pub fn post<T: DeserializeOwned>(&self, path: &str, query: &[(&str, String)], body: &Value) -> Result<T, ApiError> {
        let mut resp = self.call("POST", path, query, Some(body), Some(TIMEOUT))?;
        read_json(&mut resp)
    }

    pub fn patch<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<T, ApiError> {
        let mut resp = self.call("PATCH", path, query, body, Some(TIMEOUT))?;
        read_json(&mut resp)
    }

    /// Reads an answer a line at a time, as JSON lines or a stream that stays
    /// open, until `each` says it has had enough.
    pub fn lines(
        &self,
        path: &str,
        query: &[(&str, String)],
        timeout: Option<Duration>,
        mut each: impl FnMut(&str) -> bool,
    ) -> Result<(), ApiError> {
        let resp = self.call("GET", path, query, None, timeout)?;
        let reader = std::io::BufReader::new(resp.into_body().into_reader());
        for line in reader.lines() {
            let line = line.map_err(|e| ApiError::Network(e.to_string()))?;
            if !line.trim().is_empty() && !each(&line) {
                break;
            }
        }
        Ok(())
    }
}

fn read_json<T: DeserializeOwned>(resp: &mut ureq::http::Response<ureq::Body>) -> Result<T, ApiError> {
    let text =
        resp.body_mut().with_config().limit(MOST).read_to_string().map_err(|e| ApiError::Network(network(&e)))?;
    // Some actions answer with nothing at all.
    let text = if text.trim().is_empty() { "null" } else { &text };
    serde_json::from_str(text).map_err(|e| ApiError::Decode(e.to_string()))
}

/// What an error said in words: Vercel's say `{"error": {"code": …, "message": …}}`.
fn said(text: &str) -> Option<String> {
    let v: Value = serde_json::from_str(text).ok()?;
    let m = v["error"]["message"].as_str().or(v["message"].as_str()).or(v["error"].as_str())?;
    Some(m.trim_end_matches('.').to_string())
}

fn message(text: &str, status: u16) -> String {
    match said(text) {
        Some(m) => m,
        None if !text.trim().is_empty() && text.len() < 200 => text.trim().to_string(),
        None => format!("HTTP {status}"),
    }
}

fn network(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(_) => "it timed out".into(),
        ureq::Error::HostNotFound => "the address doesn't resolve; is the network up?".into(),
        ureq::Error::Io(io) => io.to_string(),
        other => other.to_string(),
    }
}

/// A public page with no token, such as Vercel's status page.
pub fn public<T: DeserializeOwned>(url: &str) -> Result<T, ApiError> {
    let mut resp = agent().get(url).call().map_err(|e| ApiError::Network(network(&e)))?;
    if resp.status().as_u16() >= 400 {
        return Err(ApiError::Status(resp.status().as_u16(), url.to_string()));
    }
    read_json(&mut resp)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::config::Home;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    /// A stand-in for Vercel on a local port: each connection gets the next
    /// answer, and every request it saw is kept.
    pub struct Fake {
        pub base: String,
        pub seen: Arc<Mutex<Vec<String>>>,
    }

    /// A status, headers and a body.
    pub type Answer = (u16, Vec<(&'static str, String)>, String);

    pub fn fake(answers: Vec<Answer>) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        std::thread::spawn(move || {
            for (status, headers, body) in answers {
                let Ok((mut conn, _)) = listener.accept() else { return };
                let mut buf = [0u8; 16384];
                let mut got = Vec::new();
                loop {
                    let n = conn.read(&mut buf).unwrap_or(0);
                    got.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&got);
                    if n == 0 || text.contains("\r\n\r\n") && body_complete(&text) {
                        break;
                    }
                }
                kept.lock().unwrap().push(String::from_utf8_lossy(&got).into_owned());
                let mut out = format!("HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
                for (k, v) in headers {
                    out += &format!("{k}: {v}\r\n");
                }
                out += "\r\n";
                out += &body;
                let _ = conn.write_all(out.as_bytes());
            }
        });
        Fake { base, seen }
    }

    fn body_complete(text: &str) -> bool {
        let Some((head, body)) = text.split_once("\r\n\r\n") else { return false };
        let len = head
            .lines()
            .find_map(|l| {
                l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap_or(0))
            })
            .unwrap_or(0);
        body.len() >= len
    }

    pub fn api(base: &str) -> Api {
        let dir = tempfile::tempdir().unwrap();
        let auth = Auth::resolve_with(Some("t0ken".into()), &Home::under(dir.path()), "false").unwrap();
        Api::new(Arc::new(auth), base).for_team(Some("team_1".into()))
    }

    #[test]
    fn the_token_and_the_team_go_with_every_request() {
        let f = fake(vec![(200, vec![], r#"{"user":{"username":"pat"}}"#.into())]);
        let v: Value = api(&f.base).get("/v2/user", &[("limit", "3".into())]).unwrap();
        assert_eq!(v["user"]["username"], "pat");
        let seen = f.seen.lock().unwrap()[0].clone();
        assert!(seen.starts_with("GET /v2/user?limit=3&teamId=team_1 "), "{seen}");
        assert!(seen.to_lowercase().contains("authorization: bearer t0ken"), "{seen}");
    }

    #[test]
    fn errors_say_what_vercel_said_and_rate_limits_are_kept() {
        let reset = chrono::Utc::now().timestamp() + 30;
        let limits = vec![
            ("x-ratelimit-limit", "100".to_string()),
            ("x-ratelimit-remaining", "0".into()),
            ("x-ratelimit-reset", reset.to_string()),
        ];
        let f = fake(vec![
            (
                403,
                vec![],
                r#"{"error":{"code":"forbidden","message":"You don't have permission to view billing."}}"#.into(),
            ),
            (429, limits, r#"{"error":{"code":"rate_limited"}}"#.into()),
            (401, vec![], r#"{"error":{"code":"forbidden","message":"Not authorized"}}"#.into()),
        ]);
        let api = api(&f.base);
        let e = api.get::<Value>("/v1/billing/charges", &[]).unwrap_err();
        assert_eq!(e, ApiError::Forbidden("You don't have permission to view billing".into()));
        let e = api.post::<Value>("/v2/observability/query", &[], &serde_json::json!({})).unwrap_err();
        assert_eq!(e, ApiError::Limited(reset * 1000, String::new()), "with nothing left, until the count fills again");
        assert_eq!(api.limits()["v2/observability"].remaining, 0, "what the limit said is kept");
        let e = api.get::<Value>("/v2/user", &[]).unwrap_err();
        assert_eq!(e.to_string(), "Vercel refused the token (Not authorized)");
    }

    #[test]
    fn an_empty_answer_is_null_and_lines_come_one_at_a_time() {
        let f = fake(vec![(201, vec![], String::new()), (200, vec![], "{\"a\":1}\n\n{\"a\":2}\n{\"a\":3}\n".into())]);
        let api = api(&f.base);
        let v: Value = api.post("/v1/projects/p/rollback/d", &[], &Value::Null).unwrap();
        assert!(v.is_null());
        let mut got = vec![];
        api.lines("/v1/billing/charges", &[], None, |l| {
            got.push(serde_json::from_str::<Value>(l).unwrap()["a"].as_i64().unwrap());
            got.len() < 2
        })
        .unwrap();
        assert_eq!(got, [1, 2], "it stops when told");
    }

    #[test]
    fn requests_are_counted_by_kind() {
        assert_eq!(bucket("/v2/observability/query"), "v2/observability");
        assert_eq!(bucket("/v7/deployments"), "v7/deployments");
    }
}
