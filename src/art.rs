//! What moves: the Slime in half-block pixel art, spinners, sparklines,
//! meters, and colours easing from one to another.

use crate::theme::{Theme, color, nearest_256};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// badcoffee.dev's Slime, one pixel per cell, from its creature.png in
/// badcoffee.dev's print folder by scripts/creature.py.
const SLIME: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/creatures/slime.png"));

type Rgb = [u8; 3];

#[derive(Clone, Debug, PartialEq)]
pub struct Sprite {
    pub w: usize,
    pub h: usize,
    px: Vec<Option<Rgb>>,
}

/// How the creature looks this frame.
#[derive(Clone, Copy, Debug, Default)]
pub struct Look {
    /// 0 to 1: how much it lights up when something happens.
    pub glow: f32,
    pub blink: bool,
    /// Vercel isn't answering, so it's grey.
    pub asleep: bool,
    /// Drawn a pixel lower, for it to bob.
    pub low: bool,
}

impl Sprite {
    pub fn slime() -> Sprite {
        Sprite::decode(SLIME).unwrap_or(Sprite { w: 0, h: 0, px: vec![] })
    }

    pub fn decode(bytes: &[u8]) -> Option<Sprite> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::normalize_to_color8());
        let mut reader = decoder.read_info().ok()?;
        let mut buf = vec![0; reader.output_buffer_size()?];
        let info = reader.next_frame(&mut buf).ok()?;
        let (w, h) = (info.width as usize, info.height as usize);
        let channels = info.color_type.samples();
        let px = buf[..info.buffer_size()]
            .chunks_exact(channels)
            .map(|p| match channels {
                4 => (p[3] >= 128).then_some([p[0], p[1], p[2]]),
                3 => Some([p[0], p[1], p[2]]),
                2 => (p[1] >= 128).then_some([p[0], p[0], p[0]]),
                _ => Some([p[0], p[0], p[0]]),
            })
            .collect();
        Some(Sprite { w, h, px })
    }

    fn at(&self, x: usize, y: usize) -> Option<Rgb> {
        if x < self.w && y < self.h { self.px[y * self.w + x] } else { None }
    }

    /// Half as wide and high, each pixel the first one seen of its square.
    pub fn halved(&self) -> Sprite {
        let (w, h) = (self.w.div_ceil(2), self.h.div_ceil(2));
        let mut px = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                let square = [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(dx, dy)| self.at(x * 2 + dx, y * 2 + dy));
                let solid = square.iter().filter(|p| p.is_some()).count();
                px.push(if solid >= 2 { square.into_iter().flatten().next() } else { None });
            }
        }
        Sprite { w, h, px }
    }

    /// Terminal rows it takes, with the pixel it bobs by.
    pub fn rows(&self) -> usize {
        (self.h + 1).div_ceil(2)
    }

    /// The largest version that fits `w` columns and `h` rows, at full size or
    /// half: smaller, it's a smudge.
    pub fn fitting(&self, w: usize, h: usize) -> Option<Sprite> {
        let mut s = self.clone();
        for _ in 0..2 {
            if s.w > 0 && s.w <= w && s.rows() <= h {
                return Some(s);
            }
            s = s.halved();
        }
        None
    }

    /// Two pixels to a cell, the top one as the glyph's colour and the bottom
    /// one as the cell's.
    pub fn lines(&self, look: Look, theme: &Theme, truecolor: bool) -> Vec<Line<'static>> {
        let darkest = self.px.iter().flatten().min_by_key(|p| luma(**p) as u32).copied().unwrap_or([0, 0, 0]);
        let shade = |p: Rgb| -> Color {
            let mut p = p;
            if look.blink && luma(p) > 190.0 {
                p = darkest;
            }
            if look.asleep {
                let g = (luma(p) * 0.55) as u8;
                p = [g, g, g.saturating_add(8)];
            }
            if look.glow > 0.0 {
                p = mix(p, [255, 255, 255], look.glow * 0.35);
            }
            rgb(p, truecolor)
        };
        let top = if look.low { 1 } else { 0 };
        let pixel = |x: usize, y: usize| if y >= top { self.at(x, y - top) } else { None };
        (0..self.rows())
            .map(|row| {
                let spans: Vec<Span> = (0..self.w)
                    .map(|x| match (pixel(x, row * 2), pixel(x, row * 2 + 1)) {
                        (None, None) => Span::styled(" ", Style::new().bg(theme.bg)),
                        (Some(a), None) => Span::styled("▀", Style::new().fg(shade(a)).bg(theme.bg)),
                        (None, Some(b)) => Span::styled("▄", Style::new().fg(shade(b)).bg(theme.bg)),
                        (Some(a), Some(b)) => Span::styled("▀", Style::new().fg(shade(a)).bg(shade(b))),
                    })
                    .collect();
                Line::from(spans)
            })
            .collect()
    }
}

fn luma(p: Rgb) -> f32 {
    0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32
}

fn mix(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

fn rgb(p: Rgb, truecolor: bool) -> Color {
    if truecolor { Color::Rgb(p[0], p[1], p[2]) } else { Color::Indexed(nearest_256(p[0], p[1], p[2])) }
}

/// A colour on its way from `from` to `to`, `t` of the way there. Without
/// 24-bit colour it jumps halfway.
pub fn ease(from: Color, to: Color, t: f32) -> Color {
    match (from, to) {
        (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) => {
            let [r, g, b] = mix([r1, g1, b1], [r2, g2, b2], t);
            Color::Rgb(r, g, b)
        }
        _ if t < 0.5 => from,
        _ => to,
    }
}

/// How far along an ease that started at `since` and lasts `ms` is, from 0 to 1.
pub fn progress(since: i64, now: i64, ms: i64) -> f32 {
    if since <= 0 { 1.0 } else { ((now - since) as f32 / ms as f32).clamp(0.0, 1.0) }
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A spinner's frame, at a phase of its own so a list of them doesn't tick in step.
pub fn spinner(now: i64, phase: &str) -> &'static str {
    let offset = phase.bytes().fold(0usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize));
    SPINNER[((now / 100) as usize + offset) % SPINNER.len()]
}

const BARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// A bar per value, the tallest for the largest. Zero is a blank, so a quiet
/// minute reads as a gap.
pub fn sparkline(values: &[u64]) -> String {
    let most = values.iter().copied().max().unwrap_or(0).max(1);
    values.iter().map(|&v| if v == 0 { ' ' } else { BARS[((v as f64 / most as f64) * 7.0).round() as usize] }).collect()
}

/// A thin meter, `w` wide and `ratio` full.
pub fn meter(ratio: f64, w: usize, fill: Color, theme: &Theme) -> Vec<Span<'static>> {
    let full = ((ratio.clamp(0.0, 1.0) * w as f64).round() as usize).min(w);
    vec![
        Span::styled("━".repeat(full), Style::new().fg(fill)),
        Span::styled("─".repeat(w - full), Style::new().fg(theme.gutter)),
    ]
}

/// A colour for a project, the same every time, so a project's rows stand together.
pub fn project_colour(name: &str, truecolor: bool) -> Color {
    const HUES: [u32; 6] = [0x7dcfff, 0xbb9af7, 0x9ece6a, 0xe0af68, 0x73daca, 0xff9e64];
    let h = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    color(HUES[h as usize % HUES.len()], truecolor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_slime_fits_where_it_can() {
        let s = Sprite::slime();
        assert_eq!((s.w, s.h, s.rows()), (44, 24, 13), "creature.png's grid, a pixel a cell");
        let small = s.fitting(30, 10).unwrap();
        assert_eq!((small.w, small.rows()), (22, 7), "half size where the whole one doesn't fit");
        assert!(s.fitting(3, 1).is_none());
        let theme = Theme::with(true);
        let lines = s.lines(Look::default(), &theme, true);
        assert_eq!(lines.len(), 13);
        assert!(lines.iter().all(|l| l.width() == 44));
    }

    #[test]
    fn asleep_it_has_no_colour() {
        let s = Sprite::slime();
        let theme = Theme::with(true);
        let grey = |c: Color| matches!(c, Color::Rgb(r, g, b) if r == g && b >= g && b - g <= 8);
        for line in s.lines(Look { asleep: true, ..Look::default() }, &theme, true) {
            for span in line.spans.iter().filter(|s| s.content != " ") {
                assert!(span.style.fg.is_none_or(grey), "{:?}", span.style);
            }
        }
    }

    #[test]
    fn sparklines_spinners_and_meters() {
        assert_eq!(sparkline(&[0, 1, 4, 8]), " ▂▅█");
        assert_eq!(sparkline(&[0, 0]), "  ", "nothing at all is all gaps");
        assert_ne!(spinner(0, "a"), spinner(0, "b"));
        assert_ne!(spinner(0, "a"), spinner(100, "a"));
        assert_eq!(ease(Color::Rgb(0, 0, 0), Color::Rgb(200, 100, 0), 0.5), Color::Rgb(100, 50, 0));
        let theme = Theme::with(true);
        let m = meter(1.4, 8, theme.red, &theme);
        assert_eq!((m[0].content.chars().count(), m[1].content.chars().count()), (8, 0), "past full, it's full");
    }
}
