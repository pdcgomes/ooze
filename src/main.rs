//! ooze: your Vercel projects, live, in a terminal.

#![allow(dead_code)]

mod actions;
mod api;
mod art;
mod auth;
mod billing;
mod config;
mod feed;
mod fmt;
mod model;
mod notify;
mod poll;
mod report;
mod theme;
mod vercel;

use anyhow::Result;
use std::sync::Arc;

fn main() -> Result<()> {
    let home = config::Home::from_env()?;
    let cfg = config::load_or_create(&home)?;
    let auth = auth::Auth::resolve(&home)?;
    let api = api::Api::new(Arc::new(auth), api::BASE);
    let now = chrono::Utc::now().timestamp_millis();
    let w = poll::once(api, cfg, home, None)?;
    if std::env::args().any(|a| a == "--json") {
        println!("{}", serde_json::to_string_pretty(&report::json(&w, now))?);
    } else {
        print!("{}", report::text(&w, now, theme::Paint::stdout()));
    }
    Ok(())
}
