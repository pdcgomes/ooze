//! The feed: a line for everything worth seeing, kept oldest first and shown
//! newest first.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Kind {
    Deploys,
    Traffic,
    Errors,
    Vercel,
    Team,
    Domains,
    Spend,
}

impl Kind {
    pub const ALL: [Kind; 7] = [Kind::Deploys, Kind::Traffic, Kind::Errors, Kind::Vercel, Kind::Team, Kind::Domains, Kind::Spend];

    pub fn name(self) -> &'static str {
        match self {
            Kind::Deploys => "deploys",
            Kind::Traffic => "traffic",
            Kind::Errors => "errors",
            Kind::Vercel => "Vercel",
            Kind::Team => "team",
            Kind::Domains => "domains",
            Kind::Spend => "spend",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Work,
    Good,
    Warn,
    Bad,
    Muted,
    Up,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub at: i64,
    pub kind: Kind,
    pub glyph: &'static str,
    pub tone: Tone,
    /// The project it's about, or "Vercel", or the team.
    pub who: String,
    pub text: String,
    pub detail: Vec<(String, String)>,
    pub deployment: Option<String>,
    pub project: Option<String>,
    /// When it came in, for easing it in; 0 for what was there at the start.
    pub arrived: i64,
}

impl Line {
    pub fn new(at: i64, kind: Kind, glyph: &'static str, tone: Tone, who: impl Into<String>, text: impl Into<String>) -> Line {
        Line { at, kind, glyph, tone, who: who.into(), text: text.into(), detail: vec![], deployment: None, project: None, arrived: 0 }
    }

    pub fn detail(mut self, key: &str, value: impl Into<String>) -> Line {
        let value = value.into();
        if !value.is_empty() {
            self.detail.push((key.to_string(), value));
        }
        self
    }

    pub fn about(mut self, project: Option<String>, deployment: Option<String>) -> Line {
        self.project = project;
        self.deployment = deployment;
        self
    }

    pub fn matches(&self, filter: &str) -> bool {
        filter.is_empty() || self.who.to_lowercase().contains(filter) || self.text.to_lowercase().contains(filter)
    }
}
