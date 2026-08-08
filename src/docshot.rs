//! Headless renderer for documentation screenshots.
//!
//! The TUI is drawn into a ratatui `TestBackend` buffer and emitted as SVG, so
//! README images are a build artifact instead of a manual screen capture. Two
//! properties matter and neither is achievable with a real screenshot:
//!
//! * **No private data can leak.** The render never touches the machine's
//!   transcripts, spend, or secrets — tabs that would show them are seeded with
//!   synthetic fixtures. A screenshot of a real session cannot make that
//!   guarantee, which is why the Secrets and Egress tabs have never had one.
//! * **Images cannot go stale silently.** Regenerating is a test run, so a tab
//!   that changes shape shows up as a diff instead of drifting away from the
//!   docs for months.
//!
//! SVG rather than PNG: sharp at any zoom, roughly an order of magnitude smaller
//! than the equivalent PNG, and diffable in review.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

/// Terminal cell metrics, in SVG user units. Tuned so the glyph advance matches
/// a typical monospace face at this size; box-drawing characters butt together
/// with no seam.
const CELL_W: f32 = 8.4;
const CELL_H: f32 = 18.0;
const FONT_SIZE: f32 = 14.0;
/// Baseline offset inside the cell box.
const BASELINE: f32 = 13.6;

/// Palette used for the 16 ANSI colors. Chosen to stay legible on the dark
/// canvas below and to keep tokenix's orange accent recognizable.
fn ansi_hex(c: Color) -> Option<&'static str> {
    Some(match c {
        Color::Black => "#1c1f24",
        Color::Red => "#e5534b",
        Color::Green => "#3fb950",
        Color::Yellow => "#d29922",
        Color::Blue => "#4a9eff",
        Color::Magenta => "#bc8cff",
        Color::Cyan => "#39c5cf",
        Color::Gray => "#8b949e",
        Color::DarkGray => "#484f58",
        Color::LightRed => "#ff7b72",
        Color::LightGreen => "#56d364",
        Color::LightYellow => "#e3b341",
        Color::LightBlue => "#79c0ff",
        Color::LightMagenta => "#d2a8ff",
        Color::LightCyan => "#56d4dd",
        Color::White => "#e6edf3",
        _ => return None,
    })
}

const CANVAS_BG: &str = "#0d1117";
const DEFAULT_FG: &str = "#e6edf3";

fn fg_hex(c: Color) -> String {
    match c {
        Color::Reset => DEFAULT_FG.to_string(),
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Indexed(i) => indexed_hex(i),
        other => ansi_hex(other).unwrap_or(DEFAULT_FG).to_string(),
    }
}

fn bg_hex(c: Color) -> Option<String> {
    match c {
        Color::Reset => None,
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Indexed(i) => Some(indexed_hex(i)),
        other => ansi_hex(other).map(str::to_string),
    }
}

/// xterm-256 cube + grayscale ramp. The first 16 fall back to the ANSI palette.
fn indexed_hex(i: u8) -> String {
    match i {
        0..=15 => {
            const BASE: [Color; 16] = [
                Color::Black,
                Color::Red,
                Color::Green,
                Color::Yellow,
                Color::Blue,
                Color::Magenta,
                Color::Cyan,
                Color::Gray,
                Color::DarkGray,
                Color::LightRed,
                Color::LightGreen,
                Color::LightYellow,
                Color::LightBlue,
                Color::LightMagenta,
                Color::LightCyan,
                Color::White,
            ];
            ansi_hex(BASE[i as usize]).unwrap_or(DEFAULT_FG).to_string()
        }
        16..=231 => {
            let n = i - 16;
            let steps = [0u8, 95, 135, 175, 215, 255];
            let r = steps[(n / 36) as usize];
            let g = steps[((n % 36) / 6) as usize];
            let b = steps[(n % 6) as usize];
            format!("#{r:02x}{g:02x}{b:02x}")
        }
        _ => {
            let v = 8 + (i - 232) * 10;
            format!("#{v:02x}{v:02x}{v:02x}")
        }
    }
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
    out
}

/// A run of adjacent cells sharing one style — emitted as a single `<text>`
/// element so a full-screen render stays a few hundred nodes rather than
/// width × height of them.
struct Run {
    x: u16,
    text: String,
    fg: String,
    bg: Option<String>,
    bold: bool,
    italic: bool,
}

/// Render a ratatui buffer to a standalone SVG document.
pub fn buffer_to_svg(buf: &Buffer) -> String {
    let area = buf.area;
    let (w, h) = (area.width, area.height);
    let px_w = w as f32 * CELL_W;
    let px_h = h as f32 * CELL_H;

    let mut svg = String::with_capacity(64 * 1024);
    svg.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {px_w:.0} {px_h:.0}" width="{px_w:.0}" height="{px_h:.0}" font-family="ui-monospace,'SF Mono','Cascadia Mono',Consolas,'DejaVu Sans Mono',monospace" font-size="{FONT_SIZE}">
<rect width="100%" height="100%" rx="6" fill="{CANVAS_BG}"/>
"#
    ));

    for y in 0..h {
        let mut runs: Vec<Run> = Vec::new();
        for x in 0..w {
            let idx = (y as usize) * (w as usize) + (x as usize);
            let Some(cell) = buf.content.get(idx) else {
                continue;
            };
            let sym = cell.symbol();
            if sym.is_empty() {
                continue;
            }
            let fg = fg_hex(cell.fg);
            let bg = bg_hex(cell.bg);
            let bold = cell.modifier.contains(Modifier::BOLD);
            let italic = cell.modifier.contains(Modifier::ITALIC);

            match runs.last_mut() {
                Some(r)
                    if r.fg == fg
                        && r.bg == bg
                        && r.bold == bold
                        && r.italic == italic
                        && r.x as usize + r.text.chars().count() == x as usize =>
                {
                    r.text.push_str(sym);
                }
                _ => runs.push(Run {
                    x,
                    text: sym.to_string(),
                    fg,
                    bg,
                    bold,
                    italic,
                }),
            }
        }

        // Backgrounds first so glyphs are never painted over.
        for r in &runs {
            if let Some(bg) = &r.bg {
                let cols = r.text.chars().count() as f32;
                svg.push_str(&format!(
                    r#"<rect x="{:.2}" y="{:.2}" width="{:.2}" height="{:.2}" fill="{}"/>
"#,
                    r.x as f32 * CELL_W,
                    y as f32 * CELL_H,
                    cols * CELL_W,
                    CELL_H,
                    bg
                ));
            }
        }
        for r in &runs {
            if r.text.trim().is_empty() {
                continue;
            }
            let mut attrs = String::new();
            if r.bold {
                attrs.push_str(r#" font-weight="bold""#);
            }
            if r.italic {
                attrs.push_str(r#" font-style="italic""#);
            }
            // `xml:space` keeps runs of interior spaces (table padding, tree
            // indentation) from collapsing.
            svg.push_str(&format!(
                r#"<text xml:space="preserve" x="{:.2}" y="{:.2}" fill="{}"{}>{}</text>
"#,
                r.x as f32 * CELL_W,
                y as f32 * CELL_H + BASELINE,
                r.fg,
                attrs,
                escape(&r.text)
            ));
        }
    }

    svg.push_str("</svg>\n");
    svg
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::style::Style;

    fn probe() -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, 12, 2));
        buf.set_string(0, 0, "tokenix", Style::default().light_blue().bold());
        buf.set_string(0, 1, "a<b&c", Style::default());
        buf
    }

    #[test]
    fn emits_a_well_formed_document() {
        let svg = buffer_to_svg(&probe());
        assert!(svg.starts_with("<svg xmlns="));
        assert!(svg.trim_end().ends_with("</svg>"));
        assert_eq!(svg.matches("<svg").count(), 1);
    }

    #[test]
    fn escapes_xml_metacharacters() {
        let svg = buffer_to_svg(&probe());
        assert!(svg.contains("a&lt;b&amp;c"), "must escape < and &: {svg}");
        assert!(!svg.contains("a<b&c"));
    }

    #[test]
    fn merges_adjacent_cells_of_one_style_into_one_run() {
        let svg = buffer_to_svg(&probe());
        // "tokenix" is a single styled run, not seven <text> elements.
        assert!(svg.contains(">tokenix<"), "run not merged: {svg}");
        assert!(svg.contains(r#"font-weight="bold""#));
    }

    #[test]
    fn preserves_interior_whitespace() {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 1));
        buf.set_string(0, 0, "a    b", Style::default());
        let svg = buffer_to_svg(&buf);
        assert!(svg.contains(r#"xml:space="preserve""#));
        // The row is one default-styled run, so it carries the buffer's trailing
        // padding too — what matters is that the interior gap survived.
        assert!(svg.contains(">a    b"), "spacing collapsed: {svg}");
    }

    #[test]
    fn indexed_colors_cover_cube_and_ramp() {
        assert_eq!(indexed_hex(16), "#000000");
        assert_eq!(indexed_hex(231), "#ffffff");
        assert_eq!(indexed_hex(232), "#080808");
        // Low indices fall back to the ANSI palette rather than the cube.
        assert_eq!(indexed_hex(1), "#e5534b");
    }
}
