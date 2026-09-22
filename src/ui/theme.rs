//! Parses the config file's `Theme` colour-name strings into resolved
//! `ratatui` styles, with graceful degradation for lower-depth terminals.

use ratatui::style::{Color, Modifier, Style};

use crate::config::Theme;
use crate::domain::dates::Urgency;

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
///
/// The six deadline-urgency colours (`urgency_*`) are not driven by
/// `Theme` fields: `Theme` lives in `config.rs`, which this phase's file
/// ownership excludes from editing, and it derives its `Deserialize` impl
/// with `#[serde(deny_unknown_fields)]`, so new fields could not be added
/// there without touching that file. They are instead fixed constants
/// (see `urgency_style`), resolved once per detected `ColorDepth` here and
/// cached on `Styles` so rendering never repeats the depth match.
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
    pub urgency_none: Style,
    pub urgency_distant: Style,
    pub urgency_soon: Style,
    pub urgency_near: Style,
    pub urgency_imminent: Style,
    pub urgency_overdue: Style,
}

impl Styles {
    /// The style a card should render in for deadline-urgency bucket `u`.
    pub fn for_urgency(&self, u: Urgency) -> Style {
        match u {
            Urgency::None => self.urgency_none,
            Urgency::Distant => self.urgency_distant,
            Urgency::Soon => self.urgency_soon,
            Urgency::Near => self.urgency_near,
            Urgency::Imminent => self.urgency_imminent,
            Urgency::Overdue => self.urgency_overdue,
        }
    }
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
        urgency_none: urgency_style(theme, Urgency::None, depth),
        urgency_distant: urgency_style(theme, Urgency::Distant, depth),
        urgency_soon: urgency_style(theme, Urgency::Soon, depth),
        urgency_near: urgency_style(theme, Urgency::Near, depth),
        urgency_imminent: urgency_style(theme, Urgency::Imminent, depth),
        urgency_overdue: urgency_style(theme, Urgency::Overdue, depth),
    }
}

/// Card colour as a pure function of deadline urgency and the terminal's
/// colour depth (the user-specified mapping, see `HANDOFF.md`):
///
/// | Urgency (deadline)      | Colour                              |
/// |--------------------------|--------------------------------------|
/// | `None` (no due date)     | grey                                  |
/// | `Distant` (>= 15 days)   | green                                  |
/// | `Soon` (5-14 days)       | yellow                                 |
/// | `Near` (2-4 days)        | orange (256-index 208); yellow at 16   |
/// | `Imminent` (0-1 days)    | red                                     |
/// | `Overdue` (< 0 days)     | black background, white foreground     |
pub fn urgency_style(theme: &Theme, urgency: Urgency, depth: ColorDepth) -> Style {
    match urgency {
        Urgency::None => parse_style(&theme.deadline_none, depth),
        Urgency::Distant => parse_style(&theme.deadline_distant, depth),
        Urgency::Soon => parse_style(&theme.deadline_soon, depth),
        Urgency::Near => parse_style(&theme.deadline_near, depth),
        Urgency::Imminent => parse_style(&theme.deadline_imminent, depth),
        // The only bucket with a background: overdue is white on black.
        Urgency::Overdue => {
            let fg = parse_style(&theme.deadline_overdue_fg, depth)
                .fg
                .unwrap_or(Color::White);
            let bg = parse_style(&theme.deadline_overdue_bg, depth)
                .fg
                .unwrap_or(Color::Black);
            Style::default().fg(fg).bg(bg)
        }
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
/// The six levels of the xterm 6x6x6 colour cube. They are NOT evenly
/// spaced -- assuming they were made `#ff8700` (which is exactly index
/// 208) quantise to 214, because g=135 rounded up to 175.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

fn nearest_cube_level(v: u8) -> usize {
    let mut best = 0;
    let mut best_d = i32::MAX;
    for (i, level) in CUBE_LEVELS.iter().enumerate() {
        let d = (i32::from(v) - i32::from(*level)).abs();
        if d < best_d {
            best_d = d;
            best = i;
        }
    }
    best
}

/// Quantises a truecolor value to the closest xterm-256 colour, choosing
/// between the colour cube and the 24-step greyscale ramp -- greys land
/// far better on the ramp than on the cube.
fn nearest_ansi256(r: u8, g: u8, b: u8) -> Color {
    let (ci, cj, ck) = (
        nearest_cube_level(r),
        nearest_cube_level(g),
        nearest_cube_level(b),
    );
    let (cr, cg, cb) = (CUBE_LEVELS[ci], CUBE_LEVELS[cj], CUBE_LEVELS[ck]);
    let cube_index = 16 + 36 * ci + 6 * cj + ck;
    let cube_d = sq_dist(r, g, b, cr, cg, cb);

    // Greyscale ramp: indices 232..=255 hold the values 8, 18, ... 238.
    let avg = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let gi = (((avg as i32) - 8) as f32 / 10.0).round().clamp(0.0, 23.0) as u8;
    let grey = 8 + 10 * gi;
    let grey_d = sq_dist(r, g, b, grey, grey, grey);

    if grey_d < cube_d {
        Color::Indexed(232 + gi)
    } else {
        Color::Indexed(cube_index as u8)
    }
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

    // --- deadline-urgency colour mapping, every bucket x every depth -----

    const DEPTHS: [ColorDepth; 3] = [ColorDepth::TrueColor, ColorDepth::Ansi256, ColorDepth::Ansi16];

    #[test]
    fn urgency_none_is_grey_at_every_depth() {
        for depth in DEPTHS {
            let style = urgency_style(&Theme::default(), Urgency::None, depth);
            assert_eq!(style.fg, Some(Color::DarkGray), "depth {depth:?}");
            assert_eq!(style.bg, None, "depth {depth:?}");
        }
    }

    #[test]
    fn urgency_distant_is_green_at_every_depth() {
        for depth in DEPTHS {
            let style = urgency_style(&Theme::default(), Urgency::Distant, depth);
            assert_eq!(style.fg, Some(Color::Green), "depth {depth:?}");
        }
    }

    #[test]
    fn urgency_soon_is_yellow_at_every_depth() {
        for depth in DEPTHS {
            let style = urgency_style(&Theme::default(), Urgency::Soon, depth);
            assert_eq!(style.fg, Some(Color::Yellow), "depth {depth:?}");
        }
    }

    #[test]
    fn nearest_ansi256_uses_the_real_xterm_cube_levels() {
        // #ff8700 IS xterm 208 exactly; the old evenly-spaced maths gave 214.
        assert_eq!(nearest_ansi256(0xff, 0x87, 0x00), Color::Indexed(208));
        assert_eq!(nearest_ansi256(0, 0, 0), Color::Indexed(16));
        assert_eq!(nearest_ansi256(0xff, 0xff, 0xff), Color::Indexed(231));
        // a mid grey belongs on the greyscale ramp, not the cube
        match nearest_ansi256(0x80, 0x80, 0x80) {
            Color::Indexed(i) => assert!((232..=255).contains(&i), "got {i}"),
            other => panic!("expected an indexed colour, got {other:?}"),
        }
    }

    /// Orange has no ANSI name, so the default is the hex `#ff8700`. That
    /// gives an exact orange on a truecolor terminal and degrades on its
    /// own to 256-colour index 208, then to a basic colour at 16.
    #[test]
    fn urgency_near_is_orange_and_degrades_by_colour_depth() {
        let t = Theme::default();
        assert_eq!(
            urgency_style(&t, Urgency::Near, ColorDepth::TrueColor).fg,
            Some(Color::Rgb(255, 135, 0))
        );
        assert_eq!(
            urgency_style(&t, Urgency::Near, ColorDepth::Ansi256).fg,
            Some(Color::Indexed(208))
        );
        // At 16 colours there is no orange at all; any warm fallback is
        // acceptable, but it must still resolve to something.
        assert!(urgency_style(&t, Urgency::Near, ColorDepth::Ansi16).fg.is_some());
    }

    /// The six deadline colours must be overridable from config.toml --
    /// this is the whole point of them living in `Theme`.
    #[test]
    fn deadline_colours_come_from_the_config() {
        let t = Theme {
            deadline_none: "magenta".to_string(),
            deadline_overdue_fg: "yellow".to_string(),
            deadline_overdue_bg: "blue".to_string(),
            ..Theme::default()
        };
        let s = resolve(&t, ColorDepth::TrueColor);
        assert_eq!(s.urgency_none.fg, Some(Color::Magenta));
        assert_eq!(s.urgency_overdue.fg, Some(Color::Yellow));
        assert_eq!(s.urgency_overdue.bg, Some(Color::Blue));
    }

    #[test]
    fn urgency_imminent_is_red_at_every_depth() {
        for depth in DEPTHS {
            let style = urgency_style(&Theme::default(), Urgency::Imminent, depth);
            assert_eq!(style.fg, Some(Color::Red), "depth {depth:?}");
        }
    }

    #[test]
    fn urgency_overdue_is_black_background_white_foreground_at_every_depth() {
        for depth in DEPTHS {
            let style = urgency_style(&Theme::default(), Urgency::Overdue, depth);
            assert_eq!(style.fg, Some(Color::White), "depth {depth:?}");
            assert_eq!(style.bg, Some(Color::Black), "depth {depth:?}");
        }
    }

    #[test]
    fn for_urgency_matches_precomputed_styles_field() {
        let styles = resolve(&Theme::default(), ColorDepth::TrueColor);
        assert_eq!(styles.for_urgency(Urgency::None), styles.urgency_none);
        assert_eq!(styles.for_urgency(Urgency::Overdue), styles.urgency_overdue);
    }
}
