//! `ooze watch` at login: a LaunchAgent on macOS, a systemd user unit on
//! Linux. It only notifies; it never acts on anything.

use crate::config::Home;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const LABEL: &str = "dev.badcoffee.ooze.watch";
const UNIT: &str = "ooze-watch.service";

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The LaunchAgent: `ooze watch`, started at login and again if it stops.
pub fn plist(exe: &Path, logs: &Path) -> String {
    let log = xml(&logs.join("watch.log").display().to_string());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LABEL}</string>
	<key>ProgramArguments</key>
	<array>
		<string>{}</string>
		<string>watch</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<true/>
	<key>ThrottleInterval</key>
	<integer>60</integer>
	<key>EnvironmentVariables</key>
	<dict>
		<key>PATH</key>
		<string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
	</dict>
	<key>StandardOutPath</key>
	<string>{log}</string>
	<key>StandardErrorPath</key>
	<string>{log}</string>
	<key>ProcessType</key>
	<string>Background</string>
	<key>LowPriorityIO</key>
	<true/>
	<key>Nice</key>
	<integer>10</integer>
</dict>
</plist>
"#,
        xml(&exe.display().to_string())
    )
}

/// The systemd user unit, for Linux.
pub fn unit(exe: &Path) -> String {
    format!(
        "[Unit]\nDescription=ooze: notifications from Vercel\nAfter=network-online.target\n\n[Service]\nExecStart={} watch\nRestart=always\nRestartSec=60\nNice=10\n\n[Install]\nWantedBy=default.target\n",
        exe.display()
    )
}

fn plist_path(home: &Home) -> PathBuf {
    home.home.join("Library/LaunchAgents").join(format!("{LABEL}.plist"))
}

fn unit_path(home: &Home) -> PathBuf {
    home.home.join(".config/systemd/user").join(UNIT)
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

pub fn install(home: &Home) -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    if cfg!(target_os = "macos") {
        std::fs::create_dir_all(&home.logs)?;
        let path = plist_path(home);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, plist(&exe, &home.logs)).with_context(|| format!("writing {}", path.display()))?;
        let _ = Command::new("/bin/launchctl").args(["bootout", &format!("{}/{LABEL}", domain())]).output();
        let out = Command::new("/bin/launchctl").args(["bootstrap", &domain()]).arg(&path).output()?;
        if !out.status.success() {
            bail!(
                "wrote {} but launchctl couldn't load it: {}\nRun this in Terminal: launchctl bootstrap {} {}",
                path.display(),
                String::from_utf8_lossy(&out.stderr).trim(),
                domain(),
                path.display()
            );
        }
        println!("Installed {LABEL}: ooze watch starts at login and sends notifications. It never acts on anything.");
        println!("Agent: {}\nLog:   {}", path.display(), home.logs.join("watch.log").display());
    } else {
        let path = unit_path(home);
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(&path, unit(&exe))?;
        let out = Command::new("systemctl").args(["--user", "enable", "--now", UNIT]).output();
        match out {
            Ok(o) if o.status.success() => {
                println!("Installed {UNIT}: ooze watch starts at login and sends notifications.")
            }
            _ => println!("Wrote {}. Start it with: systemctl --user enable --now {UNIT}", path.display()),
        }
    }
    Ok(())
}

pub fn uninstall(home: &Home) -> Result<()> {
    let path = if cfg!(target_os = "macos") {
        let _ = Command::new("/bin/launchctl").args(["bootout", &format!("{}/{LABEL}", domain())]).output();
        plist_path(home)
    } else {
        let _ = Command::new("systemctl").args(["--user", "disable", "--now", UNIT]).output();
        unit_path(home)
    };
    if path.exists() {
        std::fs::remove_file(&path)?;
        println!("Removed {}", path.display());
    } else {
        println!("No agent installed.");
    }
    Ok(())
}

/// Whether the agent is installed, and whether it runs.
pub fn status(home: &Home) -> (bool, bool) {
    if cfg!(target_os = "macos") {
        let running = Command::new("/bin/launchctl")
            .args(["print", &format!("{}/{LABEL}", domain())])
            .output()
            .is_ok_and(|o| o.status.success());
        (plist_path(home).exists(), running)
    } else {
        let running = Command::new("systemctl")
            .args(["--user", "is-active", "--quiet", UNIT])
            .status()
            .is_ok_and(|s| s.success());
        (unit_path(home).exists(), running)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_agent_runs_ooze_watch_and_nothing_else() {
        let p = plist(Path::new("/Users/u/.cargo/bin/ooze"), Path::new("/Users/u/Library/Logs/ooze"));
        assert!(p.contains("<string>/Users/u/.cargo/bin/ooze</string>\n\t\t<string>watch</string>"), "{p}");
        assert!(p.contains(&format!("<string>{LABEL}</string>")));
        assert!(p.contains("/Users/u/Library/Logs/ooze/watch.log"));
        if Path::new("/usr/bin/plutil").exists() {
            let dir = tempfile::tempdir().unwrap();
            let file = dir.path().join("agent.plist");
            std::fs::write(&file, &p).unwrap();
            let lint = Command::new("/usr/bin/plutil").arg("-lint").arg(&file).output().unwrap();
            assert!(lint.status.success(), "{}", String::from_utf8_lossy(&lint.stdout));
        }
        let u = unit(Path::new("/home/u/.cargo/bin/ooze"));
        assert!(u.contains("ExecStart=/home/u/.cargo/bin/ooze watch") && u.contains("Restart=always"));
    }
}
