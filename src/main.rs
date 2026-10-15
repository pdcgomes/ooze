//! ooze: your Vercel projects, live, in a terminal.

#![allow(dead_code)]

mod api;
mod art;
mod auth;
mod config;
mod fmt;
mod theme;
mod vercel;

fn main() {
    println!("ooze {}", env!("CARGO_PKG_VERSION"));
}
