//! ooze: your Vercel projects, live, in a terminal. Who's on each site,
//! what's deploying, what's failing, and what it all costs.

mod actions;
mod agent;
mod api;
mod art;
mod auth;
mod billing;
mod budget;
mod config;
mod demo;
mod feed;
mod fmt;
mod model;
mod notify;
mod poll;
mod report;
mod theme;
mod tui;
mod vercel;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "ooze",
    version,
    about = "Your Vercel projects, live, in a terminal: who's on them, what's deploying, what's failing and what it all costs."
)]
struct Cli {
    /// One glance as text, then exit: what an agent can run to see what's happening.
    #[arg(long)]
    once: bool,
    /// The same as JSON.
    #[arg(long)]
    json: bool,
    /// The team to watch, by slug, name or id.
    #[arg(long, global = true)]
    team: Option<String>,
    /// Turns off cancel, redeploy, promote and roll back.
    #[arg(long)]
    read_only: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// The view on a made-up team, to see ooze before signing in.
    Demo,
    /// Saves a Vercel token for ooze to use.
    Login,
    /// Forgets the token `ooze login` saved.
    Logout,
    /// Who ooze is signed in as, how, and what it watches.
    Status,
    /// Sends notifications without the interface, until stopped.
    Watch,
    /// Starts `ooze watch` at login, or stops it.
    Agent {
        #[command(subcommand)]
        what: AgentCmd,
    },
    /// Prints the settings and where they are.
    Config,
    /// One frame as text, for checking the layout without a terminal.
    #[command(hide = true)]
    Snapshot {
        #[arg(long, default_value_t = 160)]
        width: u16,
        #[arg(long, default_value_t = 48)]
        height: u16,
        /// The made-up team rather than Vercel.
        #[arg(long)]
        demo: bool,
        /// Keys to press first: a tab is \t, Enter \n and Esc ^.
        #[arg(long, default_value = "")]
        keys: String,
        /// In 24-bit colour, as escape codes.
        #[arg(long)]
        ansi: bool,
    },
}

#[derive(Subcommand)]
enum AgentCmd {
    Install,
    Uninstall,
    Status,
}

fn main() -> ExitCode {
    let matches = Cli::command().styles(theme::help_styles()).get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(c) => c,
        Err(e) => e.exit(),
    };
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ooze: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn signed_in(home: &config::Home) -> Result<api::Api> {
    let auth = auth::Auth::resolve(home)?;
    Ok(api::Api::new(Arc::new(auth), api::BASE))
}

fn run(cli: Cli) -> Result<()> {
    let home = config::Home::from_env()?;
    let mut cfg = config::load_or_create(&home)?;
    cfg.read_only |= cli.read_only;
    let team = cli.team.clone().or_else(|| Some(cfg.team.clone()).filter(|t| !t.is_empty()));
    match cli.cmd {
        None if cli.once || cli.json => {
            let w = poll::once(signed_in(&home)?, cfg, home, team)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report::json(&w, now()))?);
            } else {
                print!("{}", report::text(&w, now(), theme::Paint::stdout()));
            }
        }
        None => {
            let api = signed_in(&home)?;
            let (hub, rx) = poll::Hub::start(api, cfg.clone(), home.clone(), team, poll::Pace::View);
            tui::run(hub, rx, &cfg, notify::Notifier::new(&cfg, &home))?;
        }
        Some(Cmd::Demo) => tui::run_demo(&cfg)?,
        Some(Cmd::Login) => login(&home)?,
        Some(Cmd::Logout) => {
            let path = home.token();
            if path.exists() {
                std::fs::remove_file(&path)?;
                println!("Forgot the token in {}. The Vercel CLI's own login is untouched.", path.display());
            } else {
                println!("No token saved by `ooze login`.");
            }
        }
        Some(Cmd::Status) => status(&home, &cfg, team)?,
        Some(Cmd::Watch) => watch(&home, cfg, team)?,
        Some(Cmd::Agent { what }) => match what {
            AgentCmd::Install => agent::install(&home)?,
            AgentCmd::Uninstall => agent::uninstall(&home)?,
            AgentCmd::Status => {
                let (installed, running) = agent::status(&home);
                println!(
                    "agent: {} · {}",
                    if installed { "installed" } else { "not installed" },
                    if running { "running" } else { "not running" }
                );
            }
        },
        Some(Cmd::Config) => {
            println!("# {}", home.settings().display());
            print!("{}", std::fs::read_to_string(home.settings()).unwrap_or_else(|_| config::TEMPLATE.into()));
        }
        Some(Cmd::Snapshot { width, height, demo, keys, ansi }) => {
            let mut view = tui::View::new(None, &cfg, None);
            view.world = if demo { demo::world(now()) } else { poll::once(signed_in(&home)?, cfg, home, team)? };
            // A first frame picks the rows the keys then move from.
            tui::snapshot(&mut view, width, height);
            view.press(&keys.replace("\\t", "\t").replace("\\n", "\n"));
            let frame = if ansi {
                tui::snapshot_ansi(&mut view, width, height)
            } else {
                tui::snapshot(&mut view, width, height)
            };
            print!("{frame}");
        }
    }
    Ok(())
}

fn login(home: &config::Home) -> Result<()> {
    eprintln!(
        "Create a token at https://vercel.com/account/settings/tokens. It can be limited to one team and given an expiry."
    );
    let token = auth::read_secret("Paste it here: ")?;
    if token.is_empty() {
        bail!("no token given");
    }
    let auth = auth::Auth::resolve_with(Some(token.clone()), home, "vercel")?;
    let api = api::Api::new(Arc::new(auth), api::BASE);
    let user = vercel::user(&api).context("checking the token with Vercel")?;
    config::write_private(&home.token(), &format!("{token}\n"))?;
    println!("Signed in as {}. The token is in {}, readable only by you.", user.username, home.token().display());
    if std::env::var_os("VERCEL_TOKEN").is_some() {
        println!("VERCEL_TOKEN is set too, and comes first while it is.");
    }
    Ok(())
}

fn status(home: &config::Home, cfg: &config::Config, team: Option<String>) -> Result<()> {
    let api = signed_in(home)?;
    println!("signed in with {}", api.auth().source.describe());
    let user = vercel::user(&api)?;
    let teams = vercel::teams(&api)?;
    let chosen =
        poll::choose(&teams, team.as_deref(), auth::cli_team(home).as_deref(), user.default_team_id.as_deref());
    println!("as         {}", user.username);
    for t in &teams {
        let mark = if Some(&t.id) == chosen.as_ref() { "→" } else { " " };
        println!(
            "{mark} team     {} ({}){}",
            t.title(),
            t.slug,
            t.plan().map(|p| format!(", {p}")).unwrap_or_default()
        );
    }
    println!("actions    {}", if cfg.read_only { "off (read_only)" } else { "on, each confirmed" });
    let to = if cfg.notify.to.is_empty() { "off".to_string() } else { cfg.notify.to.join(", ") };
    let (installed, running) = agent::status(home);
    let sender = if notify::Notifier::new(cfg, home).elsewhere() { ", which sends them now" } else { "" };
    println!(
        "notify     {to} · background agent {}{sender}",
        match (installed, running) {
            (true, true) => "running",
            (true, false) => "installed, not running",
            _ => "not installed (`ooze agent install`)",
        }
    );
    if let Some(team) = &chosen {
        let b = budget::Budget::new(&home.cache, team, cfg.daily_queries);
        let s = b.state();
        let at = now();
        let paused = match s.paused_until.filter(|u| *u > at) {
            Some(until) => {
                format!(" · paused: Vercel's daily limit was reached; ooze tries again at {}", fmt::time(until))
            }
            None => String::new(),
        };
        println!("queries    {} of ooze's {} observability queries in the last 24 hours{paused}", s.used(at), b.daily);
    }
    for (kind, l) in api.limits() {
        println!("limit      {kind}: {} of {} left this minute", l.remaining, l.limit);
    }
    println!("settings   {}", home.settings().display());
    Ok(())
}

/// Notifications without the interface, for as long as it runs.
fn watch(home: &config::Home, cfg: config::Config, team: Option<String>) -> Result<()> {
    let mut notifier = notify::Notifier::new(&cfg, home);
    if !notifier.hold() {
        bail!("another `ooze watch` is sending the notifications already");
    }
    let api = signed_in(home)?;
    let (hub, rx) = poll::Hub::start(api, cfg.clone(), home.clone(), team, poll::Pace::Watch);
    let mut world = model::World::new(now(), cfg);
    let mut generation = 0;
    println!("{} ooze watch: started", fmt::when(now(), now()));
    loop {
        let Ok(env) = rx.recv_timeout(Duration::from_secs(60)) else { continue };
        if env.generation < generation {
            continue;
        }
        generation = env.generation;
        if let model::Msg::Failed(source, e) = &env.msg {
            println!("{} couldn't read {}: {e}", fmt::when(now(), now()), source.name());
        }
        world.apply(env.msg, now());
        for n in std::mem::take(&mut world.notices) {
            let sent = notifier.send(&n);
            println!(
                "{} {}: {} — {}",
                fmt::when(now(), now()),
                if sent { "notified" } else { "not sent" },
                n.title,
                n.body
            );
        }
        if matches!(world.link, model::Link::Refused(_)) {
            hub.stop();
            bail!("Vercel refused the token; `ooze login` or `vercel login` signs in again");
        }
    }
}
