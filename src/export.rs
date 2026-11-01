//! Выгрузка схемы. SVG строится по всей истории и остаётся векторным, поэтому годится
//! и для README, и для печати. PNG снимает текущий вид в двойном разрешении (см. shell).

use crate::app::Session;
use crate::git::Idx;
use crate::layout::{LANE, LineKind};
use crate::map::route;
use crate::theme::{Palette, mix};
use std::fmt::Write;
use vello::kurbo::Point;
use vello::peniko::Color;

const MARGIN: f64 = 80.0;

fn hex(color: Color) -> String {
    let [r, g, b, _] = color.components;
    let channel = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", channel(r), channel(g), channel(b))
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn width_of(kind: LineKind) -> f64 {
    match kind {
        LineKind::Main => 8.0,
        LineKind::Active => 5.0,
        LineKind::Merged => 2.4,
    }
}

pub fn svg(s: &Session, pal: &Palette) -> String {
    let map = &s.map;
    let lanes = (map.min_lane, map.max_lane);
    let top = (lanes.0 as f64 - 1.0) * LANE as f64 - MARGIN;
    let bottom = (lanes.1 as f64 + 1.0) * LANE as f64 + MARGIN;
    let width = map.width as f64 + MARGIN * 2.0 + 260.0;
    let height = bottom - top;
    let at = |commit: Idx| Point::new(map.x[commit as usize] as f64 + MARGIN, map.y(commit) as f64 - top);
    let color_of = |id: usize| {
        let line = &map.lines[id];
        match line.kind {
            LineKind::Main => pal.ink,
            LineKind::Active => pal.lines[line.color],
            LineKind::Merged => mix(pal.lines[line.color], pal.ground, 0.3),
        }
    };
    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width:.0} {height:.0}" width="{width:.0}" height="{height:.0}" font-family="Bahnschrift, 'Segoe UI', sans-serif">"#
    );
    let _ = writeln!(out, r#"<title>{}</title>"#, escape(&s.title));
    let _ = writeln!(out, r#"<rect width="100%" height="100%" fill="{}"/>"#, hex(pal.ground));
    let _ = writeln!(out, r#"<g fill="none" stroke-linecap="round" stroke-linejoin="round">"#);
    for kind in [LineKind::Merged, LineKind::Active, LineKind::Main] {
        for link in &map.links {
            let owner = &map.lines[link.owner as usize];
            if owner.kind != kind {
                continue;
            }
            let points = route(at(link.from), at(link.to), link.fork, 12.0);
            let path: Vec<String> = points.iter().map(|p| format!("{:.1} {:.1}", p.x, p.y)).collect();
            let _ = writeln!(
                out,
                r#"<path d="M{}" stroke="{}" stroke-width="{}"/>"#,
                path.join(" L"),
                hex(color_of(link.owner as usize)),
                width_of(kind)
            );
        }
        for (id, line) in map.lines.iter().enumerate() {
            if !line.visible || line.kind != kind {
                continue;
            }
            let (a, b) = (at(line.commits[0]), at(line.commits[line.commits.len() - 1]));
            let _ = writeln!(
                out,
                r#"<path d="M{:.1} {:.1} L{:.1} {:.1}" stroke="{}" stroke-width="{}"><title>{}</title></path>"#,
                a.x,
                a.y,
                b.x,
                b.y,
                hex(color_of(id)),
                width_of(kind),
                escape(&line.name)
            );
        }
    }
    let _ = writeln!(out, "</g>");
    // Станции: только события, иначе файл большого проекта весил бы мегабайты.
    let _ = writeln!(out, r#"<g fill="{}" stroke="{}" stroke-width="2.5">"#, hex(pal.sheet), hex(pal.ink));
    for (commit, &event) in map.event.iter().enumerate() {
        let commit = commit as Idx;
        if event && map.is_visible(commit) {
            let p = at(commit);
            let kind = map.lines[map.line_of[commit as usize] as usize].kind;
            let radius = if kind == LineKind::Merged { 3.5 } else { 6.0 };
            let _ = writeln!(out, r#"<circle cx="{:.1}" cy="{:.1}" r="{radius}"/>"#, p.x, p.y);
        }
    }
    let _ = writeln!(out, "</g>");
    let _ = writeln!(out, r#"<g font-size="14" font-weight="600">"#);
    for (id, line) in map.lines.iter().enumerate() {
        if !line.visible || line.kind == LineKind::Merged {
            continue;
        }
        let p = at(line.commits[line.commits.len() - 1]);
        let _ = writeln!(
            out,
            r#"<text x="{:.1}" y="{:.1}" fill="{}">{}</text>"#,
            p.x + 14.0,
            p.y + 5.0,
            hex(color_of(id)),
            escape(&line.name)
        );
    }
    let _ = writeln!(out, "</g>");
    for &commit in &s.label_order {
        if map.is_visible(commit) {
            let p = at(commit);
            let _ = writeln!(
                out,
                r#"<text x="{:.1}" y="{:.1}" font-size="12" fill="{}" transform="rotate(-45 {:.1} {:.1})">{}</text>"#,
                p.x + 8.0,
                p.y - 10.0,
                hex(pal.ink),
                p.x + 8.0,
                p.y - 10.0,
                escape(&s.labels[&commit])
            );
        }
    }
    let _ = writeln!(out, "</svg>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_escaped() {
        assert_eq!(escape(r#"a<b>&"c""#), "a&lt;b&gt;&amp;&quot;c&quot;");
    }

    #[test]
    fn colors_are_hex() {
        assert_eq!(hex(Color::from_rgb8(0x1B, 0x1D, 0x21)), "#1b1d21");
    }
}
