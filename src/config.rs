//! Settings in ~/.config/ooze/config.toml, and where ooze keeps its other
//! files on macOS and Linux.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Config {
    /// The team to show, by slug; empty for the Vercel CLI's current team or the only one.
    pub team: String,
    /// Turns the actions off.
    pub read_only: bool,
    /// Seconds between refreshes of the live numbers, 60 at the least.
    pub refresh: u64,
    /// The most observability queries ooze makes in any 24 hours, across every
    /// ooze on this machine.
    pub daily_queries: u64,
    /// Projects with fewer page views (or requests) in the last hour fold into one line.
    pub quiet_below: u64,
    pub notify: Notify,
    pub thresholds: Thresholds,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Notify {
    pub to: Vec<String>,
    pub deploy_failed: bool,
    pub deploy_ready: bool,
    pub traffic_spike: bool,
    pub error_spike: bool,
    pub attack: bool,
    pub incident: bool,
    pub domain_expiring: bool,
    pub credit_used: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default)]
pub struct Thresholds {
    pub spike_factor: f64,
    pub spike_min_views: u64,
    pub error_rate: f64,
    pub error_min_requests: u64,
    pub domain_days: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            team: String::new(),
            read_only: false,
            refresh: 120,
            daily_queries: 600,
            quiet_below: 1,
            notify: Notify::default(),
            thresholds: Thresholds::default(),
        }
    }
}

impl Default for Notify {
    fn default() -> Self {
        Notify {
            to: vec!["desktop".into()],
            deploy_failed: true,
            deploy_ready: false,
            traffic_spike: true,
            error_spike: true,
            attack: true,
            incident: true,
            domain_expiring: true,
            credit_used: true,
        }
    }
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds { spike_factor: 3.0, spike_min_views: 30, error_rate: 0.05, error_min_requests: 50, domain_days: 14 }
    }
}

pub const TEMPLATE: &str = r#"# ooze settings. Every key is optional; a missing key takes the value shown here.

# The team to show, by its slug. Empty: the Vercel CLI's current team, or your
# only one. `t` in the view switches between your teams.
team = ""

# Turns off cancel, redeploy, promote and roll back, so ooze only watches.
read_only = false

# Seconds between refreshes of the live numbers: who's online and page views a
# minute. Each refresh is two observability queries, which Vercel limits a day
# for each team, so it's 60 at the least, and ooze refreshes less often as the
# day's budget below runs down.
refresh = 120

# The most observability queries ooze makes in any 24 hours, counting every
# ooze on this machine. When a team reaches Vercel's daily limit, which Vercel
# doesn't publish, the live numbers stop until the next day. Web Analytics,
# deployments and billing don't count.
daily_queries = 600

# Projects with fewer page views (or requests) than this in the last hour fold
# into one line, which `a` opens.
quiet_below = 1

[notify]
# Where notifications go: "desktop" (Notification Center on macOS, notify-send
# on Linux) and "terminal" (the terminal's own notification). [] turns them off.
to = ["desktop"]
deploy_failed = true     # a production deployment failed
deploy_ready = false     # a production deployment is live
traffic_spike = true     # page views jumped, by the thresholds below
error_spike = true       # 5xx responses jumped, by the thresholds below
attack = true            # Vercel's firewall is mitigating an attack
incident = true          # Vercel's status page reports an incident
domain_expiring = true   # a domain expires soon and won't renew by itself
credit_used = true       # usage passed the plan's included credit

[thresholds]
spike_factor = 3.0       # 5 minutes of page views at least this many times the last hour's 5-minute average...
spike_min_views = 30     # ...and at least this many
error_rate = 0.05        # 5xx responses at least this share of the last 5 minutes' requests...
error_min_requests = 50  # ...out of at least this many
domain_days = 14         # how soon a domain's expiry is worth saying
"#;

/// Where ooze's files are, and the Vercel CLI's, from a home folder.
#[derive(Clone, Debug)]
pub struct Home {
    pub home: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
    pub logs: PathBuf,
    pub vercel_cli: PathBuf,
}

impl Home {
    pub fn from_env() -> Result<Home> {
        let home = std::env::var_os("HOME").map(PathBuf::from).context("HOME isn't set")?;
        let xdg = |var: &str, fallback: &str| match std::env::var_os(var) {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => home.join(fallback),
        };
        Ok(if cfg!(target_os = "macos") {
            Home {
                config: xdg("XDG_CONFIG_HOME", ".config").join("ooze"),
                cache: home.join("Library/Caches/ooze"),
                logs: home.join("Library/Logs/ooze"),
                vercel_cli: home.join("Library/Application Support/com.vercel.cli"),
                home,
            }
        } else {
            let cache = xdg("XDG_CACHE_HOME", ".cache").join("ooze");
            Home {
                config: xdg("XDG_CONFIG_HOME", ".config").join("ooze"),
                logs: cache.join("logs"),
                cache,
                vercel_cli: xdg("XDG_DATA_HOME", ".local/share").join("com.vercel.cli"),
                home,
            }
        })
    }

    /// Everything under one folder, for tests.
    #[cfg(test)]
    pub fn under(dir: &Path) -> Home {
        Home {
            home: dir.to_path_buf(),
            config: dir.join("config"),
            cache: dir.join("cache"),
            logs: dir.join("logs"),
            vercel_cli: dir.join("vercel"),
        }
    }

    pub fn settings(&self) -> PathBuf {
        self.config.join("config.toml")
    }

    pub fn token(&self) -> PathBuf {
        self.config.join("token")
    }
}

/// Templates earlier versions wrote, by their FNV-1a hash: a settings file
/// still exactly one of them is replaced by the current template.
const PAST_TEMPLATES: [u64; 1] = [0x8e41_b9f4_f8b6_ddbe];

fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| (h ^ *b as u64).wrapping_mul(0x0100_0000_01b3))
}

/// Loads the settings, writing the commented default first if there are none
/// or they're an earlier version's, untouched.
pub fn load_or_create(home: &Home) -> Result<Config> {
    let p = home.settings();
    let untouched = std::fs::read(&p).is_ok_and(|b| PAST_TEMPLATES.contains(&fnv(&b)));
    if !p.exists() || untouched {
        let _ = std::fs::create_dir_all(&home.config);
        let _ = std::fs::write(&p, TEMPLATE);
    }
    match std::fs::read_to_string(&p) {
        Ok(text) => parse(&text).with_context(|| format!("reading {}", p.display())),
        Err(_) => Ok(Config::default()),
    }
}

pub fn parse(text: &str) -> Result<Config> {
    Ok(toml::from_str(text)?)
}

/// Writes a file only its owner can read.
pub fn write_private(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(contents.as_bytes())?;
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_template_is_the_default() {
        assert_eq!(parse(TEMPLATE).unwrap(), Config::default());
    }

    #[test]
    fn missing_keys_take_defaults() {
        let c = parse("read_only = true\n[notify]\nto = []\n").unwrap();
        assert!(c.read_only);
        assert!(c.notify.to.is_empty());
        assert!(c.notify.deploy_failed, "a key not given keeps its default");
        assert_eq!((c.refresh, c.daily_queries), (120, 600));
        assert_eq!(c.thresholds.spike_factor, 3.0);
    }

    #[test]
    fn an_earlier_template_left_as_it_was_is_replaced_and_an_edited_one_kept() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        std::fs::create_dir_all(&home.config).unwrap();
        let earlier = include_str!("../tests/earlier-template.toml");
        assert!(PAST_TEMPLATES.contains(&fnv(earlier.as_bytes())), "the hash is the earlier template's");
        std::fs::write(home.settings(), earlier).unwrap();
        assert_eq!(load_or_create(&home).unwrap().refresh, 120);
        assert_eq!(std::fs::read_to_string(home.settings()).unwrap(), TEMPLATE);
        let edited = earlier.replace("read_only = false", "read_only = true");
        std::fs::write(home.settings(), &edited).unwrap();
        let c = load_or_create(&home).unwrap();
        assert_eq!((c.read_only, c.refresh), (true, 10), "what someone wrote stays");
        assert_eq!(std::fs::read_to_string(home.settings()).unwrap(), edited);
    }

    #[test]
    fn the_first_run_writes_the_template_and_the_token_is_private() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        assert_eq!(load_or_create(&home).unwrap(), Config::default());
        assert_eq!(std::fs::read_to_string(home.settings()).unwrap(), TEMPLATE);
        write_private(&home.token(), "secret").unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(home.token()).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
