use crate::components::ModalScroll;
use crate::components::Overlay;
use crate::components::keybindings::{
    ALT_SEP, KEYBINDS, ResolvedLabel, display_key_label, entry_in_identity, entry_in_kind, key,
    section_title,
};
use crate::components::modal::Modal;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use maki_lua::{ContextKind, ContextRef, IDENTITIES, KeymapSnapshot};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

const TITLE: &str = " Keybindings ";
const KEY_COL_GAP: usize = 2;
const PREFIX_TOP: &str = "  ";
const PREFIX_CHILD: &str = "    ";

const INPUT_PREFIXES: &[(&str, &str)] = &[
    ("!", "Run shell command (visible to agent)"),
    ("!!", "Run shell command (hidden from agent)"),
];

/// Escape hatches hardcoded at the top of `App::handle_key`, above the
/// keymap: not remappable, not routable, listed as their own section.
const FIXED_KEYS: &[(&[&str], &str)] = &[
    (&["Ctrl+Z"], "Suspend process (Unix only)"),
    (&["Ctrl+C", "Esc"], "Stop streaming"),
];

pub struct HelpModal {
    open: bool,
    scroll: ModalScroll,
}

fn key_parts_width(parts: &[String]) -> usize {
    let sep_w = UnicodeWidthStr::width(ALT_SEP);
    parts
        .iter()
        .map(|p| UnicodeWidthStr::width(p.as_str()))
        .sum::<usize>()
        + sep_w * parts.len().saturating_sub(1)
}

fn row_spans(
    parts: &[String],
    pad: usize,
    prefix: &str,
    desc: &str,
    theme: &crate::theme::Theme,
) -> Vec<Span<'static>> {
    let content_w = key_parts_width(parts);
    let trailing = pad.saturating_sub(content_w);
    let mut spans = Vec::with_capacity(parts.len() * 2 + 1);
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled(ALT_SEP, theme.keybind_desc));
        }
        let text = if i == 0 && i == parts.len() - 1 {
            format!("{prefix}{p}{:trailing$}", "")
        } else if i == 0 {
            format!("{prefix}{p}")
        } else if i == parts.len() - 1 {
            format!("{p}{:trailing$}", "")
        } else {
            p.clone()
        };
        spans.push(Span::styled(text, theme.keybind_key));
    }
    spans.push(Span::styled(desc.to_string(), theme.keybind_desc));
    spans
}

/// Default bindings from the loaded keymap snapshot (Lua), rendered for
/// one section: a kind (General entries carry no context refs) or an
/// identity. `Callback` rows flip to `(unavailable)` when `lua_alive` is
/// false (dead runtime); `Builtin` rows never depend on the host.
fn snapshot_rows(
    snap: &KeymapSnapshot,
    ctx: ContextRef,
    lua_alive: bool,
) -> Vec<(Vec<String>, String, bool)> {
    snap.entries
        .iter()
        .filter(|e| match ctx {
            ContextRef::Kind(kind) => entry_in_kind(e, kind),
            ContextRef::Identity(id) => entry_in_identity(e, id),
        })
        .map(|e| {
            let label = display_key_label(e.key, e.modifiers);
            let (desc, available) = match &e.kind {
                maki_lua::EntryKind::Builtin(a) => (a.description().to_string(), true),
                maki_lua::EntryKind::Callback if e.desc.is_empty() => {
                    (format!("[{}] callback", e.plugin), lua_alive)
                }
                maki_lua::EntryKind::Callback => (format!("{} ({})", e.desc, e.plugin), lua_alive),
            };
            let desc = if available {
                desc
            } else {
                format!("{desc} (unavailable)")
            };
            (vec![label], desc, available)
        })
        .collect()
}

/// Hardcoded widget/component keys from the trimmed `KEYBINDS` table.
fn hardcoded_rows(ctx: ContextRef) -> Vec<(Vec<String>, String, bool)> {
    KEYBINDS
        .iter()
        .filter(|kb| kb.context == ctx && kb.platform.is_visible())
        .map(|kb| {
            (
                resolved_parts(kb.label.resolve()),
                kb.description.to_string(),
                true,
            )
        })
        .collect()
}

fn resolved_parts(label: ResolvedLabel) -> Vec<String> {
    match label {
        ResolvedLabel::Single(s) => vec![s.to_string()],
        ResolvedLabel::Alt(a, b) => vec![a.to_string(), b.to_string()],
        ResolvedLabel::Multi(keys) => keys.iter().map(|k| k.to_string()).collect(),
    }
}

fn max_parts_w(rows: &[(Vec<String>, String, bool)]) -> usize {
    rows.iter()
        .map(|(parts, _, _)| key_parts_width(parts))
        .max()
        .unwrap_or(0)
}

fn merged_rows(
    snapshot: &KeymapSnapshot,
    ctx: ContextRef,
    lua_alive: bool,
) -> Vec<(Vec<String>, String, bool)> {
    let mut rows = snapshot_rows(snapshot, ctx, lua_alive);
    rows.extend(hardcoded_rows(ctx));
    rows
}

fn fixed_rows() -> Vec<(Vec<String>, String, bool)> {
    FIXED_KEYS
        .iter()
        .map(|(keys, desc)| {
            (
                keys.iter().map(|k| k.to_string()).collect(),
                desc.to_string(),
                true,
            )
        })
        .collect()
}

fn emit_rows(
    rows: &[(Vec<String>, String, bool)],
    col_width: usize,
    prefix: &str,
    theme: &crate::theme::Theme,
    lines: &mut Vec<Line<'static>>,
) {
    for (parts, desc, available) in rows {
        let mut spans = row_spans(parts, col_width, prefix, desc, theme);
        if !available {
            for s in &mut spans {
                s.style = s.style.patch(theme.keybind_unavailable);
            }
        }
        lines.push(Line::from(spans));
    }
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

    pub fn view(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        snapshot: &KeymapSnapshot,
        lua_alive: bool,
    ) -> Rect {
        if !self.open {
            return Rect::default();
        }

        let theme = theme::current();

        let col_width = ContextKind::ALL
            .iter()
            .flat_map(|kind| {
                let ctx = ContextRef::Kind(*kind);
                let snap = snapshot_rows(snapshot, ctx, lua_alive);
                let hard = hardcoded_rows(ctx);
                [max_parts_w(&snap), max_parts_w(&hard)]
            })
            .chain([max_parts_w(&fixed_rows())])
            .max()
            .unwrap_or(0)
            + KEY_COL_GAP
            + UnicodeWidthStr::width(PREFIX_TOP);

        let mut lines: Vec<Line> = Vec::new();
        let mut first = true;
        for kind in ContextKind::ALL {
            let kind_rows = merged_rows(snapshot, ContextRef::Kind(kind), lua_alive);
            let identity_rows: Vec<_> = IDENTITIES
                .iter()
                .filter(|i| i.kind == kind)
                .map(|identity| {
                    (
                        identity,
                        merged_rows(snapshot, ContextRef::Identity(identity.id), lua_alive),
                    )
                })
                .filter(|(_, rows)| !rows.is_empty())
                .collect();
            if kind_rows.is_empty() && identity_rows.is_empty() {
                continue;
            }
            if !first {
                lines.push(Line::default());
            }
            first = false;
            lines.push(Line::from(Span::styled(
                format!("  {}", section_title(kind)),
                theme.keybind_section,
            )));
            emit_rows(&kind_rows, col_width, PREFIX_TOP, &theme, &mut lines);

            for (identity, rows) in identity_rows {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    format!("    {}", identity.name),
                    theme.keybind_section,
                )));
                emit_rows(&rows, col_width, PREFIX_CHILD, &theme, &mut lines);
            }

            if kind == ContextKind::Chat {
                lines.push(Line::default());
                lines.push(Line::from(Span::styled(
                    "    Input Prefixes",
                    theme.keybind_section,
                )));
                for &(pfx, desc) in INPUT_PREFIXES {
                    let spans = row_spans(
                        &[pfx.to_string()],
                        col_width - UnicodeWidthStr::width(PREFIX_TOP),
                        PREFIX_CHILD,
                        desc,
                        &theme,
                    );
                    lines.push(Line::from(spans));
                }
            }
        }

        let fixed = fixed_rows();
        if !first {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled("  Fixed", theme.keybind_section)));
        emit_rows(&fixed, col_width, PREFIX_TOP, &theme, &mut lines);

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
    use crossterm::event::{KeyCode, KeyModifiers};
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

    fn app_snapshot(entries: Vec<maki_lua::KeymapEntry>) -> maki_lua::KeymapSnapshot {
        maki_lua::KeymapSnapshot {
            entries,
            generation: 1,
        }
    }

    #[test]
    fn snapshot_rows_marks_callback_unavailable_when_host_dead() {
        let snap = app_snapshot(vec![
            maki_lua::KeymapEntry::callback(
                KeyCode::Char('q'),
                KeyModifiers::NONE,
                std::sync::Arc::from("p"),
                "quit",
                7,
            ),
            maki_lua::KeymapEntry::builtin(
                KeyCode::Char('h'),
                KeyModifiers::CONTROL,
                std::sync::Arc::from("p"),
                "help",
                9,
                Vec::new(),
                maki_lua::BuiltinAction::Help,
            ),
        ]);
        let rows = snapshot_rows(&snap, ContextRef::Kind(ContextKind::General), false);
        assert_eq!(rows.len(), 2);
        let callback_row = rows
            .iter()
            .find(|(_, _, avail)| !avail)
            .expect("dead host must mark callback unavailable");
        assert!(
            callback_row.1.contains("(unavailable)"),
            "missing unavailable suffix, got: {}",
            callback_row.1
        );
        let builtin_row = rows
            .iter()
            .find(|(_, _, avail)| *avail)
            .expect("builtin must stay available");
        assert!(
            !builtin_row.1.contains("(unavailable)"),
            "builtin should not be marked unavailable, got: {}",
            builtin_row.1
        );
    }

    #[test]
    fn snapshot_rows_marks_callback_available_when_host_alive() {
        let snap = app_snapshot(vec![maki_lua::KeymapEntry::callback(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            std::sync::Arc::from("p"),
            "quit",
            7,
        )]);
        let rows = snapshot_rows(&snap, ContextRef::Kind(ContextKind::General), true);
        assert_eq!(rows.len(), 1);
        let (_, desc, avail) = &rows[0];
        assert!(avail);
        assert!(
            !desc.contains("(unavailable)"),
            "alive host should not suffix, got: {desc}"
        );
    }
}
