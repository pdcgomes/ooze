//! Cancel, redeploy, promote and roll back: when each applies, what its
//! dialog says will happen, and doing it.

use crate::api::Api;
use crate::fmt;
use crate::model::World;
use crate::vercel::{self, Deployment};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Cancel,
    Redeploy,
    Promote,
    Rollback,
}

impl Action {
    pub const ALL: [Action; 4] = [Action::Cancel, Action::Redeploy, Action::Promote, Action::Rollback];

    pub fn key(self) -> char {
        match self {
            Action::Cancel => 'x',
            Action::Redeploy => 'R',
            Action::Promote => 'P',
            Action::Rollback => 'B',
        }
    }

    pub fn verb(self) -> &'static str {
        match self {
            Action::Cancel => "cancel",
            Action::Redeploy => "redeploy",
            Action::Promote => "promote",
            Action::Rollback => "roll back",
        }
    }

    pub fn of_key(c: char) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.key() == c)
    }
}

pub fn short(uid: &str) -> String {
    uid.chars().take(11).collect()
}

/// The deployment serving production for its project, if ooze has it.
fn current<'a>(w: &'a World, d: &Deployment) -> Option<&'a Deployment> {
    let id = w.serving(w.project_of(d)?)?;
    w.find(&id)
}

/// Whether the action applies to this deployment, or why not.
pub fn check(action: Action, d: &Deployment, w: &World) -> Result<(), String> {
    let current_id = w.project_of(d).and_then(|p| w.serving(p));
    match action {
        Action::Cancel if !d.running() => Err("only a build that's queued or running can be canceled".into()),
        Action::Redeploy if d.running() => Err("it's still building".into()),
        Action::Promote | Action::Rollback if d.state() != "READY" => Err("it isn't ready".into()),
        Action::Promote | Action::Rollback if current_id.as_deref() == Some(&d.uid) => {
            Err("it's already serving production".into())
        }
        Action::Rollback if !d.production() => {
            Err("only an earlier production deployment can be rolled back to; P promotes a preview".into())
        }
        Action::Rollback if current(w, d).is_some_and(|c| c.created < d.created) => {
            Err("it's newer than production: P promotes it".into())
        }
        Action::Promote | Action::Rollback if w.project_of(d).is_none() => {
            Err("ooze doesn't know its project yet".into())
        }
        _ => Ok(()),
    }
}

/// One line about the deployment, for the dialog.
fn summary(d: &Deployment, now: i64) -> String {
    let mut parts = vec![short(&d.uid), d.environment().to_string()];
    if let Some(b) = d.branch() {
        parts.push(b);
    }
    if let Some(c) = d.commit() {
        parts.push(fmt::sha(&c));
    }
    if let Some(m) = d.message() {
        parts.push(format!("\"{}\"", fmt::fit_end(&fmt::one_line(&m), 50)));
    }
    parts.push(format!("{} {}", d.state().to_lowercase(), fmt::ago(fmt::between(d.created, now))));
    parts.join(" · ")
}

/// What the dialog says: its title, and what will happen.
pub fn describe(action: Action, d: &Deployment, now: i64) -> (String, Vec<String>) {
    let project = &d.name;
    let env = d.environment();
    let title = match action {
        Action::Cancel => format!("Cancel {project}'s {env} build?"),
        Action::Redeploy => format!("Redeploy {project} to {env}?"),
        Action::Promote => format!("Promote this {project} deployment to production?"),
        Action::Rollback => format!("Roll {project}'s production back?"),
    };
    let what = match action {
        Action::Cancel => vec!["The build stops, and the deployment is marked canceled.".to_string()],
        Action::Redeploy => vec![format!(
            "Vercel builds the same commit again, to {env}, with the original deployment's settings and environment variables."
        )],
        Action::Promote if d.production() => vec!["Production traffic moves to this deployment within seconds.".into()],
        Action::Promote => vec![
            "It's a preview deployment, so Vercel builds it again with production's environment variables,".into(),
            "then moves production traffic to the new build.".into(),
        ],
        Action::Rollback => vec![
            "Production traffic moves back to this deployment within seconds.".into(),
            "Until a deployment is promoted again, new production deployments won't go live by themselves.".into(),
        ],
    };
    let mut lines = vec![summary(d, now), String::new()];
    lines.extend(what);
    (title, lines)
}

/// Does it, and says what it did in words for the feed.
pub fn run(api: &Api, action: Action, d: &Deployment, project: &str) -> Result<String, String> {
    let env = d.environment();
    let commit = d.commit().map(|c| fmt::sha(&c)).unwrap_or_else(|| short(&d.uid));
    let said = match action {
        Action::Cancel => vercel::cancel(api, &d.uid).map(|_| format!("canceled the {env} build of {commit}")),
        Action::Redeploy => vercel::redeploy(api, d).map(|n| {
            let url = n.url.map(|u| format!(": {u}")).unwrap_or_default();
            format!("redeployed {commit} to {env}{url}")
        }),
        Action::Promote => {
            vercel::promote(api, project, &d.uid).map(|_| format!("promoted {} to production", short(&d.uid)))
        }
        Action::Rollback => {
            vercel::rollback(api, project, &d.uid).map(|_| format!("rolled production back to {}", short(&d.uid)))
        }
    };
    said.map_err(|e| format!("couldn't {} {}: {e}", action.verb(), short(&d.uid)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::model::Msg;
    use crate::model::tests::{NOW, deployment};
    use serde_json::json;

    /// moth-radio with a build running, one live, an older one and a preview;
    /// `current` other than "live" is where production was rolled back to.
    fn world(current: &str) -> World {
        let mut w = World::new(NOW, Config::default());
        let rollback = (current != "live").then(|| json!({"type": "rollback", "toDeploymentId": current, "jobStatus": "succeeded", "requestedAt": NOW - 60_000}));
        let p = serde_json::from_value(json!({"id": "prj_moth-radio", "name": "moth-radio", "targets": {"production": {"id": "new"}}, "lastAliasRequest": rollback})).unwrap();
        w.apply(Msg::Projects(vec![p]), NOW);
        w.apply(
            Msg::Deployments(vec![
                deployment("new", "moth-radio", "BUILDING", Some("production"), NOW),
                deployment("live", "moth-radio", "READY", Some("production"), NOW - 3_600_000),
                deployment("old", "moth-radio", "READY", Some("production"), NOW - 7_200_000),
                deployment("pre", "moth-radio", "READY", None, NOW - 600_000),
            ]),
            NOW,
        );
        w
    }

    #[test]
    fn each_action_says_when_it_doesnt_apply() {
        let w = world("live");
        let d = |uid: &str| w.find(uid).unwrap().clone();
        assert!(check(Action::Cancel, &d("new"), &w).is_ok());
        assert_eq!(
            check(Action::Cancel, &d("live"), &w),
            Err("only a build that's queued or running can be canceled".into())
        );
        assert_eq!(check(Action::Redeploy, &d("new"), &w), Err("it's still building".into()));
        assert!(check(Action::Rollback, &d("old"), &w).is_ok());
        assert_eq!(check(Action::Rollback, &d("live"), &w), Err("it's already serving production".into()));
        assert!(check(Action::Rollback, &d("pre"), &w).unwrap_err().contains("P promotes a preview"));
        assert!(check(Action::Promote, &d("pre"), &w).is_ok());
        assert_eq!(check(Action::Promote, &d("new"), &w), Err("it isn't ready".into()));
        let rolled_back = world("old");
        assert_eq!(
            check(Action::Rollback, &rolled_back.find("live").unwrap().clone(), &rolled_back),
            Err("it's newer than production: P promotes it".into())
        );
    }

    #[test]
    fn the_dialog_says_what_will_happen() {
        let w = world("live");
        let (title, lines) = describe(Action::Rollback, w.find("old").unwrap(), NOW);
        assert_eq!(title, "Roll moth-radio's production back?");
        assert_eq!(lines[0], "old · production · main · a1b2c3d · \"Fix search\" · ready 2 h ago");
        assert!(lines.iter().any(|l| l.contains("won't go live by themselves")));
        let (_, lines) = describe(Action::Promote, w.find("pre").unwrap(), NOW);
        assert!(lines.iter().any(|l| l.contains("builds it again with production's environment variables")));
    }

    #[test]
    fn doing_it_calls_vercel_and_says_what_happened() {
        let f = crate::api::tests::fake(vec![
            (201, vec![], String::new()),
            (403, vec![], r#"{"error":{"message":"You don't have permission"}}"#.into()),
        ]);
        let api = crate::api::tests::api(&f.base);
        let w = world("live");
        let old = w.find("old").unwrap().clone();
        assert_eq!(run(&api, Action::Rollback, &old, "prj_moth-radio"), Ok("rolled production back to old".into()));
        assert!(f.seen.lock().unwrap()[0].starts_with("POST /v1/projects/prj_moth-radio/rollback/old?teamId=team_1 "));
        let err = run(&api, Action::Cancel, &w.find("new").unwrap().clone(), "prj_moth-radio").unwrap_err();
        assert_eq!(err, "couldn't cancel new: not allowed: You don't have permission");
    }
}
