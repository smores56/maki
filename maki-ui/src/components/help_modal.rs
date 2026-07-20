use crate::components::ModalScroll;
use crate::components::Overlay;
use crate::components::keybindings::{
    ALT_SEP, Bind, KEYBINDS, Keybind, KeybindContext, ResolvedLabel, all_contexts, format_key, key,
};
use crate::components::modal::Modal;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use maki_lua::KeymapEntry;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::borrow::Cow;
use unicode_width::UnicodeWidthStr;

const TITLE: &str = " Keybindings ";
const KEY_COL_GAP: usize = 2;
const PREFIX_TOP: &str = "  ";
const PREFIX_CHILD: &str = "    ";
/// Maximum rendered width of a plugin-supplied override description.
const MAX_DESC_WIDTH: usize = 40;
/// Hard cap on rendered char count, so an untrusted desc of all zero-width
/// or combining marks cannot grow the render path. Width truncation is the
/// primary bound; this is the safety net.
const MAX_DESC_CHARS: usize = 80;
/// Caps the upstream iterator so a desc of all control/format chars (which
/// `filter` strips, yielding zero items) cannot force `take(MAX_DESC_CHARS)`
/// to scan the whole input. 4x lets a benign desc keep its full 80-char output
/// while bounding the worst-case per-call scan to MAX_DESC_SCAN chars.
const MAX_DESC_SCAN: usize = MAX_DESC_CHARS * 4;

const INPUT_PREFIXES: &[(&str, &str)] = &[
    ("!", "Run shell command (visible to agent)"),
    ("!!", "Run shell command (hidden from agent)"),
];

pub struct HelpModal {
    open: bool,
    scroll: ModalScroll,
}

/// A `Keybind` row paired with its resolved label and visible-bind slice, so a
/// single `view` pass can reuse both across column-width, render, and
/// unmatched-override matching without re-allocating per (row, override).
struct ResolvedRow {
    kb: &'static Keybind,
    label: ResolvedLabel,
    visible: &'static [Bind],
}

fn key_spans(label: &ResolvedLabel, pad: usize, prefix: &str) -> Vec<Span<'static>> {
    let theme = theme::current();
    match label {
        ResolvedLabel::Single(s) => {
            let w = UnicodeWidthStr::width(s.as_str());
            let trailing = pad.saturating_sub(w);
            vec![Span::styled(
                format!("{prefix}{s}{:trailing$}", ""),
                theme.keybind_key,
            )]
        }
        ResolvedLabel::Alt(a, b) => multi_key_spans(&[a.as_str(), b.as_str()], pad, prefix, &theme),
        ResolvedLabel::Multi(keys) => {
            let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
            multi_key_spans(&keys, pad, prefix, &theme)
        }
    }
}

fn multi_key_spans(
    keys: &[&str],
    pad: usize,
    prefix: &str,
    theme: &crate::theme::Theme,
) -> Vec<Span<'static>> {
    let sep_w = UnicodeWidthStr::width(ALT_SEP);
    let content_w: usize = keys
        .iter()
        .map(|k| UnicodeWidthStr::width(*k))
        .sum::<usize>()
        + sep_w * keys.len().saturating_sub(1);
    let trailing = pad.saturating_sub(content_w);
    let mut spans = Vec::with_capacity(keys.len() * 2);
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(ALT_SEP, theme.keybind_desc));
        }
        let text = if i == 0 && i == keys.len() - 1 {
            format!("{prefix}{k}{:trailing$}", "")
        } else if i == 0 {
            format!("{prefix}{k}")
        } else if i == keys.len() - 1 {
            format!("{k}{:trailing$}", "")
        } else {
            (*k).to_string()
        };
        spans.push(Span::styled(text, theme.keybind_key));
    }
    spans
}

fn binds_contain(binds: &[Bind], entry: &KeymapEntry) -> bool {
    binds
        .iter()
        .any(|b| b.code == entry.key && b.modifiers == entry.modifiers)
}

/// Description for a built-in row. The first override whose key matches one of
/// `visible` wins; an override on a sibling alias bind that the row does not
/// display (e.g. the trailing half of `MacAlt` off-mac) cannot win here and is
/// left for the Plugin bindings section. If the winning override's sanitized
/// desc is empty, falls through to the next match or the default.
fn row_description<'a>(
    kb: &'a Keybind,
    visible: &[Bind],
    overrides: &[KeymapEntry],
) -> Cow<'a, str> {
    for e in overrides {
        if !e.desc.is_empty() && binds_contain(visible, e) {
            let sanitized = sanitize_desc(&e.desc);
            if !sanitized.is_empty() {
                return Cow::Owned(sanitized);
            }
        }
    }
    Cow::Borrowed(kb.description)
}

fn sanitize_desc(desc: &str) -> String {
    let mut out: String = desc
        .chars()
        .take(MAX_DESC_SCAN)
        .filter(|&c| !c.is_control() && !is_format_char(c))
        .take(MAX_DESC_CHARS)
        .collect();
    let width = UnicodeWidthStr::width(out.as_str());
    if width > MAX_DESC_WIDTH {
        let mut end = 0;
        let mut w = 0;
        for (i, c) in out.char_indices() {
            let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
            if w + cw > MAX_DESC_WIDTH {
                break;
            }
            w += cw;
            end = i + c.len_utf8();
        }
        out.truncate(end);
        out.push('…');
    }
    out
}

/// Bidi overrides, joiners, and other formatting characters slip past
/// `is_control` but still reorder or hide text. Strip them so a plugin
/// cannot spoof or invert its description.
fn is_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}' // soft hyphen
        | '\u{061C}' // arabic letter mark (bidi)
        | '\u{115F}' | '\u{1160}' // hangul fillers
        | '\u{200B}' // zero-width space
        | '\u{200C}' // zero-width non-joiner
        | '\u{200D}' // zero-width joiner
        | '\u{200E}' | '\u{200F}' // left/right-to-left/right mark
        | '\u{202A}'..='\u{202E}' // bidi embedding/override
        | '\u{2060}' | '\u{2061}' | '\u{2062}' | '\u{2063}' | '\u{2064}' // word/func joiners
        | '\u{2066}'..='\u{2069}' // bidi isolate
        | '\u{3164}' // hangul filler
        | '\u{FE00}'..='\u{FE0F}' // variation selectors
        | '\u{FEFF}' // zero-width no-break space
        | '\u{FFF9}' | '\u{FFFA}' | '\u{FFFB}' // interlinear annotation
        | '\u{FFA0}' // halfwidth hangul filler
        | '\u{E0001}' | '\u{E0020}'..='\u{E007F}' // tag chars
        | '\u{E0100}'..='\u{E01EF}' // variation selectors supplement
    )
}

impl HelpModal {
    pub fn new() -> Self {
        Self {
            open: false,
            scroll: ModalScroll::new_top(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
        self.scroll.reset();
    }

    pub fn close(&mut self) {
        self.open = false;
        self.scroll.reset();
    }

    pub fn scroll(&mut self, delta: i32) {
        self.scroll.scroll(delta);
    }

    pub fn handle_key(&mut self, key_event: KeyEvent) -> bool {
        let close = key_event.code == KeyCode::Esc
            || key::HELP.matches(key_event)
            || key::QUIT.matches(key_event);
        if close {
            self.close();
            return true;
        }
        self.scroll.handle_key(key_event);
        true
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect, overrides: &[KeymapEntry]) -> Rect {
        if !self.open {
            return Rect::default();
        }

        // Resolve each visible row once per draw, so the per-override matching
        // below pays one `resolved_label` allocation per row instead of one
        // per (row, override).
        let rows: Vec<ResolvedRow> = KEYBINDS
            .iter()
            .filter(|kb| kb.platform.is_visible())
            .map(|kb| {
                let label = kb.resolved_label();
                let n = label.visible_count().min(kb.binds.len());
                ResolvedRow {
                    kb,
                    label,
                    visible: &kb.binds[..n],
                }
            })
            .collect();

        let mut lines: Vec<Line> = Vec::new();
        let theme = theme::current();

        let key_col_width = rows
            .iter()
            .map(|r| r.label.display_width())
            .chain(
                overrides
                    .iter()
                    .filter(|e| !e.desc.is_empty())
                    .map(|e| UnicodeWidthStr::width(format_key(e.key, e.modifiers).as_str())),
            )
            .max()
            .unwrap_or(0)
            + KEY_COL_GAP;

        let mut first = true;
        for ctx in all_contexts() {
            if ctx.parent().is_some() {
                continue;
            }
            if !first {
                lines.push(Line::default());
            }
            first = false;

            lines.push(Line::from(Span::styled(
                format!("  {}", ctx.label()),
                theme.keybind_section,
            )));

            for r in rows.iter().filter(|r| r.kb.context == ctx) {
                let desc = row_description(r.kb, r.visible, overrides);
                let mut spans = key_spans(&r.label, key_col_width, PREFIX_TOP);
                spans.push(Span::styled(desc, theme.keybind_desc));
                lines.push(Line::from(spans));
            }

            for child in all_contexts() {
                if child.parent() != Some(ctx) {
                    continue;
                }
                let child_rows: Vec<&ResolvedRow> =
                    rows.iter().filter(|r| r.kb.context == child).collect();
                if child_rows.is_empty() {
                    continue;
                }
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    format!("    {}", child.label()),
                    theme.keybind_section,
                )));
                for r in child_rows {
                    let desc = row_description(r.kb, r.visible, overrides);
                    let mut spans = key_spans(&r.label, key_col_width - KEY_COL_GAP, PREFIX_CHILD);
                    spans.push(Span::styled(desc, theme.keybind_desc));
                    lines.push(Line::from(spans));
                }
            }

            if ctx == KeybindContext::Editing {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "    Input Prefixes",
                    theme.keybind_section,
                )));
                for &(pfx, desc) in INPUT_PREFIXES {
                    let label = ResolvedLabel::Single(pfx.to_string());
                    let mut spans = key_spans(&label, key_col_width - KEY_COL_GAP, PREFIX_CHILD);
                    spans.push(Span::styled(desc, theme.keybind_desc));
                    lines.push(Line::from(spans));
                }
            }
        }

        let unmatched: Vec<&KeymapEntry> = overrides
            .iter()
            .filter(|e| !e.desc.is_empty() && !rows.iter().any(|r| binds_contain(r.visible, e)))
            .collect();
        if !unmatched.is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "  Plugin bindings",
                theme.keybind_section,
            )));
            for entry in &unmatched {
                let key_str = format_key(entry.key, entry.modifiers);
                let key_w = UnicodeWidthStr::width(key_str.as_str());
                let trailing = key_col_width.saturating_sub(key_w);
                let label = format!("{PREFIX_TOP}{key_str}{:trailing$}", "");
                let mut spans = vec![Span::styled(label, theme.keybind_key)];
                spans.push(Span::styled(sanitize_desc(&entry.desc), theme.keybind_desc));
                lines.push(Line::from(spans));
            }
        }

        let total = lines.len() as u16;
        let modal = Modal {
            title: TITLE,
            width_percent: 50,
            max_height_percent: 80,
        };
        let (popup, inner) = modal.render(frame, area, total);
        let viewport_h = inner.height;
        self.scroll.update_dimensions(total, viewport_h);
        let scroll = self.scroll.offset();

        let paragraph = Paragraph::new(lines).scroll((scroll, 0));
        frame.render_widget(paragraph, inner);

        if total > viewport_h {
            render_vertical_scrollbar(frame, inner, total, scroll);
        }

        popup
    }
}

impl Overlay for HelpModal {
    fn is_open(&self) -> bool {
        self.is_open()
    }

    fn close(&mut self) {
        self.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::components::key as key_ev;
    use crossterm::event::KeyCode;
    use crossterm::event::KeyModifiers;
    use test_case::test_case;

    #[test_case(key_ev(KeyCode::Esc)       ; "esc_closes")]
    #[test_case(key::QUIT.to_key_event()    ; "ctrl_c_closes")]
    #[test_case(key::HELP.to_key_event()    ; "ctrl_h_closes")]
    fn handle_key_closes(k: KeyEvent) {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(k));
        assert!(!modal.is_open());
    }

    #[test]
    fn handle_key_consumes_all() {
        let mut modal = HelpModal::new();
        modal.toggle();
        assert!(modal.handle_key(key_ev(KeyCode::Char('a'))));
        assert!(modal.is_open());
    }

    fn render_modal(overrides: &[KeymapEntry]) -> ratatui::Terminal<ratatui::backend::TestBackend> {
        let mut modal = HelpModal::new();
        modal.toggle();
        let backend = ratatui::backend::TestBackend::new(140, 120);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                modal.view(f, f.area(), overrides);
            })
            .unwrap();
        terminal
    }

    fn buffer_text(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        buf.content.iter().map(|c| c.symbol()).collect::<String>()
    }

    #[test]
    fn override_desc_replaces_default_on_matching_row() {
        let entry = KeymapEntry {
            key: key::HELP.code,
            modifiers: key::HELP.modifiers,
            desc: "plugin help override".into(),
            plugin: std::sync::Arc::from("p"),
            id: 1,
        };
        let terminal = render_modal(&[entry]);
        let text = buffer_text(&terminal);
        assert!(
            text.contains("plugin help override"),
            "override desc must appear in help"
        );
        assert!(
            !text.contains("Show keybindings"),
            "default desc must leave the row when an override replaces it"
        );
    }

    #[test]
    fn default_desc_shown_when_no_override() {
        let terminal = render_modal(&[]);
        let text = buffer_text(&terminal);
        assert!(text.contains("Show keybindings"));
    }

    #[test]
    fn plugin_binding_without_default_row() {
        let entry = KeymapEntry {
            key: KeyCode::F(7),
            modifiers: KeyModifiers::NONE,
            desc: "run my tool".into(),
            plugin: std::sync::Arc::from("p"),
            id: 2,
        };
        let terminal = render_modal(&[entry]);
        let text = buffer_text(&terminal);
        assert!(text.contains("Plugin bindings"));
        assert!(text.contains("run my tool"));
        assert!(text.contains("F7"));
    }

    #[test]
    fn override_on_alias_matches_shared_row() {
        let entry = KeymapEntry {
            key: key::NEXT_CHAT.code,
            modifiers: key::NEXT_CHAT.modifiers,
            desc: "alt chat override".into(),
            plugin: std::sync::Arc::from("p"),
            id: 3,
        };
        let terminal = render_modal(&[entry]);
        let text = buffer_text(&terminal);
        assert!(text.contains("alt chat override"));
        assert!(
            !text.contains("Plugin bindings"),
            "matched override must not be listed again in Plugin bindings"
        );
    }

    #[test]
    fn first_alias_override_wins_and_swallows_second() {
        let first = KeymapEntry {
            key: key::NEXT_CHAT.code,
            modifiers: key::NEXT_CHAT.modifiers,
            desc: "first override".into(),
            plugin: std::sync::Arc::from("p"),
            id: 10,
        };
        let second = KeymapEntry {
            key: key::PREV_CHAT.code,
            modifiers: key::PREV_CHAT.modifiers,
            desc: "second override".into(),
            plugin: std::sync::Arc::from("p"),
            id: 11,
        };
        let terminal = render_modal(&[first, second]);
        let text = buffer_text(&terminal);
        assert!(
            text.contains("first override"),
            "first alias override wins the shared row"
        );
        assert!(
            !text.contains("second override"),
            "later alias override is not surfaced (first wins, shared-row match excludes it from Plugin bindings)"
        );
        assert!(
            !text.contains("Plugin bindings"),
            "matched alias row swallows both overrides, no Plugin bindings section"
        );
    }

    #[test]
    fn desc_sanitized_and_truncated() {
        let long = format!("A\tB{}", "x".repeat(MAX_DESC_WIDTH + 20));
        let out = sanitize_desc(&long);
        assert!(!out.contains('\t'), "control chars stripped");
        assert!(
            unicode_width::UnicodeWidthStr::width(out.as_str()) <= MAX_DESC_WIDTH + 1,
            "desc truncated within bound + ellipsis"
        );
        assert!(out.ends_with('…'));
    }

    #[test]
    fn sanitize_strips_bidi_and_zero_width_chars() {
        // U+202E reverses text; U+0301 is a combining mark (zero-width).
        let malicious = "\u{202E}normal\u{202C}\u{200B}\u{0301}";
        let out = sanitize_desc(malicious);
        assert!(!out.contains('\u{202E}'), "bidi override stripped");
        assert!(!out.contains('\u{200B}'), "zero-width space stripped");
        assert!(out.contains("normal"));
    }

    #[test]
    fn sanitize_truncates_cjk_at_width_boundary() {
        // Each CJK char is width 2; one over MAX_DESC_WIDTH should be truncated.
        let cjk = "中".repeat(MAX_DESC_WIDTH / 2 + 1);
        let out = sanitize_desc(&cjk);
        assert!(
            unicode_width::UnicodeWidthStr::width(out.as_str()) <= MAX_DESC_WIDTH + 1,
            "CJK desc truncated within width bound"
        );
        assert!(out.ends_with('…'));
    }

    #[test]
    fn sanitize_caps_zero_width_flood() {
        // Tens of thousands of combining marks must not grow the output.
        let flood = format!("A{}", "\u{0301}".repeat(MAX_DESC_CHARS * 4));
        let out = sanitize_desc(&flood);
        assert!(
            out.chars().count() <= MAX_DESC_CHARS + 1,
            "zero-width flood bounded by char cap"
        );
    }

    #[test]
    fn empty_desc_override_is_invisible_everywhere() {
        let entry = KeymapEntry {
            key: key::HELP.code,
            modifiers: key::HELP.modifiers,
            desc: String::new(),
            plugin: std::sync::Arc::from("p"),
            id: 4,
        };
        let terminal = render_modal(&[entry]);
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Show keybindings"),
            "empty-desc override falls back to default on the matching row"
        );
        assert!(
            !text.contains("Plugin bindings"),
            "empty-desc override does not appear in Plugin bindings"
        );
    }

    #[test]
    fn sanitized_to_empty_override_falls_back_to_default() {
        // A desc made only of bidi/format chars is non-empty raw but sanitizes
        // to empty. The row must keep its default description instead of going
        // blank, and the override must not surface in Plugin bindings.
        let entry = KeymapEntry {
            key: key::HELP.code,
            modifiers: key::HELP.modifiers,
            desc: "\u{202E}\u{200B}\u{FEFF}".into(),
            plugin: std::sync::Arc::from("p"),
            id: 5,
        };
        let terminal = render_modal(&[entry]);
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Show keybindings"),
            "sanitizes-to-empty override falls back to default on the matching row"
        );
        assert!(
            !text.contains("Plugin bindings"),
            "sanitizes-to-empty override does not surface as a Plugin binding"
        );
    }

    #[test_case(KeyCode::F(7), KeyModifiers::NONE, "F7" ; "f_key_bare")]
    #[test_case(KeyCode::Char('c'), KeyModifiers::CONTROL, "Ctrl+C" ; "ctrl_char")]
    #[test_case(KeyCode::Char(' '), KeyModifiers::NONE, "Space" ; "space")]
    #[test_case(KeyCode::BackTab, KeyModifiers::NONE, "Shift+Tab" ; "backtab_adds_shift")]
    #[test_case(KeyCode::Tab, KeyModifiers::SHIFT, "Shift+Tab" ; "shift_tab")]
    #[test_case(KeyCode::F(1), KeyModifiers::CONTROL | KeyModifiers::SHIFT, "Ctrl+Shift+F1" ; "ctrl_shift_f")]
    #[test_case(KeyCode::Char('1'), KeyModifiers::SHIFT, "!" ; "shift_one_becomes_bang")]
    #[test_case(KeyCode::Char('2'), KeyModifiers::SHIFT, "@" ; "shift_two_becomes_at")]
    #[test_case(KeyCode::Char('3'), KeyModifiers::SHIFT, "#" ; "shift_three_becomes_hash")]
    #[test_case(KeyCode::Char('4'), KeyModifiers::SHIFT, "$" ; "shift_four_becomes_dollar")]
    #[test_case(KeyCode::Char('a'), KeyModifiers::SHIFT, "A" ; "shift_letter_uppercases")]
    fn format_key_cases(code: KeyCode, mods: KeyModifiers, expected: &str) {
        assert_eq!(format_key(code, mods), expected);
    }
}
