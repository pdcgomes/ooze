//! The view: Omarchy's look in Tokyo Night, like sweep, jobtop and hauntop.
//! Rounded panels with a column of air either side, a pill for the title,
//! thin meters, and the keys in the last panel's bottom border. The panel
//! holding the selection is outlined in blue.
//!
//! It draws when something changes, ten times a second while something
//! moves on screen, and once a second otherwise.

use crate::actions::{self, Action};
use crate::art::{self, Look, Sprite};
use crate::billing;
use crate::config::Config;
use crate::feed::{Kind, Line as FeedLine, Tone};
use crate::fmt;
use crate::model::{Link, Msg, Row, SHOWN, Source, World};
use crate::notify::Notifier;
use crate::poll::{Envelope, Hub};
use crate::theme::Theme;
use crate::vercel::Deployment;
use anyhow::Result;
use ratatui::Frame;
use ratatui::crossterm::event::{
    self, Event as Input, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Padding, Paragraph};
use std::collections::HashSet;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// How long a new line or a changed row takes to ease in.
const EASE: i64 = 1_500;
/// The Slime glows this long after something happens.
const GLOW: i64 = 900;
const DAY: i64 = 86_400_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Panel {
    Projects,
    Deployments,
    Billing,
    Feed,
}

const ORDER: [Panel; 4] = [Panel::Projects, Panel::Deployments, Panel::Billing, Panel::Feed];

#[derive(Clone, Debug, PartialEq)]
enum Overlay {
    Help,
    Project(String),
    Deployment(String),
    Billing,
    Line(Box<FeedLine>),
    Confirm(Action, String),
}

pub struct View {
    pub world: World,
    pub theme: Theme,
    truecolor: bool,
    pub no_color: bool,
    focus: Panel,
    project: Option<String>,
    deployment: Option<String>,
    /// The selected line of the feed, counting from the newest shown.
    line: usize,
    filter: String,
    editing: bool,
    hidden: HashSet<Kind>,
    /// Lines fed when the feed was paused.
    paused: Option<u64>,
    zoom: bool,
    overlay: Option<Overlay>,
    scroll: u16,
    /// A log's details stay at its end as lines come, until scrolled up.
    stick: bool,
    all: bool,
    pub quit: bool,
    flash: Option<(String, Instant)>,
    sprite: Option<Sprite>,
    hub: Option<Hub>,
    generation: u64,
    notifier: Option<Notifier>,
    read_only: bool,
    /// Whether the last frame was wide enough for the billing panel.
    wide: bool,
    /// A clock that stands still, for frames that must come out the same.
    pub clock: Option<i64>,
    /// Where each panel was last drawn, for the mouse.
    areas: Vec<(Panel, Rect)>,
}

impl View {
    pub fn new(hub: Option<Hub>, cfg: &Config, notifier: Option<Notifier>) -> View {
        let truecolor = crate::theme::truecolor();
        let now = chrono::Utc::now().timestamp_millis();
        View {
            world: World::new(now, cfg.clone()),
            theme: Theme::with(truecolor),
            truecolor,
            no_color: crate::theme::no_color(),
            focus: Panel::Projects,
            project: None,
            deployment: None,
            line: 0,
            filter: String::new(),
            editing: false,
            hidden: HashSet::new(),
            paused: None,
            zoom: false,
            overlay: None,
            scroll: 0,
            stick: true,
            all: false,
            quit: false,
            flash: None,
            sprite: None,
            hub,
            generation: 0,
            notifier,
            read_only: cfg.read_only,
            wide: true,
            clock: None,
            areas: Vec::new(),
        }
    }

    pub fn now(&self) -> i64 {
        self.clock.unwrap_or_else(|| chrono::Utc::now().timestamp_millis())
    }

    fn moving(&self, now: i64) -> bool {
        self.world.moving(now) || self.flash.as_ref().is_some_and(|(_, at)| at.elapsed() < Duration::from_secs(4))
    }

    fn say(&mut self, text: impl Into<String>) {
        self.flash = Some((text.into(), Instant::now()));
    }

    /// Folds in what a source said, unless it was for a team no longer watched.
    pub fn take(&mut self, env: Envelope) {
        let now = self.now();
        if env.generation < self.generation {
            return;
        }
        if env.generation > self.generation {
            if self.world.team.is_some() {
                self.world.switch(now);
            }
            self.generation = env.generation;
        }
        self.world.apply(env.msg, now);
        let notices = std::mem::take(&mut self.world.notices);
        if let Some(n) = &mut self.notifier {
            for notice in &notices {
                n.send(notice);
            }
        }
    }

    fn creature(&mut self) -> Option<&Sprite> {
        Some(self.sprite.get_or_insert_with(Sprite::slime))
    }

    pub fn look(&self, now: i64) -> Look {
        let glow = 1.0 - art::progress(self.world.stirred, now, GLOW);
        Look {
            glow: if self.world.stirred > 0 { glow } else { 0.0 },
            blink: now % 4_700 < 160,
            asleep: self.world.link != Link::Up,
            low: (now / 1_300) % 2 == 1,
        }
    }

    fn panel(&self, border: Color) -> Block<'static> {
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::new().fg(border))
            .style(Style::new().bg(self.theme.bg).fg(self.theme.fg))
    }

    fn border(&self, p: Panel) -> Color {
        if self.focus == p && self.overlay.is_none() { self.theme.blue } else { self.theme.gutter }
    }

    fn title(&self, name: &str, more: Vec<Span<'static>>) -> Line<'static> {
        let mut spans = vec![Span::styled(format!(" {name} "), Style::new().fg(self.theme.text))];
        spans.extend(more);
        Line::from(spans)
    }

    pub fn draw(&mut self, f: &mut Frame) {
        let now = self.now();
        let t = self.theme;
        f.render_widget(Block::new().style(Style::new().bg(t.bg).fg(t.fg)), f.area());
        // A column of air either side, like the gaps between Omarchy's windows.
        let area = f.area().inner(Margin { horizontal: 1, vertical: 0 });
        self.areas.clear();
        let [top, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(4)]).areas(area);
        self.draw_header(f, top, now);
        if self.world.team.is_none() || matches!(self.world.link, Link::Refused(_)) {
            self.draw_waiting(f, body, now);
        } else {
            self.draw_body(f, body, now);
        }
        match self.overlay.clone() {
            Some(Overlay::Help) => self.draw_help(f),
            Some(Overlay::Confirm(action, uid)) => self.draw_confirm(f, action, &uid, now),
            Some(o) => self.draw_details(f, &o, now),
            None => {}
        }
        // With NO_COLOR set, crossterm turns each colour change into a full
        // attribute reset, which would wipe out the reversed selection.
        if self.no_color {
            for cell in f.buffer_mut().content.iter_mut() {
                cell.set_fg(Color::Reset);
                cell.set_bg(Color::Reset);
            }
        }
    }

    fn draw_header(&mut self, f: &mut Frame, area: Rect, now: i64) {
        let t = self.theme;
        let pill = |text: &str, bg: Color| {
            Span::styled(format!(" {text} "), Style::new().bg(bg).fg(t.bg).add_modifier(Modifier::BOLD))
        };
        let mut title = vec![Span::raw(" "), pill("ooze", t.teal)];
        let mut border = if self.world.troubled(now) { t.orange } else { t.gutter };
        match &self.world.link {
            Link::Connecting => title.push(Span::styled(" signing in to Vercel… ", Style::new().fg(t.comment))),
            Link::Up => {
                if let Some(team) = &self.world.team {
                    let plan = team.plan().map(|p| format!(" · {}", capitalised(p))).unwrap_or_default();
                    title.push(Span::styled(format!(" {}{plan} ", team.title()), Style::new().fg(t.text)));
                }
            }
            Link::Down(why) => {
                border = t.red;
                let since = fmt::span(fmt::between(self.world.since, now));
                title.push(Span::styled(
                    format!(" can't reach Vercel: {} · for {since}, trying again ", fmt::fit_end(why, 60)),
                    Style::new().fg(t.red),
                ));
            }
            Link::Refused(_) => {
                border = t.red;
                title.push(Span::styled(" Vercel refused the token ", Style::new().fg(t.red)));
            }
        }
        let light = |name: &str, c: Color| {
            vec![Span::styled(format!(" {name} "), Style::new().fg(t.comment)), Span::styled("●", Style::new().fg(c))]
        };
        let status = match self.world.status.as_ref().map(|s| s.status.indicator.as_str()) {
            Some("none") => t.green,
            Some("minor") => t.yellow,
            Some("major") => t.orange,
            Some("critical") => t.red,
            _ => t.gutter,
        };
        let api = match &self.world.link {
            Link::Down(_) | Link::Refused(_) => t.red,
            _ if self.world.limited.is_some_and(|until| until > now) => t.orange,
            _ if self.world.failed.iter().any(|(s, (at, _))| *s != Source::Status && now - at < 120_000) => t.yellow,
            Link::Up => t.green,
            Link::Connecting => t.gutter,
        };
        let mut right = light("vercel", status);
        right.extend(light("api", api));
        let updated = self.world.updated.get(&Source::Traffic).or(self.world.updated.get(&Source::Deployments));
        if let Some(at) = updated {
            right.push(Span::styled(
                format!("  updated {}", fmt::ago(fmt::between(*at, now))),
                Style::new().fg(t.comment),
            ));
        }
        let clock = match self.clock {
            Some(at) => fmt::time(at),
            None => chrono::Local::now().format("%H:%M:%S").to_string(),
        };
        right.push(Span::styled(format!("  {clock} "), Style::new().fg(t.text)));
        let block = self.panel(border).title(Line::from(title)).title(Line::from(right).right_aligned());
        let inner = block.inner(area);
        f.render_widget(block, area);
        f.render_widget(Paragraph::new(self.header_line(inner.width as usize, now)), inner);
    }

    fn header_line(&self, width: usize, now: i64) -> Line<'static> {
        let t = &self.theme;
        if self.world.team.is_none() {
            return Line::default();
        }
        let g = self.world.glance(now);
        let label = |s: &str| Span::styled(s.to_string(), Style::new().fg(t.comment));
        // The richest line that fits: the sparkline goes first, then the counts.
        let left = |spark_w: usize, today: bool, counts: bool| -> Vec<Span<'static>> {
            let mut left: Vec<Span> = vec![
                Span::raw(" "),
                label("online "),
                Span::styled(fmt::num(g.online), Style::new().fg(t.teal).add_modifier(Modifier::BOLD)),
            ];
            if spark_w > 0 {
                left.push(Span::raw("  "));
                left.push(Span::styled(
                    art::sparkline(&g.minutes[g.minutes.len().saturating_sub(spark_w)..]),
                    Style::new().fg(t.teal),
                ));
            }
            if today {
                left.push(Span::raw("  "));
                left.push(label("today "));
                left.push(Span::styled(
                    format!(
                        "{} · {}",
                        fmt::count(g.visitors, "visitor", "visitors"),
                        fmt::count(g.views, "view", "views")
                    ),
                    Style::new().fg(t.fg),
                ));
            }
            if counts && g.building > 0 {
                left.push(Span::styled(format!("  ⠿ {} building", g.building), Style::new().fg(t.blue)));
            }
            if counts && g.failed_today > 0 {
                left.push(Span::styled(format!("  ✗ {} failed today", g.failed_today), Style::new().fg(t.red)));
            }
            left
        };
        let mut right = vec![];
        if let Some(usage) = g.usage {
            let (text, colour) = match g.credit {
                Some(c) if c > 0.0 => {
                    (format!("{} of {}", fmt::money(usage), fmt::dollars(c)), ratio_colour(usage / c, t))
                }
                _ => (fmt::money(usage), t.fg),
            };
            right.push(label("spent "));
            right.push(Span::styled(text, Style::new().fg(colour)));
            right.push(Span::raw(" "));
        }
        let room = width.saturating_sub(right.iter().map(|s| s.width()).sum());
        let width_of = |spans: &[Span]| spans.iter().map(|s| s.width()).sum::<usize>();
        let line = [
            (SHOWN, true, true),
            (20, true, true),
            (12, true, true),
            (0, true, true),
            (0, true, false),
            (0, false, false),
        ]
        .into_iter()
        .map(|(s, today, counts)| left(s, today, counts))
        .find(|l| width_of(l) < room)
        .unwrap_or_else(|| left(0, false, false));
        fill(line, right, width)
    }

    fn draw_waiting(&mut self, f: &mut Frame, area: Rect, now: i64) {
        let t = self.theme;
        let block = self.panel(t.gutter);
        let inner = block.inner(area);
        f.render_widget(block, area);
        let look = self.look(now);
        let (theme, truecolor) = (self.theme, self.truecolor);
        let mut lines = match self
            .creature()
            .and_then(|s| s.fitting(inner.width as usize, (inner.height as usize).saturating_sub(5)))
        {
            Some(s) => s.lines(look, &theme, truecolor),
            None => vec![],
        };
        let said = match &self.world.link {
            Link::Connecting => vec!["Asking Vercel who you are…".to_string()],
            Link::Up => vec!["Fetching your projects…".to_string()],
            Link::Down(why) => vec![format!("Can't reach Vercel: {why}."), "ooze keeps trying.".into()],
            Link::Refused(why) => vec![
                format!("Vercel refused the token: {why}."),
                "`ooze login` signs in with a new one, or `vercel login` renews the Vercel CLI's.".into(),
            ],
        };
        lines.push(Line::default());
        for s in said {
            lines.push(Line::from(Span::styled(s, Style::new().fg(t.text))));
        }
        let pad = inner.height.saturating_sub(lines.len() as u16) / 2;
        let mut out = vec![Line::default(); pad as usize];
        out.extend(lines.into_iter().map(|l| l.centered()));
        f.render_widget(Paragraph::new(out), inner);
    }

    fn draw_body(&mut self, f: &mut Frame, body: Rect, now: i64) {
        self.wide = body.width >= 120;
        if self.zoom {
            let p = if !self.wide && self.focus == Panel::Billing { Panel::Projects } else { self.focus };
            self.draw_panel(f, p, body, now);
            return;
        }
        let (rows, folded) = self.world.rows(now, self.all);
        let shown = rows.iter().filter(|r| self.matches(&r.name)).count();
        let wanted = (shown + 1 + usize::from(folded > 0 || self.all) + 2).max(5) as u16;
        let middle_h = if self.wide && body.height >= 44 { 11 } else { 9 };
        let projects_h = wanted.min(body.height.saturating_sub(middle_h + 6)).max(5);
        let [top, middle, lower] =
            Layout::vertical([Constraint::Length(projects_h), Constraint::Length(middle_h), Constraint::Min(5)])
                .areas(body);
        self.draw_panel(f, Panel::Projects, top, now);
        if self.wide {
            let [dep, bill] =
                Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)]).areas(middle);
            self.draw_panel(f, Panel::Deployments, dep, now);
            self.draw_panel(f, Panel::Billing, bill, now);
        } else {
            if self.focus == Panel::Billing {
                self.focus = Panel::Deployments;
            }
            self.draw_panel(f, Panel::Deployments, middle, now);
        }
        self.draw_panel(f, Panel::Feed, lower, now);
    }

    fn draw_panel(&mut self, f: &mut Frame, p: Panel, area: Rect, now: i64) {
        self.areas.push((p, area));
        match p {
            Panel::Projects => self.draw_projects(f, area, now),
            Panel::Deployments => self.draw_deployments(f, area, now),
            Panel::Billing => self.draw_billing(f, area, now),
            Panel::Feed => self.draw_feed(f, area, now),
        }
    }

    fn matches(&self, s: &str) -> bool {
        self.filter.is_empty() || s.to_lowercase().contains(&self.filter.to_lowercase())
    }

    fn filter_title(&self) -> Option<Line<'static>> {
        if !self.editing && self.filter.is_empty() {
            return None;
        }
        let t = &self.theme;
        let cursor = if self.editing { "█" } else { "" };
        Some(Line::from(vec![
            Span::styled(" / ", Style::new().fg(t.blue).add_modifier(Modifier::BOLD)),
            Span::styled(format!("{}{cursor} ", self.filter), Style::new().fg(t.cyan)),
        ]))
    }

    fn shown_rows(&self, now: i64) -> (Vec<Row>, usize) {
        let (rows, folded) = self.world.rows(now, self.all);
        (rows.into_iter().filter(|r| self.matches(&r.name)).collect(), folded)
    }

    fn draw_projects(&mut self, f: &mut Frame, area: Rect, now: i64) {
        let t = self.theme;
        let (rows, folded) = self.shown_rows(now);
        let busy = rows.iter().filter(|r| r.online > 0).count();
        let more = vec![Span::styled(
            format!("· {} with visitors ", busy),
            Style::new().fg(if busy > 0 { t.teal } else { t.comment }),
        )];
        let mut block = self
            .panel(self.border(Panel::Projects))
            .title(self.title("projects", more))
            .padding(Padding::horizontal(1));
        if let Some(filter) = self.filter_title() {
            block = block.title(filter.right_aligned());
        }
        let inner = block.inner(area);
        f.render_widget(block, area);
        if self.project.as_ref().is_none_or(|p| !rows.iter().any(|r| &r.id == p)) {
            self.project = rows.first().map(|r| r.id.clone());
        }
        let w = inner.width as usize;
        let cols = Columns::for_width(w);
        let mut lines = vec![self.projects_heading(&cols)];
        let mut selected = 0;
        for r in &rows {
            if Some(&r.id) == self.project.as_ref() {
                selected = lines.len();
            }
            lines.push(self.project_line(r, &cols, w, now));
        }
        if folded > 0 || self.all {
            let what = if self.all {
                "a folds the quiet ones away".to_string()
            } else {
                format!("· {} · a shows them", fmt::count(folded as u64, "quiet project", "quiet projects"))
            };
            lines.push(Line::from(Span::styled(format!("  {what}"), Style::new().fg(t.comment))));
        }
        if rows.is_empty() {
            let quiet =
                if self.filter.is_empty() { "  Nothing's happening on any project." } else { "  Nothing matches." };
            lines.insert(1, Line::from(Span::styled(quiet, Style::new().fg(t.comment))));
        }
        let h = inner.height as usize;
        let offset = selected.saturating_sub(h.saturating_sub(2));
        let mut shown: Vec<Line> = vec![lines[0].clone()];
        shown.extend(lines.into_iter().skip(1 + offset).take(h.saturating_sub(1)));
        f.render_widget(Paragraph::new(shown), inner);
    }

    fn projects_heading(&self, c: &Columns) -> Line<'static> {
        let mut s = format!("  {}", fmt::pad("project", c.name));
        s += &fmt::rpad("online", c.online);
        if c.spark > 0 {
            s += &format!("  {}", fmt::pad(&format!("last {} min", c.spark.min(SHOWN)), c.spark));
        }
        s += &fmt::rpad("views/h", c.hour);
        if c.today > 0 {
            s += &fmt::rpad("today", c.today);
        }
        if c.errors > 0 {
            s += &fmt::rpad("5xx", c.errors);
        }
        if c.cache > 0 {
            s += &fmt::rpad("cache", c.cache);
        }
        s += "  production";
        Line::from(Span::styled(s, Style::new().fg(self.theme.comment)))
    }

    fn project_line(&self, r: &Row, c: &Columns, w: usize, now: i64) -> Line<'static> {
        let t = &self.theme;
        let (glyph, glyph_colour) = match (&r.production, r.attack) {
            (_, true) => ("⊘".to_string(), t.red),
            (Some(p), _) => match p.state.as_str() {
                "BUILDING" | "QUEUED" | "INITIALIZING" => (art::spinner(now, &r.id).to_string(), t.blue),
                "ERROR" => ("●".into(), t.red),
                "CANCELED" => ("●".into(), t.comment),
                _ => ("●".into(), t.green),
            },
            (None, _) => ("○".into(), t.comment),
        };
        let mut spans = vec![
            Span::styled(format!("{glyph} "), Style::new().fg(glyph_colour)),
            Span::styled(
                fmt::pad(&r.name, c.name),
                Style::new().fg(art::project_colour(&r.name, self.truecolor)).add_modifier(Modifier::BOLD),
            ),
        ];
        let online = if r.analytics { fmt::rpad(&fmt::num(r.online), c.online) } else { fmt::rpad("–", c.online) };
        spans.push(Span::styled(online, Style::new().fg(if r.online > 0 { t.teal } else { t.comment })));
        if c.spark > 0 {
            let values: Vec<u64> = r.minutes[r.minutes.len().saturating_sub(c.spark)..].to_vec();
            let line = fmt::pad(&art::sparkline(&values), c.spark);
            spans.push(Span::raw("  "));
            spans.push(Span::styled(line, Style::new().fg(if r.analytics { t.teal } else { t.muted })));
        }
        let hour = if r.analytics { fmt::compact(r.hour) } else { format!("{} req", fmt::compact(r.hour)) };
        spans.push(Span::styled(fmt::rpad(&hour, c.hour), Style::new().fg(if r.analytics { t.fg } else { t.muted })));
        if c.today > 0 {
            spans.push(Span::styled(
                fmt::rpad(&r.today.map(fmt::compact).unwrap_or_default(), c.today),
                Style::new().fg(t.text),
            ));
        }
        if c.errors > 0 {
            let (text, colour) = match r.errors {
                Some(e) if e >= 0.01 => (fmt::percent(e), t.red),
                Some(e) if e > 0.0 => (fmt::percent(e), t.yellow),
                Some(_) => ("0%".to_string(), t.comment),
                None => (String::new(), t.comment),
            };
            spans.push(Span::styled(fmt::rpad(&text, c.errors), Style::new().fg(colour)));
        }
        if c.cache > 0 {
            spans.push(Span::styled(
                fmt::rpad(&r.cache.map(fmt::percent).unwrap_or_default(), c.cache),
                Style::new().fg(t.muted),
            ));
        }
        spans.push(Span::raw("  "));
        spans.extend(self.production_spans(r, now));
        let line = Line::from(spans);
        let line = clip(line, w);
        self.lift(line, Some(&r.id) == self.project.as_ref() && self.focus == Panel::Projects, 0, now, w)
    }

    fn production_spans(&self, r: &Row, now: i64) -> Vec<Span<'static>> {
        let t = &self.theme;
        let mut spans = match &r.production {
            None => vec![Span::styled("no production deployment", Style::new().fg(t.comment))],
            Some(p) => match p.state.as_str() {
                "BUILDING" | "QUEUED" | "INITIALIZING" => vec![Span::styled(
                    format!("building {}", fmt::clock(fmt::between(p.since, now))),
                    Style::new().fg(t.blue),
                )],
                "ERROR" => vec![
                    Span::styled("failed", Style::new().fg(t.red).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" · {}", fmt::ago(fmt::between(p.at, now))), Style::new().fg(t.comment)),
                ],
                _ if p.at == 0 => vec![Span::styled("ready", Style::new().fg(t.green))],
                _ => vec![
                    Span::styled("ready", Style::new().fg(t.green)),
                    Span::styled(format!(" · {}", fmt::ago(fmt::between(p.at, now))), Style::new().fg(t.comment)),
                ],
            },
        };
        if r.attack {
            spans.push(Span::styled(" · under attack", Style::new().fg(t.red).add_modifier(Modifier::BOLD)));
        } else if r.blocked > 0 {
            spans.push(Span::styled(format!(" · {} blocked", fmt::compact(r.blocked)), Style::new().fg(t.orange)));
        }
        spans
    }

    fn shown_deployments(&self) -> Vec<&Deployment> {
        self.world
            .deploy_rows()
            .into_iter()
            .filter(|d| {
                self.matches(&d.name)
                    || d.message().is_some_and(|m| self.matches(&m))
                    || d.branch().is_some_and(|b| self.matches(&b))
            })
            .collect()
    }

    fn draw_deployments(&mut self, f: &mut Frame, area: Rect, now: i64) {
        let t = self.theme;
        let g = self.world.glance(now);
        let mut more = vec![Span::styled(
            format!("· {} building ", g.building),
            Style::new().fg(if g.building > 0 { t.blue } else { t.comment }),
        )];
        if g.failed_today > 0 {
            more.push(Span::styled(format!("· {} failed today ", g.failed_today), Style::new().fg(t.red)));
        }
        let block = self
            .panel(self.border(Panel::Deployments))
            .title(self.title("deployments", more))
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let list: Vec<Deployment> = self.shown_deployments().into_iter().cloned().collect();
        if self.deployment.as_ref().is_none_or(|d| !list.iter().any(|x| &x.uid == d)) {
            self.deployment = list.first().map(|d| d.uid.clone());
        }
        let w = inner.width as usize;
        let selected = list.iter().position(|d| Some(&d.uid) == self.deployment.as_ref()).unwrap_or(0);
        let h = inner.height as usize;
        let offset = selected.saturating_sub(h.saturating_sub(1));
        let shown: Vec<&Deployment> = list.iter().skip(offset).take(h).collect();
        let name_w =
            shown.iter().map(|d| d.name.chars().count()).max().unwrap_or(10).clamp(10, if w >= 84 { 18 } else { 14 })
                + 1;
        let lines: Vec<Line> = shown.iter().map(|d| self.deployment_line(d, name_w, w, now)).collect();
        if lines.is_empty() {
            let said = if self.world.updated.contains_key(&Source::Deployments) {
                "No deployments yet."
            } else {
                "Fetching deployments…"
            };
            f.render_widget(Paragraph::new(Span::styled(said, Style::new().fg(t.comment))), inner);
            return;
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn deployment_line(&self, d: &Deployment, name_w: usize, w: usize, now: i64) -> Line<'static> {
        let t = &self.theme;
        let (glyph, colour) = match d.state() {
            "BUILDING" => (art::spinner(now, &d.uid).to_string(), t.blue),
            "QUEUED" | "INITIALIZING" => ("◌".into(), t.yellow),
            "READY" => ("✓".into(), t.green),
            "ERROR" => ("✗".into(), t.red),
            _ => ("■".into(), t.comment),
        };
        let when = if d.running() {
            fmt::clock(fmt::between(d.building_at.unwrap_or(d.created), now))
        } else {
            fmt::age(fmt::between(d.ready.unwrap_or(d.created), now))
        };
        let env_colour = if d.production() { t.purple } else { t.muted };
        let live = self.world.is_current(d);
        let author_w = if w >= 90 { 10 } else { 0 };
        let branch_w = if w >= 70 { 14 } else { 0 };
        let fixed = 2 + name_w + 11 + 9 + branch_w + author_w + if live { 6 } else { 0 };
        let rest = w.saturating_sub(fixed + 2);
        let (what, what_colour) = match (d.state(), d.error_message.as_ref()) {
            ("ERROR", Some(e)) => (fmt::one_line(e), t.red),
            _ => (d.message().map(|m| fmt::one_line(&m)).unwrap_or_default(), t.fg),
        };
        let mut spans = vec![
            Span::styled(format!("{glyph} "), Style::new().fg(colour)),
            Span::styled(
                fmt::pad(&d.name, name_w),
                Style::new().fg(art::project_colour(&d.name, self.truecolor)).add_modifier(Modifier::BOLD),
            ),
            Span::styled(fmt::pad(d.environment(), 11), Style::new().fg(env_colour)),
            Span::styled(fmt::pad(&when, 9), Style::new().fg(if d.running() { t.blue } else { t.comment })),
        ];
        if branch_w > 0 {
            spans.push(Span::styled(fmt::pad(&d.branch().unwrap_or_default(), branch_w), Style::new().fg(t.muted)));
        }
        spans.push(Span::styled(fmt::pad(&what, rest), Style::new().fg(what_colour)));
        if author_w > 0 {
            spans.push(Span::styled(fmt::rpad(&d.author().unwrap_or_default(), author_w), Style::new().fg(t.comment)));
        }
        if live {
            spans.push(Span::styled("  live", Style::new().fg(t.green).add_modifier(Modifier::BOLD)));
        }
        self.lift(
            Line::from(spans),
            Some(&d.uid) == self.deployment.as_ref() && self.focus == Panel::Deployments,
            0,
            now,
            w,
        )
    }

    fn day_of(&self, now: i64) -> Option<(i64, i64)> {
        let p = self.world.team.as_ref()?.billing.as_ref()?.period?;
        let total = ((p.end - p.start) as f64 / DAY as f64).round() as i64;
        Some((((now - p.start) / DAY + 1).clamp(1, total.max(1)), total))
    }

    fn draw_billing(&mut self, f: &mut Frame, area: Rect, now: i64) {
        let t = self.theme;
        let more = match self.day_of(now) {
            Some((day, total)) => vec![Span::styled(format!("· day {day} of {total} "), Style::new().fg(t.comment))],
            None => vec![],
        };
        let block =
            self.panel(self.border(Panel::Billing)).title(self.title("billing", more)).padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let look = self.look(now);
        let (theme, truecolor) = (self.theme, self.truecolor);
        let sprite = self
            .creature()
            .map(|s| s.halved())
            .filter(|s| inner.width as usize >= s.w + 34 && inner.height as usize >= s.rows());
        let text_area = match &sprite {
            Some(s) => {
                let [text, art_area] =
                    Layout::horizontal([Constraint::Min(30), Constraint::Length(s.w as u16 + 1)]).areas(inner);
                let pad = art_area.height.saturating_sub(s.rows() as u16) / 2;
                let mut lines = vec![Line::default(); pad as usize];
                lines.extend(s.lines(look, &theme, truecolor));
                f.render_widget(Paragraph::new(lines), art_area);
                text
            }
            None => inner,
        };
        let lines = self.billing_lines(text_area.width as usize, now);
        f.render_widget(Paragraph::new(lines), text_area);
    }

    fn billing_lines(&self, w: usize, now: i64) -> Vec<Line<'static>> {
        let t = &self.theme;
        let muted = |s: String| Line::from(Span::styled(s, Style::new().fg(t.comment)));
        let Some(spend) = &self.world.spend else {
            return vec![match self.world.failed.get(&Source::Billing) {
                Some((_, crate::api::ApiError::Forbidden(why))) => {
                    muted(format!("Billing isn't open to this token: {why}."))
                }
                Some((_, e)) => muted(format!("Couldn't read billing: {e}")),
                None => muted("Fetching this period's charges…".into()),
            }];
        };
        let team = self.world.team.clone().unwrap_or_default();
        let usage = spend.usage();
        let mut lines = vec![];
        match billing::credit(&team).filter(|c| *c > 0.0) {
            Some(credit) => {
                let ratio = usage / credit;
                let colour = ratio_colour(ratio, t);
                let mut spans = vec![
                    Span::styled("usage ", Style::new().fg(t.comment)),
                    Span::styled(fmt::money(usage), Style::new().fg(colour).add_modifier(Modifier::BOLD)),
                    Span::styled(format!(" of {} credit ", fmt::dollars(credit)), Style::new().fg(t.text)),
                ];
                let share = format!(" {}", fmt::percent(ratio));
                let used: usize = spans.iter().map(|s| s.width()).sum::<usize>() + share.chars().count();
                spans.extend(art::meter(ratio, w.saturating_sub(used).clamp(3, 12), colour, t));
                spans.push(Span::styled(share, Style::new().fg(colour)));
                lines.push(Line::from(spans));
                if let Some(p) = spend.projected() {
                    let projected = format!("projected {} by {}", fmt::dollars(p), fmt::day(spend.period.end));
                    let over = p > credit;
                    let long = if over {
                        format!(" · {} over the credit", fmt::dollars(p - credit))
                    } else {
                        " · within the credit".to_string()
                    };
                    let short =
                        if over { format!(", {} over", fmt::dollars(p - credit)) } else { ", within it".to_string() };
                    let said = if projected.chars().count() + long.chars().count() <= w { long } else { short };
                    lines.push(Line::from(vec![
                        Span::styled(projected, Style::new().fg(t.text)),
                        Span::styled(said, Style::new().fg(if over { t.orange } else { t.green })),
                    ]));
                }
            }
            None => lines.push(Line::from(vec![
                Span::styled("usage ", Style::new().fg(t.comment)),
                Span::styled(format!("{} this period", fmt::money(usage)), Style::new().fg(t.fg)),
            ])),
        }
        let services: Vec<String> = spend
            .services()
            .into_iter()
            .take(3)
            .map(|(s, c)| format!("{} {}", short_service(&s), fmt::money(c)))
            .collect();
        if !services.is_empty() {
            lines.push(Line::from(Span::styled(fmt::fit_end(&services.join(" · "), w), Style::new().fg(t.text))));
        }
        let ai = self.world.ai.as_ref().filter(|a| a.requests_today > 0 || a.cost_today > 0.0);
        if let Some(b) = self.world.budgets.iter().find(|b| b.scope_type == "team") {
            let period =
                b.refresh_period.as_deref().filter(|p| *p != "none").map(|p| format!(" {p}")).unwrap_or_default();
            lines.push(Line::from(Span::styled(
                fmt::fit_end(
                    &format!(
                        "AI Gateway {} of {}{period} budget",
                        fmt::money(b.current_spend),
                        fmt::money(b.limit_amount)
                    ),
                    w,
                ),
                Style::new().fg(t.text),
            )));
        } else if let Some(ai) = ai {
            lines.push(Line::from(Span::styled(
                format!(
                    "AI Gateway {} today · {}",
                    fmt::money(ai.cost_today),
                    fmt::count(ai.requests_today, "request", "requests")
                ),
                Style::new().fg(t.text),
            )));
        }
        let fixed: Vec<String> =
            billing::fixed(&team).into_iter().map(|(n, c)| format!("{n} {}", fmt::dollars(c))).collect();
        if !fixed.is_empty() {
            lines.push(Line::from(Span::styled(
                fmt::fit_end(&format!("{} a month", fixed.join(" · ")), w),
                Style::new().fg(t.muted),
            )));
        }
        let through =
            spend.latest().map(|d| format!("charges to {}", fmt::day(d))).unwrap_or_else(|| "no charges yet".into());
        lines.push(muted(format!("{through} · read {}", fmt::ago(fmt::between(spend.fetched, now)))));
        lines
    }

    /// The feed's lines as shown: the kinds not hidden, matching the filter,
    /// up to where it was paused, newest first.
    fn shown(&self) -> Vec<&FeedLine> {
        let w = &self.world;
        let first = w.fed - w.feed.len() as u64;
        let upto = self.paused.unwrap_or(w.fed);
        let filter = self.filter.to_lowercase();
        w.feed
            .iter()
            .enumerate()
            .filter(|(i, _)| first + (*i as u64) < upto)
            .map(|(_, l)| l)
            .filter(|l| !self.hidden.contains(&l.kind) && l.matches(&filter))
            .rev()
            .collect()
    }

    fn draw_feed(&mut self, f: &mut Frame, area: Rect, now: i64) {
        let t = self.theme;
        let mut more = vec![];
        if !self.hidden.is_empty() {
            let showing: Vec<&str> = Kind::ALL.iter().filter(|k| !self.hidden.contains(k)).map(|k| k.name()).collect();
            let showing = if showing.is_empty() { "nothing".to_string() } else { showing.join(", ") };
            more.push(Span::styled(format!("· {showing} "), Style::new().fg(t.comment)));
        }
        if let Some(at) = self.paused {
            let new = self.world.fed - at;
            more.push(Span::styled(" paused ", Style::new().bg(t.yellow).fg(t.bg).add_modifier(Modifier::BOLD)));
            more.push(Span::styled(
                format!(" {} · p carries on ", fmt::count(new, "new line", "new lines")),
                Style::new().fg(t.yellow),
            ));
        }
        let block = self
            .panel(self.border(Panel::Feed))
            .title(self.title("feed", more))
            .title_bottom(self.hints())
            .padding(Padding::horizontal(1));
        let inner = block.inner(area);
        f.render_widget(block, area);
        let shown = self.shown();
        let h = inner.height as usize;
        let selected = self.line.min(shown.len().saturating_sub(1));
        let offset = selected.saturating_sub(h.saturating_sub(1));
        let w = inner.width as usize;
        let focused = self.focus == Panel::Feed;
        let who_w = shown
            .iter()
            .skip(offset)
            .take(h)
            .map(|l| l.who.chars().count())
            .max()
            .unwrap_or(10)
            .clamp(10, if w >= 100 { 18 } else { 14 })
            + 1;
        let lines: Vec<Line> = shown
            .iter()
            .enumerate()
            .skip(offset)
            .take(h)
            .map(|(i, l)| self.lift(self.feed_line(l, who_w, w, now), focused && i == selected, l.arrived, now, w))
            .collect();
        if lines.is_empty() {
            let quiet = if self.filter.is_empty() && self.hidden.is_empty() {
                "Nothing has happened yet."
            } else {
                "Nothing matches."
            };
            f.render_widget(Paragraph::new(Span::styled(quiet, Style::new().fg(t.comment))), inner);
            return;
        }
        f.render_widget(Paragraph::new(lines), inner);
    }

    fn tone(&self, tone: Tone) -> Color {
        let t = &self.theme;
        match tone {
            Tone::Plain => t.fg,
            Tone::Work => t.blue,
            Tone::Good => t.green,
            Tone::Warn => t.orange,
            Tone::Bad => t.red,
            Tone::Muted => t.comment,
            Tone::Up => t.teal,
        }
    }

    fn feed_line(&self, l: &FeedLine, who_w: usize, w: usize, now: i64) -> Line<'static> {
        let t = &self.theme;
        let project = l.project.is_some() || self.world.projects.iter().any(|p| p.name == l.who);
        let who_style = if project {
            Style::new().fg(art::project_colour(&l.who, self.truecolor)).add_modifier(Modifier::BOLD)
        } else {
            Style::new().fg(t.muted)
        };
        let text_colour = match l.tone {
            Tone::Bad | Tone::Warn => self.tone(l.tone),
            Tone::Muted => t.muted,
            _ => t.fg,
        };
        let fresh = art::progress(l.arrived, now, EASE) < 1.0 && l.arrived > 0;
        let glyph_style =
            Style::new().fg(self.tone(l.tone)).add_modifier(if fresh { Modifier::BOLD } else { Modifier::empty() });
        let when = chrono::DateTime::from_timestamp_millis(l.at)
            .map(|d| d.with_timezone(&chrono::Local).format("%H:%M").to_string())
            .unwrap_or_default();
        Line::from(vec![
            Span::styled(format!("{when}  "), Style::new().fg(t.comment)),
            Span::styled(format!("{} ", l.glyph), glyph_style),
            Span::styled(fmt::pad(&l.who, who_w), who_style),
            Span::styled(fmt::fit_end(&l.text, w.saturating_sub(who_w + 9)), Style::new().fg(text_colour)),
        ])
    }

    /// The keys, lazygit-style; a passing message takes their place for a few seconds.
    fn hints(&self) -> Line<'static> {
        let t = &self.theme;
        if let Some((msg, _)) = self.flash.as_ref().filter(|(_, at)| at.elapsed() < Duration::from_secs(4)) {
            return Line::from(Span::styled(format!(" {msg} "), Style::new().fg(t.yellow)));
        }
        let mut keys: Vec<(&str, &str)> = if self.editing {
            vec![("⏎", "keep filter"), ("esc", "clear")]
        } else {
            vec![("tab", "panel"), ("↑↓", "move"), ("⏎", "details"), ("o", "open")]
        };
        if !self.editing {
            if !self.read_only {
                keys.extend([("x", "cancel"), ("R", "redeploy"), ("P", "promote"), ("B", "roll back")]);
            }
            keys.extend([("/", "filter"), ("?", "keys"), ("q", "quit")]);
        }
        let mut spans = vec![Span::raw(" ")];
        for (key, what) in keys {
            spans.push(Span::styled(key, Style::new().fg(t.blue).add_modifier(Modifier::BOLD)));
            spans.push(Span::styled(format!(" {what}  "), Style::new().fg(t.muted)));
        }
        Line::from(spans)
    }

    /// The selected row in Tokyo Night's selection, or reverse video without
    /// colour; a row that just changed eases in from the selection's colour.
    fn lift(&self, mut line: Line<'static>, selected: bool, changed: i64, now: i64, width: usize) -> Line<'static> {
        let t = &self.theme;
        let bg = if selected {
            Some(t.selection)
        } else {
            let p = art::progress(changed, now, EASE);
            (p < 1.0 && changed > 0).then(|| art::ease(t.selection, t.bg, p))
        };
        let Some(bg) = bg else { return line };
        let lift = |s: Style| {
            if self.no_color && selected {
                s.add_modifier(Modifier::REVERSED)
            } else if selected {
                s.bg(bg).add_modifier(Modifier::BOLD)
            } else {
                s.bg(bg)
            }
        };
        for span in &mut line.spans {
            span.style = lift(span.style);
        }
        let pad = width.saturating_sub(line.width());
        line.spans.push(Span::styled(" ".repeat(pad), lift(Style::new())));
        line
    }

    fn overlay_area(&self, f: &Frame, w: u16, h: u16) -> Rect {
        let w = w.min(f.area().width.saturating_sub(4));
        let h = h.min(f.area().height.saturating_sub(2));
        Rect::new((f.area().width - w) / 2, (f.area().height - h) / 2, w, h)
    }

    fn draw_help(&self, f: &mut Frame) {
        let t = &self.theme;
        let mut keys = vec![
            ("tab, shift-tab", "move between the panels; the one outlined in blue has the selection"),
            ("↑ ↓, j k", "move the selection; moving in the feed pauses it"),
            ("pgup pgdn, home end", "move further"),
            ("⏎", "details of a project, a deployment, billing or a line of the feed"),
            ("o", "open what's selected in the browser"),
        ];
        if !self.read_only {
            keys.extend([
                ("x", "cancel the selected build"),
                ("R", "redeploy the selected deployment"),
                ("P", "promote the selected deployment to production"),
                ("B", "roll production back to the selected deployment"),
            ]);
        }
        keys.extend([
            ("/", "filter projects, deployments and the feed by what they say"),
            ("1 to 7", "show or hide deploys, traffic, errors, Vercel, team, domains, spend in the feed"),
            ("0", "show every kind again"),
            ("p", "pause the feed, or carry on"),
            ("a", "show the quiet projects too, or fold them away"),
            ("z", "the panel with the selection on its own"),
            ("r", "ask Vercel for everything again now"),
            ("t", "watch your next team"),
            ("esc", "back, or clear the filter"),
            ("q", "quit"),
        ]);
        let mut lines = vec![Line::default()];
        for (key, what) in keys {
            lines.push(Line::from(vec![
                Span::styled(format!("{key:>20}  "), Style::new().fg(t.blue).add_modifier(Modifier::BOLD)),
                Span::styled(what, Style::new().fg(t.text)),
            ]));
        }
        lines.push(Line::default());
        let note = if self.read_only {
            "    ooze only watches: actions are off in the settings."
        } else {
            "    ooze changes nothing on Vercel unless you say so: x, R, P and B each ask first."
        };
        lines.push(Line::from(Span::styled(note, Style::new().fg(t.comment))));
        let area = self.overlay_area(f, 100, lines.len() as u16 + 2);
        f.render_widget(Clear, area);
        let block = self
            .panel(t.blue)
            .style(Style::new().bg(t.surface).fg(t.fg))
            .title(Span::styled(" keys ", Style::new().fg(t.blue).add_modifier(Modifier::BOLD)));
        f.render_widget(Paragraph::new(lines).block(block), area);
    }

    fn draw_details(&mut self, f: &mut Frame, o: &Overlay, now: i64) {
        let t = self.theme;
        let (title, lines) = match o {
            Overlay::Project(id) => self.project_details(id, now),
            Overlay::Deployment(uid) => self.deployment_details(uid, now),
            Overlay::Billing => self.billing_details(now),
            Overlay::Line(l) => self.line_details(l, now),
            Overlay::Help | Overlay::Confirm(..) => return,
        };
        let w = (f.area().width * 9 / 10).min(124);
        let h = f.area().height * 5 / 6;
        let area = self.overlay_area(f, w, h);
        f.render_widget(Clear, area);
        let mut bottom = vec![Span::raw(" ")];
        let mut key = |k: &str, what: &str| {
            bottom.push(Span::styled(k.to_string(), Style::new().fg(t.blue).add_modifier(Modifier::BOLD)));
            bottom.push(Span::styled(format!(" {what}  "), Style::new().fg(t.muted)));
        };
        key("↑↓", "scroll");
        key("o", "open");
        if matches!(o, Overlay::Deployment(_)) && !self.read_only {
            key("x R P B", "act");
        }
        key("esc", "back");
        let block = self
            .panel(t.blue)
            .style(Style::new().bg(t.surface).fg(t.fg))
            .title(Span::styled(
                format!(" {} ", fmt::fit_end(&title, w as usize - 6)),
                Style::new().fg(t.blue).add_modifier(Modifier::BOLD),
            ))
            .title_bottom(Line::from(bottom))
            .padding(Padding::horizontal(2));
        let inner_h = block.inner(area).height;
        let most = (lines.len() as u16).saturating_sub(inner_h);
        if matches!(o, Overlay::Deployment(_)) && (self.stick || self.scroll >= most) {
            self.stick = true;
            self.scroll = most;
        }
        self.scroll = self.scroll.min(most);
        f.render_widget(Paragraph::new(lines).block(block).scroll((self.scroll, 0)), area);
    }

    fn pair(&self, label: &str, value: impl Into<String>) -> Line<'static> {
        let t = &self.theme;
        Line::from(vec![
            Span::styled(fmt::pad(label, 13), Style::new().fg(t.comment)),
            Span::styled(value.into(), Style::new().fg(t.text)),
        ])
    }

    fn rule(&self, label: &str) -> Line<'static> {
        Line::from(Span::styled(format!("── {label} ──"), Style::new().fg(self.theme.comment)))
    }

    fn ranked(&self, rows: &[(String, u64)], unit: &str) -> Vec<Line<'static>> {
        let t = &self.theme;
        let most = rows.first().map(|r| r.1).unwrap_or(1).max(1);
        rows.iter()
            .map(|(k, n)| {
                let mut spans = vec![
                    Span::styled(format!("  {}", fmt::pad(k, 44)), Style::new().fg(t.fg)),
                    Span::styled(fmt::rpad(&fmt::num(*n), 8), Style::new().fg(t.text)),
                ];
                spans.push(Span::raw(format!(" {unit}  ")));
                spans.extend(art::meter(*n as f64 / most as f64, 16, t.teal, t));
                Line::from(spans)
            })
            .collect()
    }

    fn project_details(&self, id: &str, now: i64) -> (String, Vec<Line<'static>>) {
        let w = &self.world;
        let Some(p) = w.projects.iter().find(|p| p.id == id) else { return ("project".into(), vec![]) };
        let row = w.rows(now, true).0.into_iter().find(|r| r.id == id).unwrap_or_default();
        let mut lines = vec![Line::default()];
        let prod = match &row.production {
            Some(pr) => {
                let url = w
                    .find(&pr.uid)
                    .and_then(|d| d.url.clone())
                    .or(p.production().and_then(|c| c.url.clone()))
                    .map(|u| format!(" · https://{u}"))
                    .unwrap_or_default();
                format!(
                    "{}{url}",
                    self.production_spans(&row, now).iter().map(|s| s.content.to_string()).collect::<String>()
                )
            }
            None => "none yet".into(),
        };
        lines.push(self.pair("production", prod));
        lines.push(self.pair(
            "web analytics",
            if row.analytics { "on" } else { "off: its rows show requests instead of page views" },
        ));
        if row.analytics {
            lines.push(self.pair(
                "now",
                format!("{} online · {} in the last hour", row.online, fmt::count(row.hour, "page view", "page views")),
            ));
            if let Some((visitors, views)) = w.today.get(id) {
                lines.push(self.pair(
                    "today",
                    format!(
                        "{} · {}",
                        fmt::count(*visitors, "visitor", "visitors"),
                        fmt::count(*views, "page view", "page views")
                    ),
                ));
            }
        } else {
            lines.push(self.pair("now", format!("{} in the last hour", fmt::count(row.hour, "request", "requests"))));
        }
        let detail = w.details.get(id);
        if let Some(d) = detail {
            for (label, v) in [("7 days", d.week), ("30 days", d.month)] {
                if let Some((views, visitors)) = v {
                    lines.push(self.pair(
                        label,
                        format!(
                            "{} · {}",
                            fmt::count(visitors, "visitor", "visitors"),
                            fmt::count(views, "page view", "page views")
                        ),
                    ));
                }
            }
        }
        if let Some(e) = row.errors {
            lines.push(self.pair("errors", format!("5xx on {} of requests in the last 15 min", fmt::percent(e))));
        }
        if let Some(c) = row.cache {
            lines.push(self.pair("cache", format!("{} of cacheable requests were hits", fmt::percent(c))));
        }
        if row.blocked > 0 {
            lines.push(self.pair(
                "firewall",
                format!("{} challenged or denied in the last hour", fmt::count(row.blocked, "request", "requests")),
            ));
        }
        if row.attack {
            lines.push(self.pair("attack", "Vercel's firewall is mitigating an attack on it now"));
        }
        match detail {
            None => {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "Fetching where its visitors come from…",
                    Style::new().fg(self.theme.comment),
                )));
            }
            Some(d) => {
                for (label, rows, unit) in [
                    ("pages, last hour", &d.pages, "views"),
                    ("referrers, last hour", &d.referrers, "views"),
                    ("countries, last hour", &d.countries, "views"),
                    ("5xx by route, last hour", &d.errors, "errors"),
                ] {
                    if !rows.is_empty() {
                        lines.push(Line::default());
                        lines.push(self.rule(label));
                        lines.extend(self.ranked(rows, unit));
                    }
                }
            }
        }
        let mine: Vec<&Deployment> =
            w.deployments.iter().filter(|d| d.project_id.as_deref() == Some(id)).take(8).collect();
        if !mine.is_empty() {
            lines.push(Line::default());
            lines.push(self.rule("deployments"));
            for d in mine {
                lines.push(self.deployment_line(d, 19, 110, now));
            }
        }
        (p.name.clone(), lines)
    }

    fn deployment_details(&self, uid: &str, now: i64) -> (String, Vec<Line<'static>>) {
        let t = &self.theme;
        let w = &self.world;
        let Some(d) = w.find(uid) else { return ("deployment".into(), vec![]) };
        let mut lines = vec![Line::default()];
        let live = if w.is_current(d) { " · serving production" } else { "" };
        lines.push(self.pair("environment", format!("{}{live}", d.environment())));
        let state = match d.state() {
            s if d.running() => format!(
                "{} for {}",
                s.to_lowercase(),
                fmt::clock(fmt::between(d.building_at.unwrap_or(d.created), now))
            ),
            "READY" => format!("ready, built in {}", fmt::span(fmt::between(0, d.took(now)))),
            s => s.to_lowercase(),
        };
        lines.push(self.pair("state", state));
        if let Some(e) = &d.error_message {
            lines.push(Line::from(vec![
                Span::styled(fmt::pad("error", 13), Style::new().fg(t.comment)),
                Span::styled(fmt::one_line(e), Style::new().fg(t.red)),
            ]));
        }
        let git = [d.branch(), d.commit().map(|c| fmt::sha(&c))].into_iter().flatten().collect::<Vec<_>>().join(" ");
        if !git.is_empty() {
            lines.push(self.pair("commit", git));
        }
        if let Some(m) = d.message() {
            lines.push(self.pair("", fmt::one_line(&m)));
        }
        if let Some(a) = d.author() {
            lines.push(self.pair("by", a));
        }
        lines.push(
            self.pair("created", format!("{} · {}", fmt::when(d.created, now), fmt::ago(fmt::between(d.created, now)))),
        );
        if let Some(u) = &d.url {
            lines.push(self.pair("url", format!("https://{u}")));
        }
        lines.push(self.pair("id", d.uid.clone()));
        lines.push(Line::default());
        lines.push(self.rule("actions"));
        if self.read_only {
            lines.push(Line::from(Span::styled(
                "  Actions are off: read_only is set in the settings.",
                Style::new().fg(t.comment),
            )));
        } else {
            for a in Action::ALL {
                let (style, why) = match actions::check(a, d, w) {
                    Ok(()) => (Style::new().fg(t.fg), String::new()),
                    Err(why) => (Style::new().fg(t.comment), format!(": {why}")),
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("  {}  ", a.key()), Style::new().fg(t.blue).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("{}{why}", a.verb()), style),
                ]));
            }
        }
        lines.push(Line::default());
        lines.push(self.rule("build log"));
        match w.logs.get(uid) {
            None => lines.push(Line::from(Span::styled("  Fetching the log…", Style::new().fg(t.comment)))),
            Some((log, done)) => {
                for l in log {
                    let when = l.at.map(fmt::time).unwrap_or_default();
                    lines.push(Line::from(vec![
                        Span::styled(format!("{when:>8}  "), Style::new().fg(t.comment)),
                        Span::styled(
                            l.text.trim_end().to_string(),
                            Style::new().fg(if l.error { t.red } else { t.text }),
                        ),
                    ]));
                }
                if !done {
                    lines.push(Line::from(Span::styled(
                        format!("  {} following…", art::spinner(now, uid)),
                        Style::new().fg(t.blue),
                    )));
                } else if log.is_empty() {
                    lines.push(Line::from(Span::styled("  The log is empty.", Style::new().fg(t.comment))));
                }
            }
        }
        (format!("{} · {} · {}", d.name, d.environment(), actions::short(&d.uid)), lines)
    }

    fn billing_details(&self, now: i64) -> (String, Vec<Line<'static>>) {
        let t = &self.theme;
        let w = &self.world;
        let team = w.team.clone().unwrap_or_default();
        let mut lines = vec![Line::default()];
        let fixed = billing::fixed(&team);
        if let Some(plan) = team.plan() {
            let costs = fixed.iter().map(|(n, c)| format!("{n} {}", fmt::dollars(*c))).collect::<Vec<_>>().join(" · ");
            lines
                .push(self.pair("plan", if costs.is_empty() { capitalised(plan) } else { format!("{costs} a month") }));
        }
        let Some(spend) = &w.spend else {
            lines.push(self.billing_lines(100, now).into_iter().next().unwrap_or_default());
            return ("billing".into(), lines);
        };
        if let Some((day, total)) = self.day_of(now) {
            lines.push(self.pair(
                "period",
                format!("{} to {} · day {day} of {total}", fmt::day(spend.period.start), fmt::day(spend.period.end)),
            ));
        }
        let usage = spend.usage();
        match billing::credit(&team) {
            Some(c) if c > 0.0 => lines.push(self.pair(
                "usage",
                format!(
                    "{} of the {} included credit ({})",
                    fmt::money(usage),
                    fmt::dollars(c),
                    fmt::percent(usage / c)
                ),
            )),
            _ => lines.push(self.pair("usage", fmt::money(usage))),
        }
        if let Some(p) = spend.projected() {
            lines.push(
                self.pair(
                    "projected",
                    format!("{} by {}, at the rate so far", fmt::money(p), fmt::day(spend.period.end)),
                ),
            );
        }
        lines.push(self.pair("billed", format!("{} so far", fmt::money(spend.billed()))));
        if spend.other().abs() >= 0.005 {
            lines.push(self.pair("other", format!("{} in purchases, adjustments and tax", fmt::money(spend.other()))));
        }
        let through = spend.latest().map(fmt::day).unwrap_or_else(|| "none yet".into());
        lines.push(self.pair(
            "charges",
            format!("to {through}, a day at a time · read {}", fmt::ago(fmt::between(spend.fetched, now))),
        ));
        let daily = spend.daily();
        if !daily.is_empty() {
            lines.push(Line::default());
            lines.push(self.rule("a day"));
            let cents: Vec<u64> = daily.iter().map(|d| (d * 100.0).max(0.0) as u64).collect();
            let most = daily.iter().cloned().fold(0.0, f64::max);
            lines.push(Line::from(vec![
                Span::styled(format!("  {}", art::sparkline(&cents)), Style::new().fg(t.teal)),
                Span::styled(format!("  up to {} a day", fmt::money(most)), Style::new().fg(t.comment)),
            ]));
        }
        let table = |rows: Vec<(String, f64)>| -> Vec<Line<'static>> {
            let most = rows.first().map(|r| r.1).unwrap_or(1.0).max(0.01);
            rows.into_iter()
                .take(30)
                .map(|(k, c)| {
                    let mut spans = vec![
                        Span::styled(format!("  {}", fmt::pad(&k, 40)), Style::new().fg(t.fg)),
                        Span::styled(fmt::rpad(&fmt::money(c), 10), Style::new().fg(t.text)),
                        Span::raw("  "),
                    ];
                    spans.extend(art::meter(c / most, 16, t.teal, t));
                    Line::from(spans)
                })
                .collect()
        };
        for (label, rows) in
            [("by service", spend.services()), ("by project", spend.projects()), ("marketplace", spend.providers())]
        {
            if !rows.is_empty() {
                lines.push(Line::default());
                lines.push(self.rule(label));
                lines.extend(table(rows));
            }
        }
        if w.ai.is_some() || !w.budgets.is_empty() {
            lines.push(Line::default());
            lines.push(self.rule("AI Gateway"));
            if let Some(ai) = &w.ai {
                lines.push(self.pair(
                    "today",
                    format!("{} · {}", fmt::money(ai.cost_today), fmt::count(ai.requests_today, "request", "requests")),
                ));
            }
            if spend.ai_gateway() > 0.0 {
                lines.push(self.pair("this period", fmt::money(spend.ai_gateway())));
            }
            for b in &w.budgets {
                let name = b.name.clone().unwrap_or_else(|| b.scope_type.clone());
                let period =
                    b.refresh_period.clone().filter(|p| p != "none").map(|p| format!(", {p}")).unwrap_or_default();
                lines.push(self.pair(
                    "budget",
                    format!("{name}: {} of {}{period}", fmt::money(b.current_spend), fmt::money(b.limit_amount)),
                ));
            }
        }
        ("billing".into(), lines)
    }

    fn line_details(&self, l: &FeedLine, now: i64) -> (String, Vec<Line<'static>>) {
        let mut lines = vec![Line::default()];
        lines.push(self.pair("when", format!("{} · {}", fmt::when(l.at, now), fmt::ago(fmt::between(l.at, now)))));
        lines.push(self.pair("kind", l.kind.name()));
        lines.push(self.pair("about", l.who.clone()));
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(l.text.clone(), Style::new().fg(self.theme.fg))));
        lines.push(Line::default());
        for (k, v) in &l.detail {
            for (i, part) in v.lines().take(60).enumerate() {
                lines.push(self.pair(if i == 0 { k } else { "" }, part.to_string()));
            }
        }
        (format!("{} {}", l.glyph, l.who), lines)
    }

    fn draw_confirm(&self, f: &mut Frame, action: Action, uid: &str, now: i64) {
        let t = &self.theme;
        let Some(d) = self.world.find(uid) else { return };
        let (title, said) = actions::describe(action, d, now);
        let mut lines = vec![Line::default()];
        for s in said {
            lines.push(Line::from(Span::styled(s, Style::new().fg(t.text))));
        }
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled("y", Style::new().fg(t.orange).add_modifier(Modifier::BOLD)),
            Span::styled(format!(" {}", action.verb()), Style::new().fg(t.fg)),
            Span::styled("   any other key leaves it as it is", Style::new().fg(t.comment)),
        ]));
        let width = lines.iter().map(|l| l.width()).max().unwrap_or(40).max(title.chars().count() + 4) as u16 + 6;
        let area = self.overlay_area(f, width, lines.len() as u16 + 2);
        f.render_widget(Clear, area);
        let border =
            if d.production() || matches!(action, Action::Promote | Action::Rollback) { t.orange } else { t.blue };
        let block = self
            .panel(border)
            .style(Style::new().bg(t.surface).fg(t.fg))
            .title(Span::styled(format!(" {title} "), Style::new().fg(border).add_modifier(Modifier::BOLD)))
            .padding(Padding::horizontal(2));
        f.render_widget(Paragraph::new(lines).block(block), area);
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.editing {
            match k.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.editing = false;
                }
                KeyCode::Enter => self.editing = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) => self.filter.push(c),
                _ => {}
            }
            return;
        }
        if let Some(Overlay::Confirm(action, uid)) = self.overlay.clone() {
            self.overlay = None;
            if matches!(k.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
                self.confirm(action, &uid);
            } else {
                self.say("left as it is");
            }
            return;
        }
        if let Some(o) = self.overlay.clone() {
            match k.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') => self.close(),
                KeyCode::Up | KeyCode::Char('k') => self.scroll_by(-1),
                KeyCode::Down | KeyCode::Char('j') => self.scroll_by(1),
                KeyCode::PageUp => self.scroll_by(-10),
                KeyCode::PageDown => self.scroll_by(10),
                KeyCode::Home => self.scroll_by(-100_000),
                KeyCode::End => self.scroll_by(100_000),
                KeyCode::Char('o') => self.browse(),
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Char(c) if matches!(o, Overlay::Deployment(_)) => {
                    if let Some(a) = Action::of_key(c) {
                        self.act(a);
                    }
                }
                _ => {}
            }
            return;
        }
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('?') => {
                self.overlay = Some(Overlay::Help);
                self.scroll = 0;
            }
            KeyCode::Tab => self.cycle(1),
            KeyCode::BackTab => self.cycle(-1),
            KeyCode::Up | KeyCode::Char('k') => self.step(-1),
            KeyCode::Down | KeyCode::Char('j') => self.step(1),
            KeyCode::PageUp => self.step(-10),
            KeyCode::PageDown => self.step(10),
            KeyCode::Home => self.step(-100_000),
            KeyCode::End => self.step(100_000),
            KeyCode::Enter => self.open(),
            KeyCode::Esc => {
                if self.zoom {
                    self.zoom = false;
                } else if !self.filter.is_empty() {
                    self.filter.clear();
                } else if self.paused.is_some() {
                    self.paused = None;
                    self.line = 0;
                }
            }
            KeyCode::Char('/') => self.editing = true,
            KeyCode::Char('p') => match self.paused {
                Some(_) => {
                    self.paused = None;
                    self.line = 0;
                    self.say("the feed carries on");
                }
                None => {
                    self.paused = Some(self.world.fed);
                    self.say("paused: new lines wait until p");
                }
            },
            KeyCode::Char('a') => {
                self.all = !self.all;
                self.say(if self.all { "showing every project" } else { "the quiet projects are folded away" });
            }
            KeyCode::Char('z') => self.zoom = !self.zoom,
            KeyCode::Char('o') => self.browse(),
            KeyCode::Char('r') => self.refresh(),
            KeyCode::Char('t') => self.next_team(),
            KeyCode::Char('0') => {
                self.hidden.clear();
                self.say("showing every kind");
            }
            KeyCode::Char(c @ '1'..='7') => {
                let kind = Kind::ALL[c as usize - '1' as usize];
                if !self.hidden.remove(&kind) {
                    self.hidden.insert(kind);
                }
                let now = if self.hidden.contains(&kind) { "hidden" } else { "shown" };
                self.say(format!("{} {now} in the feed", kind.name()));
            }
            KeyCode::Char(c) => {
                if let Some(a) = Action::of_key(c) {
                    self.act(a);
                }
            }
            _ => {}
        }
    }

    fn cycle(&mut self, by: i64) {
        let order: Vec<Panel> = ORDER.into_iter().filter(|p| self.wide || *p != Panel::Billing).collect();
        let i = order.iter().position(|p| *p == self.focus).unwrap_or(0) as i64;
        self.focus = order[(i + by).rem_euclid(order.len() as i64) as usize];
    }

    fn scroll_by(&mut self, by: i64) {
        self.scroll = (self.scroll as i64 + by).clamp(0, u16::MAX as i64) as u16;
        // Scrolling up stops following a log; reaching its end follows it again.
        if by < 0 {
            self.stick = false;
        } else if by >= 100_000 {
            self.stick = true;
        }
    }

    fn close(&mut self) {
        if let (Some(Overlay::Deployment(_)), Some(hub)) = (&self.overlay, &self.hub) {
            hub.unfollow();
        }
        self.overlay = None;
    }

    fn step(&mut self, by: i64) {
        let now = self.now();
        let move_in = |list: &[String], at: Option<&String>| -> Option<String> {
            if list.is_empty() {
                return None;
            }
            let i = at.and_then(|a| list.iter().position(|x| x == a)).unwrap_or(0) as i64;
            Some(list[(i + by).clamp(0, list.len() as i64 - 1) as usize].clone())
        };
        match self.focus {
            Panel::Projects => {
                let ids: Vec<String> = self.shown_rows(now).0.into_iter().map(|r| r.id).collect();
                self.project = move_in(&ids, self.project.as_ref());
            }
            Panel::Deployments => {
                let ids: Vec<String> = self.shown_deployments().into_iter().map(|d| d.uid.clone()).collect();
                self.deployment = move_in(&ids, self.deployment.as_ref());
            }
            Panel::Feed => {
                // Moving through the feed holds it still, so lines don't slide away from the selection.
                if self.paused.is_none() {
                    self.paused = Some(self.world.fed);
                }
                let n = self.shown().len() as i64;
                self.line = (self.line as i64 + by).clamp(0, (n - 1).max(0)) as usize;
            }
            Panel::Billing => {}
        }
    }

    fn open(&mut self) {
        self.scroll = 0;
        self.stick = true;
        self.overlay = match self.focus {
            Panel::Projects => self.project.clone().map(|id| {
                let stale = self.world.details.get(&id).is_none_or(|d| self.now() - d.at > 60_000);
                if let (Some(hub), true) = (&self.hub, stale) {
                    hub.detail(&id);
                }
                Overlay::Project(id)
            }),
            Panel::Deployments => self.deployment.clone().map(|uid| {
                self.follow(&uid);
                Overlay::Deployment(uid)
            }),
            Panel::Billing => Some(Overlay::Billing),
            Panel::Feed => match self.shown().get(self.line).map(|l| (*l).clone()) {
                Some(l) if l.deployment.as_ref().is_some_and(|d| self.world.find(d).is_some()) => {
                    let uid = l.deployment.clone().unwrap_or_default();
                    self.follow(&uid);
                    Some(Overlay::Deployment(uid))
                }
                Some(l) => Some(Overlay::Line(Box::new(l))),
                None => None,
            },
        };
    }

    fn follow(&mut self, uid: &str) {
        let running = self.world.find(uid).is_some_and(|d| d.running());
        if let Some(hub) = &self.hub {
            self.world.logs.remove(uid);
            hub.follow(uid, running);
        } else if !self.world.logs.contains_key(uid) {
            let lines = crate::demo::log(self.world.find(uid));
            self.world.apply(Msg::Log { deployment: uid.to_string(), lines, done: !running }, self.now());
        }
    }

    /// The deployment an action would be for: the one open, else the one selected.
    fn target(&self) -> Option<String> {
        match &self.overlay {
            Some(Overlay::Deployment(uid)) => Some(uid.clone()),
            _ if self.focus == Panel::Deployments => self.deployment.clone(),
            _ => None,
        }
    }

    fn act(&mut self, action: Action) {
        if self.read_only {
            self.say("actions are off: read_only is set in the settings");
            return;
        }
        let Some(uid) = self.target() else {
            self.say(format!("{}: select a deployment first (tab to deployments)", action.verb()));
            return;
        };
        let Some(d) = self.world.find(&uid) else { return };
        match actions::check(action, d, &self.world) {
            Err(why) => self.say(format!("can't {}: {why}", action.verb())),
            Ok(()) => self.overlay = Some(Overlay::Confirm(action, uid)),
        }
    }

    fn confirm(&mut self, action: Action, uid: &str) {
        let Some(d) = self.world.find(uid).cloned() else { return };
        let project = self.world.project_of(&d).map(|p| p.id.clone()).unwrap_or_default();
        match &self.hub {
            Some(hub) => {
                hub.act(action, d, project);
                self.say(format!("asked Vercel to {}", action.verb()));
            }
            None => {
                let said =
                    Ok(format!("would {} {} (in the demo, nothing is sent)", action.verb(), actions::short(uid)));
                let now = self.now();
                self.world.apply(Msg::Acted { project, deployment: uid.to_string(), said }, now);
            }
        }
    }

    /// What `o` opens for what's selected.
    fn url(&self) -> Option<String> {
        let team = self.world.team.as_ref().map(|t| t.slug.clone()).unwrap_or_default();
        let deployment = |uid: &str| {
            self.world.find(uid).and_then(|d| d.inspector_url.clone().or(d.url.clone().map(|u| format!("https://{u}"))))
        };
        match &self.overlay {
            Some(Overlay::Deployment(uid)) => return deployment(uid),
            Some(Overlay::Project(id)) => {
                return self
                    .world
                    .projects
                    .iter()
                    .find(|p| &p.id == id)
                    .map(|p| format!("https://vercel.com/{team}/{}", p.name));
            }
            Some(Overlay::Billing) => return Some(format!("https://vercel.com/{team}/~/usage")),
            Some(Overlay::Line(l)) => {
                return l.detail.iter().find(|(k, _)| k == "url" || k == "link").map(|(_, v)| v.clone());
            }
            _ => {}
        }
        match self.focus {
            Panel::Projects => self
                .project
                .as_ref()
                .and_then(|id| self.world.projects.iter().find(|p| &p.id == id))
                .map(|p| format!("https://vercel.com/{team}/{}", p.name)),
            Panel::Deployments => self.deployment.as_deref().and_then(deployment),
            Panel::Billing => Some(format!("https://vercel.com/{team}/~/usage")),
            Panel::Feed => self
                .shown()
                .get(self.line)
                .and_then(|l| l.detail.iter().find(|(k, _)| k == "url" || k == "link").map(|(_, v)| v.clone())),
        }
    }

    fn browse(&mut self) {
        let Some(url) = self.url() else {
            self.say("nothing to open there");
            return;
        };
        if self.hub.is_none() {
            self.say(format!("in the demo, o would open {url}"));
            return;
        }
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        match std::process::Command::new(opener)
            .arg(&url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(_) => self.say(format!("opened {url}")),
            Err(e) => self.say(format!("couldn't open {url}: {e}")),
        }
    }

    fn refresh(&mut self) {
        match &self.hub {
            Some(hub) => {
                for s in [
                    Source::Traffic,
                    Source::Health,
                    Source::Deployments,
                    Source::Projects,
                    Source::Today,
                    Source::Status,
                    Source::Events,
                    Source::Alerts,
                ] {
                    hub.poke(s);
                }
                self.say("asking Vercel again");
            }
            None => self.say("the demo makes its own numbers up"),
        }
    }

    fn next_team(&mut self) {
        let Some(hub) = self.hub.clone() else {
            self.say("the demo has one team");
            return;
        };
        let teams = self.world.teams.clone();
        if teams.len() < 2 {
            self.say("this account has one team");
            return;
        }
        let current = hub.team();
        let i = teams.iter().position(|t| Some(&t.id) == current.as_ref()).map_or(0, |i| i + 1) % teams.len();
        hub.switch(&teams[i].id);
        self.generation = hub.generation();
        let now = self.now();
        self.world.switch(now);
        (self.project, self.deployment, self.line) = (None, None, 0);
        self.say(format!("watching {}", teams[i].title()));
    }

    pub fn on_mouse(&mut self, m: MouseEvent) {
        let at = |r: &Rect| m.column >= r.x && m.column < r.x + r.width && m.row >= r.y && m.row < r.y + r.height;
        let Some((panel, _)) = self.areas.iter().find(|(_, r)| at(r)).copied() else { return };
        match m.kind {
            MouseEventKind::ScrollDown | MouseEventKind::ScrollUp if self.overlay.is_some() => {
                self.scroll_by(if m.kind == MouseEventKind::ScrollDown { 3 } else { -3 });
            }
            MouseEventKind::ScrollDown => {
                self.focus = panel;
                self.step(1);
            }
            MouseEventKind::ScrollUp => {
                self.focus = panel;
                self.step(-1);
            }
            MouseEventKind::Down(MouseButton::Left) if self.overlay.is_none() => self.focus = panel,
            _ => {}
        }
    }
}

/// The columns of the projects table, for the width there is.
struct Columns {
    name: usize,
    online: usize,
    spark: usize,
    hour: usize,
    today: usize,
    errors: usize,
    cache: usize,
}

impl Columns {
    fn for_width(w: usize) -> Columns {
        Columns {
            name: if w >= 100 { 18 } else { 14 },
            online: 7,
            spark: if w >= 140 {
                SHOWN
            } else if w >= 118 {
                20
            } else if w >= 98 {
                12
            } else {
                0
            },
            hour: 10,
            today: if w >= 80 { 8 } else { 0 },
            errors: if w >= 90 { 7 } else { 0 },
            cache: if w >= 104 { 7 } else { 0 },
        }
    }
}

fn ratio_colour(ratio: f64, t: &Theme) -> Color {
    if ratio >= 1.0 {
        t.red
    } else if ratio >= 0.75 {
        t.yellow
    } else {
        t.green
    }
}

/// A service's name, short enough to list three in a line.
fn short_service(s: &str) -> String {
    match s {
        "Fast Data Transfer" => "data transfer".into(),
        "Fast Origin Transfer" => "origin transfer".into(),
        "Fluid Active CPU" => "fluid CPU".into(),
        "Fluid Provisioned Memory" => "fluid memory".into(),
        "Build CPU Minutes" => "build minutes".into(),
        "Edge Requests" => "edge requests".into(),
        "Function Invocations" => "invocations".into(),
        "Web Analytics Events" => "analytics events".into(),
        "Observability Events" => "observability".into(),
        other => other.to_lowercase(),
    }
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

/// A line cut to `w` columns, span by span.
fn clip(line: Line<'static>, w: usize) -> Line<'static> {
    let mut used = 0;
    let mut spans = vec![];
    for s in line.spans {
        let n = s.content.chars().count();
        if used + n <= w {
            used += n;
            spans.push(s);
        } else {
            let room = w - used;
            if room > 0 {
                spans.push(Span::styled(fmt::fit_end(&s.content, room), s.style));
            }
            break;
        }
    }
    Line::from(spans)
}

/// `left` and `right` on one line `width` wide, with `right` against the end.
fn fill(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(|s| s.width()).sum();
    let mut spans = left;
    if used <= width {
        spans.push(Span::raw(" ".repeat(width - used)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// Runs the view on Vercel until q.
pub fn run(hub: Hub, rx: Receiver<Envelope>, cfg: &Config, notifier: Notifier) -> Result<()> {
    let view = View::new(Some(hub.clone()), cfg, Some(notifier));
    let result = drive(view, rx);
    hub.stop();
    result
}

/// Runs the view on a made-up team.
pub fn run_demo(cfg: &Config) -> Result<()> {
    drive(View::new(None, cfg, None), crate::demo::spawn())
}

fn drive(mut view: View, rx: Receiver<Envelope>) -> Result<()> {
    let mut terminal = ratatui::init();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::EnableMouseCapture);
    let result = (|| -> Result<()> {
        let mut last = Instant::now() - Duration::from_secs(1);
        loop {
            let mut changed = false;
            while let Ok(env) = rx.try_recv() {
                view.take(env);
                changed = true;
            }
            let every = if view.moving(view.now()) { Duration::from_millis(100) } else { Duration::from_secs(1) };
            if changed || last.elapsed() >= every {
                terminal.draw(|f| view.draw(f))?;
                last = Instant::now();
            }
            if event::poll(Duration::from_millis(60))? {
                match event::read()? {
                    Input::Key(k) if k.kind == KeyEventKind::Press => {
                        view.on_key(k);
                        terminal.draw(|f| view.draw(f))?;
                    }
                    Input::Mouse(m) => {
                        view.on_mouse(m);
                        terminal.draw(|f| view.draw(f))?;
                    }
                    Input::Resize(..) => {
                        terminal.draw(|f| view.draw(f))?;
                    }
                    _ => {}
                }
            }
            if view.quit {
                break;
            }
        }
        Ok(())
    })();
    let _ = ratatui::crossterm::execute!(std::io::stdout(), ratatui::crossterm::event::DisableMouseCapture);
    ratatui::restore();
    result
}

fn frame(view: &mut View, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).expect("a test backend");
    term.draw(|f| view.draw(f)).expect("a frame");
    term.backend().buffer().clone()
}

/// One frame as text, for checking the layout without a terminal.
pub fn snapshot(view: &mut View, width: u16, height: u16) -> String {
    let buf = frame(view, width, height);
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut line = String::new();
        for x in 0..buf.area.width {
            line.push_str(buf[(x, y)].symbol());
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

/// One frame in 24-bit colour, as escape codes, for pictures of it.
pub fn snapshot_ansi(view: &mut View, width: u16, height: u16) -> String {
    let buf = frame(view, width, height);
    let code = |c: Color, fg: bool| match c {
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", if fg { 38 } else { 48 }),
        Color::Indexed(i) => format!("{};5;{i}", if fg { 38 } else { 48 }),
        _ => (if fg { "39" } else { "49" }).to_string(),
    };
    let mut out = String::new();
    for y in 0..buf.area.height {
        let mut last: Option<(Color, Color, Modifier)> = None;
        for x in 0..buf.area.width {
            let cell = &buf[(x, y)];
            let now = (cell.fg, cell.bg, cell.modifier);
            if last != Some(now) {
                let bold = if cell.modifier.contains(Modifier::BOLD) { ";1" } else { ";22" };
                out.push_str(&format!("\x1b[0;{};{}{bold}m", code(cell.fg, true), code(cell.bg, false)));
                last = Some(now);
            }
            out.push_str(cell.symbol());
        }
        out.push_str("\x1b[0m\n");
    }
    out
}

impl View {
    /// Presses keys, for a frame of something other than the main screen: a
    /// tab is Tab, a newline Enter, and ^ Esc.
    pub fn press(&mut self, keys: &str) {
        for c in keys.chars() {
            let code = match c {
                '\t' => KeyCode::Tab,
                '\n' => KeyCode::Enter,
                '^' => KeyCode::Esc,
                other => KeyCode::Char(other),
            };
            self.on_key(KeyEvent::new(code, KeyModifiers::NONE));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::tests::NOW;

    fn view() -> View {
        let mut v = View::new(None, &Config::default(), None);
        v.world = crate::demo::world(NOW);
        v.clock = Some(NOW);
        v.theme = Theme::with(true);
        v.truecolor = true;
        v.no_color = false;
        v
    }

    #[test]
    fn the_main_screen_shows_every_panel_at_a_glance() {
        let mut v = view();
        let screen = snapshot(&mut v, 160, 48);
        for seen in [
            "ooze",
            "night-shift · Pro",
            "vercel ●",
            "projects · ",
            "with visitors",
            "deployments · 1 building",
            "billing · day",
            "feed",
            "tab panel",
        ] {
            assert!(screen.contains(seen), "{seen}:\n{screen}");
        }
        assert!(screen.contains("moth-radio") && screen.contains("last 30 min"), "{screen}");
        assert!(screen.contains("quiet projects · a shows them"), "{screen}");
        assert!(screen.contains("failed") && screen.contains("exited with 1"), "the failed build says why:\n{screen}");
        assert!(screen.contains("of $20 credit") && screen.contains("% ") && screen.contains(" over"), "{screen}");
        assert!(screen.contains("live"), "the deployment serving production:\n{screen}");
        assert!(screen.contains('▀') || screen.contains('▄'), "the Slime sits in billing:\n{screen}");
        assert!(screen.lines().all(|l| l.chars().count() <= 160));
    }

    #[test]
    fn a_narrow_window_drops_billing_and_columns_but_keeps_the_rest() {
        let mut v = view();
        let screen = snapshot(&mut v, 100, 32);
        assert!(!screen.contains("billing · day"), "{screen}");
        assert!(screen.contains("spent $"), "spend stays in the header:\n{screen}");
        assert!(screen.contains("deployments") && screen.contains("feed"), "{screen}");
        assert!(screen.lines().all(|l| l.chars().count() <= 100));
        let tiny = snapshot(&mut v, 80, 24);
        assert!(tiny.contains("moth-radio"), "{tiny}");
    }

    #[test]
    fn the_selected_panel_is_outlined_in_blue_and_tab_moves_it() {
        let mut v = view();
        let projects = |v: &mut View| {
            let buf = frame(v, 160, 48);
            let at = v.areas.iter().find(|(p, _)| *p == Panel::Projects).unwrap().1;
            buf[(at.x, at.y)].fg
        };
        assert_eq!(projects(&mut v), v.theme.blue);
        v.press("\t");
        assert_eq!(v.focus, Panel::Deployments);
        assert_eq!(projects(&mut v), v.theme.gutter);
    }

    #[test]
    fn details_open_on_enter_and_close_on_esc() {
        let mut v = view();
        snapshot(&mut v, 160, 48);
        v.press("\n");
        let screen = snapshot(&mut v, 160, 48);
        assert!(
            screen.contains("web analytics") && screen.contains("Fetching where its visitors come from"),
            "{screen}"
        );
        v.press("^\t\n");
        let screen = snapshot(&mut v, 160, 48);
        assert!(screen.contains("── actions ──") && screen.contains("── build log ──"), "{screen}");
        assert!(screen.contains("Installing dependencies"), "the demo's log:\n{screen}");
        v.press("^\t\n");
        let screen = snapshot(&mut v, 160, 48);
        assert!(
            screen.contains("── by service ──") && screen.contains("included credit"),
            "billing's details:\n{screen}"
        );
    }

    #[test]
    fn an_action_asks_first_and_only_does_it_on_y() {
        let mut v = view();
        snapshot(&mut v, 160, 48);
        v.press("\t");
        let building = v.world.deploy_rows()[0].uid.clone();
        assert_eq!(v.deployment.as_deref(), Some(building.as_str()), "the running build comes first");
        v.press("x");
        let screen = snapshot(&mut v, 160, 48);
        assert!(
            screen.contains("Cancel moth-radio's production build?") && screen.contains("any other key leaves it"),
            "{screen}"
        );
        v.press("n");
        assert!(v.overlay.is_none());
        assert!(!v.world.feed.iter().any(|l| l.text.contains("would cancel")), "n leaves it");
        v.press("xy");
        assert!(v.world.feed.back().unwrap().text.starts_with("would cancel"), "{:?}", v.world.feed.back());
        v.press("B");
        assert!(v.flash.as_ref().unwrap().0.starts_with("can't roll back"), "a running build can't be rolled back to");
    }

    #[test]
    fn read_only_turns_the_actions_off() {
        let mut v = view();
        v.read_only = true;
        let screen = snapshot(&mut v, 160, 48);
        assert!(!screen.contains("R redeploy"), "{screen}");
        v.press("\tx");
        assert!(v.overlay.is_none());
        assert!(v.flash.as_ref().unwrap().0.contains("read_only"));
    }

    #[test]
    fn the_filter_keeps_what_mentions_it() {
        let mut v = view();
        v.press("/cave\n");
        let screen = snapshot(&mut v, 160, 48);
        assert!(screen.contains("cave-notes") && !screen.contains("night-market "), "{screen}");
        assert!(screen.contains("/ cave"), "the filter shows in the border:\n{screen}");
    }

    #[test]
    fn kinds_hide_from_the_feed() {
        let mut v = view();
        v.press("1");
        let screen = snapshot(&mut v, 160, 48);
        assert!(screen.contains("· traffic, errors, Vercel, team, domains, spend"), "{screen}");
        assert!(v.shown().iter().all(|l| l.kind != Kind::Deploys));
    }

    #[test]
    fn without_colour_the_selection_is_reversed() {
        let mut v = view();
        v.no_color = true;
        let buf = frame(&mut v, 160, 48);
        assert!(buf.content.iter().all(|c| c.fg == Color::Reset && c.bg == Color::Reset));
        assert!(buf.content.iter().any(|c| c.modifier.contains(Modifier::REVERSED)), "the selected row still shows");
    }

    #[test]
    fn a_refused_token_says_how_to_sign_in_and_the_slime_sleeps() {
        let mut v = View::new(None, &Config::default(), None);
        v.clock = Some(NOW);
        v.world.apply(Msg::Failed(Source::Who, crate::api::ApiError::Unauthorized("Not authorized".into())), NOW);
        let screen = snapshot(&mut v, 120, 40);
        assert!(
            screen.contains("Vercel refused the token: Not authorized.") && screen.contains("`ooze login` signs in"),
            "{screen}"
        );
        assert!(screen.contains('▀') || screen.contains('▄'), "the Slime, asleep:\n{screen}");
    }

    #[test]
    fn a_switch_drops_what_was_fetched_for_the_team_before() {
        let mut v = view();
        v.generation = 3;
        let fed = v.world.fed;
        v.take(Envelope {
            generation: 2,
            msg: Msg::Events(vec![crate::vercel::Event {
                id: "x".into(),
                text: "old team".into(),
                kind: "k".into(),
                created_at: NOW,
                user: None,
            }]),
        });
        assert_eq!(v.world.fed, fed, "from before the switch, it's dropped");
    }
}
