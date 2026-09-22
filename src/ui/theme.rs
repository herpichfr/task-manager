//! Parses the config file's `Theme` colour-name strings into resolved
//! `ratatui` styles, with graceful degradation for lower-depth terminals.

use ratatui::style::{Color, Modifier, Style};

use crate::config::Theme;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorDepth {
    TrueColor,
    Ansi256,
    Ansi16,
}

/// Detects the terminal's colour depth from `$COLORTERM` and `$TERM`.
/// `COLORTERM` of `truecolor` or `24bit` wins outright; otherwise a `TERM`
/// containing `256` means 256-colour; anything else is treated as basic
/// 16-colour ANSI.
pub fn detect_depth(colorterm: Option<&str>, term: Option<&str>) -> ColorDepth {
    if let Some(c) = colorterm {
        let c = c.to_ascii_lowercase();
        if c == "truecolor" || c == "24bit" {
            return ColorDepth::TrueColor;
        }
    }
    if let Some(t) = term {
        if t.contains("256") {
            return ColorDepth::Ansi256;
        }
    }
    ColorDepth::Ansi16
}

/// Resolved styles for every themeable element, plus a plain default used
/// as the fallback for anything unparseable.
#[derive(Debug, Clone, Copy)]
pub struct Styles {
    pub todo: Style,
    pub doing: Style,
    pub done: Style,
    pub priority_low: Style,
    pub priority_normal: Style,
    pub priority_high: Style,
    pub priority_urgent: Style,
    pub border: Style,
    pub selection: Style,
    pub keyhints: Style,
    pub default: Style,
}

pub fn resolve(theme: &Theme, depth: ColorDepth) -> Styles {
    Styles {
        todo: parse_style(&theme.todo, depth),
        doing: parse_style(&theme.doing, depth),
        done: parse_style(&theme.done, depth),
        priority_low: parse_style(&theme.priority_low, depth),
        priority_normal: parse_style(&theme.priority_normal, depth),
        priority_high: parse_style(&theme.priority_high, depth),
        priority_urgent: parse_style(&theme.priority_urgent, depth),
        border: parse_style(&theme.border, depth),
        selection: parse_style(&theme.selection, depth),
        keyhints: parse_style(&theme.keyhints, depth),
        default: Style::default(),
    }
}

/// Parses one theme colour-name string into a `Style`. `"reverse"` is a
/// modifier, not a colour. An unparseable name falls back to the plain
/// default style rather than panicking.
fn parse_style(name: &str, depth: ColorDepth) -> Style {
    if name.eq_ignore_ascii_case("reverse") {
        return Style::default().add_modifier(Modifier::REVERSED);
    }
    if let Some((r, g, b)) = parse_hex(name) {
        let color = match depth {
            ColorDepth::TrueColor => Color::Rgb(r, g, b),
            ColorDepth::Ansi256 => nearest_ansi256(r, g, b),
            ColorDepth::Ansi16 => nearest_ansi16(r, g, b),
        };
        return Style::default().fg(color);
    }
    if let Some(color) = parse_named(name) {
        return Style::default().fg(color);
    }
    Style::default()
}

fn parse_named(name: &str) -> Option<Color> {
    let color = match name.to_ascii_lowercase().as_str() {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "darkgray" | "darkgrey" => Color::DarkGray,
        "lightred" => Color::LightRed,
        "lightgreen" => Color::LightGreen,
        "lightyellow" => Color::LightYellow,
        "lightblue" => Color::LightBlue,
        "lightmagenta" => Color::LightMagenta,
        "lightcyan" => Color::LightCyan,
        "white" => Color::White,
        _ => return None,
    };
    Some(color)
}

fn parse_hex(name: &str) -> Option<(u8, u8, u8)> {
    let s = name.strip_prefix('#')?;
    if s.len() != 6 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some((r, g, b))
}

const ANSI16_PALETTE: [(Color, (u8, u8, u8)); 16] = [
    (Color::Black, (0, 0, 0)),
    (Color::Red, (128, 0, 0)),
    (Color::Green, (0, 128, 0)),
    (Color::Yellow, (128, 128, 0)),
    (Color::Blue, (0, 0, 128)),
    (Color::Magenta, (128, 0, 128)),
    (Color::Cyan, (0, 128, 128)),
    (Color::Gray, (192, 192, 192)),
    (Color::DarkGray, (128, 128, 128)),
    (Color::LightRed, (255, 0, 0)),
    (Color::LightGreen, (0, 255, 0)),
    (Color::LightYellow, (255, 255, 0)),
    (Color::LightBlue, (0, 0, 255)),
    (Color::LightMagenta, (255, 0, 255)),
    (Color::LightCyan, (0, 255, 255)),
    (Color::White, (255, 255, 255)),
];

/// Downgrades a 24-bit colour to the nearest of the 16 basic ANSI colours.
fn nearest_ansi16(r: u8, g: u8, b: u8) -> Color {
    ANSI16_PALETTE
        .iter()
        .min_by_key(|(_, (pr, pg, pb))| sq_dist(r, g, b, *pr, *pg, *pb))
        .map(|(c, _)| *c)
        .unwrap_or(Color::White)
}

/// Downgrades a 24-bit colour to the nearest colour in the standard
/// 6x6x6 xterm-256 colour cube (indices 16-231).
fn nearest_ansi256(r: u8, g: u8, b: u8) -> Color {
    let to_cube = |v: u8| -> u16 { ((u16::from(v) * 5 + 127) / 255).min(5) };
    let (cr, cg, cb) = (to_cube(r), to_cube(g), to_cube(b));
    let index = 16 + 36 * cr + 6 * cg + cb;
    Color::Indexed(index as u8)
}

fn sq_dist(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> i32 {
    let dr = i32::from(r1) - i32::from(r2);
    let dg = i32::from(g1) - i32::from(g2);
    let db = i32::from(b1) - i32::from(b2);
    dr * dr + dg * dg + db * db
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_depth_truecolor_from_colorterm() {
        assert_eq!(detect_depth(Some("truecolor"), Some("xterm")), ColorDepth::TrueColor);
        assert_eq!(detect_depth(Some("24bit"), None), ColorDepth::TrueColor);
        assert_eq!(detect_depth(Some("TrueColor"), None), ColorDepth::TrueColor);
    }

    #[test]
    fn detect_depth_256_from_term() {
        assert_eq!(detect_depth(None, Some("xterm-256color")), ColorDepth::Ansi256);
    }

    #[test]
    fn detect_depth_falls_back_to_ansi16() {
        assert_eq!(detect_depth(None, Some("xterm")), ColorDepth::Ansi16);
        assert_eq!(detect_depth(None, None), ColorDepth::Ansi16);
    }

    #[test]
    fn parse_named_color() {
        let style = parse_style("yellow", ColorDepth::TrueColor);
        assert_eq!(style.fg, Some(Color::Yellow));
    }

    #[test]
    fn parse_reverse_is_a_modifier_not_a_color() {
        let style = parse_style("reverse", ColorDepth::TrueColor);
        assert_eq!(style.fg, None);
        assert!(style.add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn parse_bad_name_falls_back_to_default_without_panicking() {
        let style = parse_style("not-a-real-color", ColorDepth::TrueColor);
        assert_eq!(style, Style::default());
    }

    #[test]
    fn parse_hex_truecolor_is_exact_rgb() {
        let style = parse_style("#336699", ColorDepth::TrueColor);
        assert_eq!(style.fg, Some(Color::Rgb(0x33, 0x66, 0x99)));
    }

    #[test]
    fn parse_hex_ansi256_downgrades_to_indexed() {
        let style = parse_style("#336699", ColorDepth::Ansi256);
        assert!(matches!(style.fg, Some(Color::Indexed(_))));
    }

    #[test]
    fn parse_hex_ansi16_downgrades_to_basic_color() {
        let style = parse_style("#ff0000", ColorDepth::Ansi16);
        assert_eq!(style.fg, Some(Color::LightRed));
    }

    #[test]
    fn resolve_builds_all_styles_without_panicking() {
        let theme = Theme::default();
        let styles = resolve(&theme, ColorDepth::Ansi16);
        assert_eq!(styles.todo.fg, Some(Color::Yellow));
        assert!(styles.selection.add_modifier.contains(Modifier::REVERSED));
    }
}
