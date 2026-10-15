//! Notifications: Notification Center on macOS, notify-send on Linux, and the
//! terminal's own (OSC 9) when asked for. Only the process holding the lock
//! in ooze's cache folder sends them: `ooze watch` holds it while it runs,
//! and the open view takes it for each one only when nobody else has it.

use crate::config::{Config, Home};
use crate::model::Notice;
use anyhow::{Result, bail};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::process::Command;

pub struct Notifier {
    to: Vec<String>,
    lock: PathBuf,
    held: Option<File>,
    /// Stands in for delivery in tests.
    pub sent: Option<Vec<Notice>>,
}

fn try_lock(path: &PathBuf) -> Option<File> {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let f = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(path).ok()?;
    (unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0).then_some(f)
}

impl Notifier {
    pub fn new(cfg: &Config, home: &Home) -> Notifier {
        Notifier { to: cfg.notify.to.clone(), lock: home.cache.join("notify.lock"), held: None, sent: None }
    }

    /// Holds the lock for as long as this process runs, as `ooze watch` does.
    pub fn hold(&mut self) -> bool {
        if self.held.is_none() {
            self.held = try_lock(&self.lock);
        }
        self.held.is_some()
    }

    /// Whether another process sends the notifications now.
    pub fn elsewhere(&self) -> bool {
        self.held.is_none() && try_lock(&self.lock).is_none()
    }

    /// Sends it, unless another process holds the lock. Says whether it went.
    pub fn send(&mut self, n: &Notice) -> bool {
        if self.to.is_empty() {
            return false;
        }
        let _taken = match &self.held {
            Some(_) => None,
            None => match try_lock(&self.lock) {
                Some(f) => Some(f),
                None => return false,
            },
        };
        if let Some(sent) = &mut self.sent {
            sent.push(n.clone());
            return true;
        }
        let mut went = false;
        for to in &self.to {
            went |= match to.as_str() {
                "desktop" => desktop(&n.title, &n.body).is_ok(),
                "terminal" => terminal(&n.title, &n.body),
                _ => false,
            };
        }
        went
    }
}

fn desktop(title: &str, body: &str) -> Result<()> {
    let out = if cfg!(target_os = "macos") {
        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("display notification \"{}\" with title \"ooze\" subtitle \"{}\"", esc(body), esc(title));
        Command::new("/usr/bin/osascript").args(["-e", &script]).output()?
    } else {
        Command::new("notify-send").args(["--app-name=ooze", title, body]).output()?
    };
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// OSC 9, which iTerm2, Ghostty and WezTerm show.
fn terminal(title: &str, body: &str) -> bool {
    use std::io::Write;
    let clean = |s: &str| s.chars().filter(|c| !c.is_control()).collect::<String>();
    let mut out = std::io::stdout();
    write!(out, "\x1b]9;{}: {}\x07", clean(title), clean(body)).is_ok() && out.flush().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice() -> Notice {
        Notice { key: "k".into(), title: "moth-radio: production deploy failed".into(), body: "exit 1".into() }
    }

    #[test]
    fn only_one_process_notifies() {
        let dir = tempfile::tempdir().unwrap();
        let home = Home::under(dir.path());
        let cfg = Config::default();
        let mut watch = Notifier::new(&cfg, &home);
        let mut view = Notifier::new(&cfg, &home);
        (watch.sent, view.sent) = (Some(vec![]), Some(vec![]));
        assert!(view.send(&notice()), "nobody holds the lock, so the view sends");
        assert!(watch.hold());
        assert!(view.elsewhere());
        assert!(!view.send(&notice()), "while ooze watch runs, it sends instead");
        assert!(watch.send(&notice()));
        assert_eq!((view.sent.unwrap().len(), watch.sent.unwrap().len()), (1, 1));
    }

    #[test]
    fn nowhere_to_send_sends_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.notify.to.clear();
        let mut n = Notifier::new(&cfg, &Home::under(dir.path()));
        n.sent = Some(vec![]);
        assert!(!n.send(&notice()));
    }
}
