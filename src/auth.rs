//! Signing in: `VERCEL_TOKEN`, then the token `ooze login` saved, then the
//! Vercel CLI's login.
//!
//! ooze never renews the CLI's token itself. The CLI renews it with a refresh
//! token that changes each time it's used, so renewing it here would sign the
//! CLI out. When it's about to expire, ooze runs `vercel whoami`, which renews
//! it, and reads it again.

use crate::config::Home;
use anyhow::{Result, bail};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long before it expires the CLI's token is renewed.
const EARLY: i64 = 5 * 60 * 1000;
const RENEW_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Env,
    Saved(PathBuf),
    Cli(PathBuf),
}

impl Source {
    pub fn describe(&self) -> String {
        match self {
            Source::Env => "VERCEL_TOKEN".into(),
            Source::Saved(p) => format!("the token `ooze login` saved in {}", p.display()),
            Source::Cli(_) => "the Vercel CLI's login".into(),
        }
    }
}

#[derive(Debug)]
pub struct Auth {
    pub source: Source,
    held: Mutex<Held>,
    vercel: String,
}

#[derive(Debug, Clone)]
struct Held {
    token: String,
    /// When the CLI's token expires, in milliseconds.
    expires: Option<i64>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CliAuth {
    token: Option<String>,
    expires_at: Option<f64>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

impl Auth {
    pub fn resolve(home: &Home) -> Result<Auth> {
        Auth::resolve_with(std::env::var("VERCEL_TOKEN").ok(), home, "vercel")
    }

    pub fn resolve_with(env: Option<String>, home: &Home, vercel: &str) -> Result<Auth> {
        let held = |token: String, expires| Mutex::new(Held { token, expires });
        if let Some(token) = env.map(|t| t.trim().to_string()).filter(|t| !t.is_empty()) {
            return Ok(Auth { source: Source::Env, held: held(token, None), vercel: vercel.into() });
        }
        let saved = home.token();
        if let Ok(text) = std::fs::read_to_string(&saved)
            && !text.trim().is_empty()
        {
            return Ok(Auth { source: Source::Saved(saved), held: held(text.trim().to_string(), None), vercel: vercel.into() });
        }
        let cli = home.vercel_cli.join("auth.json");
        if let Some((token, expires)) = read_cli(&cli) {
            let auth = Auth { source: Source::Cli(cli), held: held(token, expires), vercel: vercel.into() };
            auth.token()?;
            return Ok(auth);
        }
        bail!(
            "ooze isn't signed in to Vercel. Any of these does it:\n  \
             • `ooze login`, with a token from https://vercel.com/account/settings/tokens\n  \
             • VERCEL_TOKEN set to such a token\n  \
             • `vercel login`, and ooze uses the Vercel CLI's login"
        )
    }

    /// A token good for a while yet.
    pub fn token(&self) -> Result<String> {
        let held = self.held.lock().unwrap().clone();
        let Source::Cli(path) = &self.source else { return Ok(held.token) };
        if held.expires.is_none_or(|e| e - now() > EARLY) {
            return Ok(held.token);
        }
        // Another reader may have renewed it already.
        if let Some((token, expires)) = read_cli(path).filter(|(_, e)| e.is_none_or(|e| e - now() > EARLY)) {
            *self.held.lock().unwrap() = Held { token: token.clone(), expires };
            return Ok(token);
        }
        self.renew()
    }

    /// Has the CLI renew its login, after Vercel refused the token ooze had.
    /// Only the CLI's login can be renewed.
    pub fn renew_after_refusal(&self) -> Option<String> {
        matches!(self.source, Source::Cli(_)).then(|| self.renew().ok()).flatten()
    }

    fn renew(&self) -> Result<String> {
        let Source::Cli(path) = &self.source else { bail!("only the Vercel CLI's login can be renewed") };
        let before = self.held.lock().unwrap().token.clone();
        if let Err(e) = run_with_timeout(Command::new(&self.vercel).args(["whoami", "--non-interactive"]), RENEW_TIMEOUT) {
            bail!(
                "the Vercel CLI's login has expired and `vercel whoami` couldn't renew it ({e:#}): `vercel login` signs in \
                 again, or `ooze login` uses a token instead"
            );
        }
        match read_cli(path) {
            Some((token, expires)) if token != before || expires.is_none_or(|e| e - now() > EARLY) => {
                *self.held.lock().unwrap() = Held { token: token.clone(), expires };
                Ok(token)
            }
            _ => bail!("the Vercel CLI's login has expired: `vercel login` signs in again"),
        }
    }
}

fn read_cli(path: &std::path::Path) -> Option<(String, Option<i64>)> {
    let auth: CliAuth = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let token = auth.token.filter(|t| !t.is_empty())?;
    // The CLI has written it in seconds and in milliseconds.
    let expires = auth.expires_at.map(|e| if e < 1e11 { (e * 1000.0) as i64 } else { e as i64 });
    Some((token, expires))
}

fn run_with_timeout(cmd: &mut Command, timeout: Duration) -> Result<()> {
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if status.success() {
                return Ok(());
            }
            let mut err = String::new();
            if let Some(mut e) = child.stderr.take() {
                use std::io::Read;
                let _ = e.read_to_string(&mut err);
            }
            bail!("{}", err.lines().find(|l| !l.trim().is_empty()).unwrap_or("it failed").trim());
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            bail!("it took longer than {} s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The team the Vercel CLI has switched to, if any.
pub fn cli_team(home: &Home) -> Option<String> {
    let text = std::fs::read_to_string(home.vercel_cli.join("config.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v["currentTeam"].as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// Reads a line without showing it, when there's a terminal to read from.
pub fn read_secret(prompt: &str) -> Result<String> {
    use std::io::{BufRead, IsTerminal, Write};
    let stdin = std::io::stdin();
    if !stdin.is_terminal() {
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        return Ok(line.trim().to_string());
    }
    eprint!("{prompt}");
    std::io::stderr().flush()?;
    let fd = libc::STDIN_FILENO;
    let mut term: libc::termios = unsafe { std::mem::zeroed() };
    let hidden = unsafe { libc::tcgetattr(fd, &mut term) } == 0;
    let original = term;
    if hidden {
        term.c_lflag &= !libc::ECHO;
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &term) };
    }
    let mut line = String::new();
    let read = stdin.lock().read_line(&mut line);
    if hidden {
        unsafe { libc::tcsetattr(fd, libc::TCSANOW, &original) };
    }
    eprintln!();
    read?;
    Ok(line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn cli_auth(home: &Home, token: &str, expires: i64) {
        std::fs::create_dir_all(&home.vercel_cli).unwrap();
        let json = serde_json::json!({ "token": token, "userId": "u", "expiresAt": expires, "refreshToken": "r" });
        std::fs::write(home.vercel_cli.join("auth.json"), json.to_string()).unwrap();
    }

    /// A stand-in for the Vercel CLI whose `whoami` renews its login.
    fn fake_vercel(home: &Home, renews: bool) -> String {
        let path = home.home.join("bin/vercel");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let auth = home.vercel_cli.join("auth.json");
        let later = now() + 3_600_000;
        let body = if renews {
            format!("#!/bin/sh\nprintf '{{\"token\":\"renewed\",\"expiresAt\":{later}}}' > '{}'\necho someone\n", auth.display())
        } else {
            "#!/bin/sh\necho 'Error: The specified token is not valid' >&2\nexit 1\n".to_string()
        };
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.display().to_string()
    }

    #[test]
    fn the_environment_comes_first_then_the_saved_token_then_the_cli() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        cli_auth(&home, "from-cli", now() + 3_600_000);
        assert_eq!(Auth::resolve_with(None, &home, "false").unwrap().source, Source::Cli(home.vercel_cli.join("auth.json")));
        crate::config::write_private(&home.token(), "saved\n").unwrap();
        let saved = Auth::resolve_with(None, &home, "false").unwrap();
        assert_eq!((saved.source.clone(), saved.token().unwrap()), (Source::Saved(home.token()), "saved".into()));
        let env = Auth::resolve_with(Some("from-env".into()), &home, "false").unwrap();
        assert_eq!((env.source.clone(), env.token().unwrap()), (Source::Env, "from-env".into()));
    }

    #[test]
    fn a_fresh_cli_token_is_used_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        cli_auth(&home, "fresh", now() + 3_600_000);
        let vercel = fake_vercel(&home, false);
        assert_eq!(Auth::resolve_with(None, &home, &vercel).unwrap().token().unwrap(), "fresh", "no need to run the CLI");
    }

    #[test]
    fn an_expiring_cli_token_is_renewed_by_the_cli_and_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        cli_auth(&home, "stale", (now() + 60_000) / 1000);
        let vercel = fake_vercel(&home, true);
        let auth = Auth::resolve_with(None, &home, &vercel).unwrap();
        assert_eq!(auth.token().unwrap(), "renewed");
    }

    #[test]
    fn when_the_cli_cant_renew_it_says_to_log_in_again() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        cli_auth(&home, "stale", now() - 60_000);
        let vercel = fake_vercel(&home, false);
        let err = format!("{:#}", Auth::resolve_with(None, &home, &vercel).unwrap_err());
        assert!(err.contains("token is not valid") && err.contains("`vercel login` signs in again"), "{err}");
        assert!(Auth::resolve_with(None, &Home::under(&dir.path().join("nobody")), &vercel).unwrap_err().to_string().contains("ooze login"));
    }

    #[test]
    fn the_cli_team_is_read_from_its_settings() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        assert_eq!(cli_team(&home), None);
        std::fs::create_dir_all(&home.vercel_cli).unwrap();
        std::fs::write(home.vercel_cli.join("config.json"), r#"{"// Note":"x","currentTeam":"team_abc"}"#).unwrap();
        assert_eq!(cli_team(&home).as_deref(), Some("team_abc"));
    }
}
