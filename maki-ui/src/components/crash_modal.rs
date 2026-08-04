use crate::components::ModalScroll;
use crate::components::keybindings::key;
use crate::components::modal::Modal;
use crate::components::scrollbar::render_vertical_scrollbar;
use crate::theme;

use crossterm::event::{KeyCode, KeyEvent};
use maki_lua::CrashInfo;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

const TITLE: &str = "Lua runtime crashed";
const PANIC_PREFIX: &str = "panic: ";
const RELOAD_HINT: &str = "R";
const RELOAD_NO_PLUGINS_HINT: &str = "P";
const QUIT_HINT: &str = "Ctrl+C";
const ESC_HINT: &str = "Esc";
const UP_DOWN_HINT: &str = "↑/↓";

/// What the focused crash modal asked for. The App translates each variant
/// into the matching `quit_with` call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashAction {
    Quit,
    Reload,
    ReloadWithoutPlugins,
}

pub struct CrashModal {
    open: bool,
    info: Option<CrashInfo>,
    no_plugins: bool,
    scroll: ModalScroll,
}

impl CrashModal {
    pub fn new() -> Self {
        Self {
            open: false,
            info: None,
            no_plugins: false,
            scroll: ModalScroll::new_top(),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    pub fn open(&mut self, info: CrashInfo, no_plugins: bool) {
        self.info = Some(info);
        self.no_plugins = no_plugins;
        self.scroll.reset();
        self.open = true;
    }

    pub fn close(&mut self) {
        self.open = false;
        self.info = None;
        self.scroll.reset();
    }

    #[cfg(test)]
    pub(crate) fn info(&self) -> Option<&CrashInfo> {
        self.info.as_ref()
    }

    pub fn scroll(&mut self, delta: i32) {
        if self.open {
            self.scroll.scroll(delta);
        }
    }

    /// Only hardscape keys are accepted while the modal is open, so a stray
    /// Lua-backed keybind can't dispatch into a half-dead App. Q/Ctrl+C
    /// quits; R reloads; P reloads without plugins (hidden when already
    /// running that way); scroll keys move the traceback.
    pub fn handle_key(&mut self, key_event: KeyEvent) -> Option<CrashAction> {
        if !self.open {
            return None;
        }
        if key_event.code == KeyCode::Esc || key::QUIT.matches(key_event) {
            self.close();
            return Some(CrashAction::Quit);
        }
        if let KeyCode::Char(c) = key_event.code {
            if c == 'r' || c == 'R' {
                self.close();
                return Some(CrashAction::Reload);
            }
            if (c == 'p' || c == 'P') && !self.no_plugins {
                self.close();
                return Some(CrashAction::ReloadWithoutPlugins);
            }
        }
        self.scroll.handle_key(key_event);
        None
    }

    pub fn view(&mut self, frame: &mut Frame, area: Rect) -> Rect {
        if !self.open {
            return Rect::default();
        }
        let Some(info) = self.info.as_ref() else {
            return Rect::default();
        };
        let theme = theme::current();
        let mut lines: Vec<Line> = Vec::new();

        lines.push(Line::from(vec![
            Span::styled(PANIC_PREFIX, theme.error),
            Span::styled(info.message.clone(), theme.tool_dim),
        ]));
        if let Some(loc) = &info.location {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(loc.clone(), theme.tool_dim)));
        }
        if let Some(traceback) = &info.traceback {
            lines.push(Line::default());
            for raw in traceback.lines() {
                lines.push(Line::from(Span::styled(raw.to_owned(), theme.tool_dim)));
            }
        }

        lines.push(Line::default());
        let mut hints: Vec<(&str, &str)> = vec![
            (RELOAD_HINT, "reload"),
            (QUIT_HINT, "quit"),
            (UP_DOWN_HINT, "scroll"),
            (ESC_HINT, "quit"),
        ];
        if !self.no_plugins {
            hints.insert(1, (RELOAD_NO_PLUGINS_HINT, "reload without plugins"));
        }
        lines.push(crate::components::hint_line(&hints));

        let total = lines.len() as u16;
        let modal = Modal {
            title: TITLE,
            width_percent: 60,
            max_height_percent: 80,
        };
        let (popup, inner) = modal.render(frame, area, total);
        let viewport_h = inner.height;
        self.scroll.update_dimensions(total, viewport_h);
        let scroll = self.scroll.offset();

        frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), inner);

        if total > viewport_h {
            render_vertical_scrollbar(frame, inner, total, scroll);
        }

        popup
    }
}

impl crate::components::Overlay for CrashModal {
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
    use crossterm::event::{KeyCode, KeyModifiers};
    use test_case::test_case;

    const PANIC_MSG: &str = "boom";
    const TRACE_LINE: &str = "stack backtrace:";

    fn info(panicked: bool) -> CrashInfo {
        CrashInfo {
            message: PANIC_MSG.to_owned(),
            location: Some("runtime.rs:1".to_owned()),
            traceback: Some(format!("{TRACE_LINE}\n0: 0x1\n1: 0x2")),
            panicked,
        }
    }

    fn open_modal(no_plugins: bool) -> CrashModal {
        let mut modal = CrashModal::new();
        modal.open(info(true), no_plugins);
        modal
    }

    #[test_case(KeyCode::Char('r')         ; "r_reloads")]
    #[test_case(KeyCode::Char('R')         ; "uppercase_r_reloads")]
    fn reload_key_returns_reload(code: KeyCode) {
        let mut modal = open_modal(false);
        assert_eq!(
            modal.handle_key(KeyEvent::new(code, KeyModifiers::NONE)),
            Some(CrashAction::Reload)
        );
        assert!(!modal.is_open());
    }

    #[test]
    fn p_reloads_without_plugins_when_shown() {
        let mut modal = open_modal(false);
        assert_eq!(
            modal.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
            Some(CrashAction::ReloadWithoutPlugins)
        );
    }

    #[test]
    fn p_hidden_when_no_plugins_already_true() {
        let mut modal = open_modal(true);
        assert_eq!(
            modal.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE)),
            None
        );
        assert!(modal.is_open());
    }

    #[test_case(key::QUIT.to_key_event()                          ; "ctrl_c_quits")]
    #[test_case(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)    ; "esc_quits")]
    fn quit_keys_return_quit(key_event: KeyEvent) {
        let mut modal = open_modal(false);
        assert_eq!(modal.handle_key(key_event), Some(CrashAction::Quit));
        assert!(!modal.is_open());
    }

    #[test]
    fn scroll_keys_do_not_dispatch_action() {
        let mut modal = open_modal(false);
        assert_eq!(
            modal.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
            None
        );
        assert!(modal.is_open());
    }

    #[test]
    fn handle_key_returns_none_when_closed() {
        let mut modal = CrashModal::new();
        assert_eq!(
            modal.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn close_clears_info_and_resets_scroll() {
        let mut modal = open_modal(false);
        modal
            .scroll
            .handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        modal.close();
        assert!(modal.info.is_none());
        assert_eq!(modal.scroll.offset(), 0);
    }
}
