//! The always-visible, two-line key-hint / status bar drawn at the bottom
//! of every frame.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::App;
use crate::keymap::{self, Binding, Ctx};
use crate::mode::Mode;
use crate::ui::theme::Styles;

const SEP: &str = "  ";
const ELLIPSIS: &str = "…";

/// Picks the prefix of `hints` (assumed already sorted by importance, most
/// important first) that fits within `width` columns when rendered as
/// `"key label"` pairs separated by `SEP`, reserving room for `ELLIPSIS`
/// when anything had to be dropped. Never returns more entries than fit.
fn select_fitting<'a>(hints: &[&'a Binding], width: u16) -> (Vec<&'a Binding>, bool) {
    let width = width as usize;
    let mut included: Vec<&Binding> = Vec::new();
    let mut used = 0usize;
    let mut dropped = false;

    for b in hints {
        let piece_len = b.keys.chars().count() + 1 + b.label.chars().count();
        let addition = if included.is_empty() {
            piece_len
        } else {
            piece_len + SEP.chars().count()
        };
        if used + addition <= width {
            used += addition;
            included.push(b);
        } else {
            dropped = true;
            break;
        }
    }

    if dropped {
        let ellipsis_len = ELLIPSIS.chars().count();
        loop {
            let extra = if included.is_empty() {
                ellipsis_len
            } else {
                SEP.chars().count() + ellipsis_len
            };
            if used + extra <= width {
                break;
            }
            if included.pop().is_none() {
                break;
            }
            used = included.iter().enumerate().fold(0, |acc, (i, b)| {
                let piece_len = b.keys.chars().count() + 1 + b.label.chars().count();
                acc + if i == 0 { piece_len } else { piece_len + SEP.chars().count() }
            });
        }
    }

    (included, dropped)
}

/// Fits `hints` into `width` display columns, dropping the least important
/// (highest-rank) entries first and appending an ellipsis when anything was
/// dropped. Pure and total: never wraps, never exceeds `width`, never
/// panics, and never cuts a hint mid-word.
pub fn fit_hints(hints: &[&Binding], width: u16) -> String {
    if width == 0 {
        return String::new();
    }
    let (included, dropped) = select_fitting(hints, width);
    let mut pieces: Vec<String> = included.iter().map(|b| format!("{} {}", b.keys, b.label)).collect();
    if dropped {
        if pieces.is_empty() && ELLIPSIS.chars().count() > width as usize {
            return String::new();
        }
        pieces.push(ELLIPSIS.to_string());
    }
    pieces.join(SEP)
}

fn hint_line(hints: &[&Binding], width: u16, styles: &Styles) -> Line<'static> {
    let (included, dropped) = select_fitting(hints, width);
    let mut spans: Vec<Span<'static>> = Vec::new();
    for (i, b) in included.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(SEP, styles.keyhints));
        }
        spans.push(Span::styled(
            b.keys,
            styles.keyhints.add_modifier(ratatui::style::Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {}", b.label), styles.keyhints));
    }
    if dropped {
        if !included.is_empty() {
            spans.push(Span::styled(SEP, styles.keyhints));
        }
        spans.push(Span::styled(ELLIPSIS, styles.keyhints));
    }
    Line::from(spans)
}

fn mode_label(app: &App) -> String {
    match app.mode {
        Mode::Command => format!(":{}", app.cmdline),
        Mode::Search => format!("/{}", app.search),
        Mode::Insert => "INSERT".to_string(),
        Mode::Normal => "NORMAL".to_string(),
    }
}

fn status_line(app: &App, width: u16, styles: &Styles) -> Line<'static> {
    let left = mode_label(app);
    let right = app.message.clone().unwrap_or_default();
    let width = width as usize;
    let left_len = left.chars().count();
    let right_len = right.chars().count();

    if right.is_empty() || left_len + 1 > width {
        let mut text = left;
        if text.chars().count() > width {
            text = text.chars().take(width).collect();
        }
        return Line::from(Span::styled(text, styles.default));
    }

    let gap = width.saturating_sub(left_len + right_len);
    if gap == 0 {
        return Line::from(Span::styled(left, styles.default));
    }
    let mut text = left;
    text.push_str(&" ".repeat(gap));
    text.push_str(&right);
    Line::from(Span::styled(text, styles.default))
}

/// Draws the two-line footer: hints (unless `show_keyhints` is off) on the
/// first line, mode indicator / input buffer and status message on the
/// second.
pub fn render(frame: &mut Frame, area: Rect, app: &App, styles: &Styles) {
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(area);

    if app.config.show_keyhints {
        let ctx = app.ctx();
        let mut hints = keymap::hints_for(ctx);
        if matches!(ctx, Ctx::Board | Ctx::Notes) {
            hints.extend(keymap::hints_for(Ctx::Global));
        }
        hints.sort_by_key(|b| b.rank);
        let line = hint_line(&hints, rows[0].width, styles);
        frame.render_widget(Paragraph::new(line), rows[0]);
    }

    let line2 = status_line(app, rows[1].width, styles);
    frame.render_widget(Paragraph::new(line2), rows[1]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<&'static Binding> {
        keymap::hints_for(Ctx::Board)
    }

    #[test]
    fn width_200_fits_everything_no_ellipsis() {
        let hints = sample();
        let out = fit_hints(&hints, 200);
        assert!(!out.contains('…'));
        for b in &hints {
            assert!(out.contains(b.keys));
        }
        assert!(out.chars().count() <= 200);
    }

    #[test]
    fn width_80_never_exceeds_and_never_panics() {
        let hints = sample();
        let out = fit_hints(&hints, 80);
        assert!(out.chars().count() <= 80);
    }

    #[test]
    fn width_60_never_exceeds_and_never_panics() {
        let hints = sample();
        let out = fit_hints(&hints, 60);
        assert!(out.chars().count() <= 60);
    }

    #[test]
    fn width_20_degrades_with_ellipsis_and_no_mid_word_cut() {
        let hints = sample();
        let out = fit_hints(&hints, 20);
        assert!(out.chars().count() <= 20);
        if out.contains('…') {
            assert!(out.ends_with('…'));
        }
    }

    #[test]
    fn width_5_degrades_sanely_without_panicking() {
        let hints = sample();
        let out = fit_hints(&hints, 5);
        assert!(out.chars().count() <= 5);
    }

    #[test]
    fn width_0_returns_empty_without_panicking() {
        let hints = sample();
        let out = fit_hints(&hints, 0);
        assert_eq!(out, "");
    }

    #[test]
    fn form_and_dropdown_contexts_have_distinct_hint_content() {
        let form_hints = keymap::hints_for(Ctx::Form);
        let dropdown_hints = keymap::hints_for(Ctx::Dropdown);
        let board_hints = keymap::hints_for(Ctx::Board);
        let form_line = fit_hints(&form_hints, 200);
        let dropdown_line = fit_hints(&dropdown_hints, 200);
        let board_line = fit_hints(&board_hints, 200);
        assert_ne!(form_line, board_line);
        assert_ne!(dropdown_line, board_line);
        assert!(form_line.contains("save"));
        assert!(dropdown_line.contains("select"));
    }

    #[test]
    fn empty_hints_never_panics() {
        let hints: Vec<&Binding> = Vec::new();
        assert_eq!(fit_hints(&hints, 50), "");
    }
}
