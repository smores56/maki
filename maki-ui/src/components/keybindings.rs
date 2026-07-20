use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fmt::Write;
use strum::EnumIter;
use unicode_width::UnicodeWidthStr;

macro_rules! ctrl_bind {
    ($char:tt) => {
        Bind {
            code: KeyCode::Char($char),
            modifiers: KeyModifiers::CONTROL,
        }
    };
}

macro_rules! bind {
    (KeyCode::Char($c:tt), KeyModifiers :: $mods:ident) => {
        Bind {
            code: KeyCode::Char($c),
            modifiers: KeyModifiers::$mods,
        }
    };
    (KeyCode::$key:ident, KeyModifiers :: $mods:ident) => {
        Bind {
            code: KeyCode::$key,
            modifiers: KeyModifiers::$mods,
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl Bind {
    pub fn matches(&self, key: KeyEvent) -> bool {
        key.code == self.code && key.modifiers == self.modifiers
    }

    #[cfg(test)]
    pub const fn to_key_event(self) -> KeyEvent {
        KeyEvent {
            code: self.code,
            modifiers: self.modifiers,
            kind: crossterm::event::KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }
    }
}

pub mod key {
    use super::Bind;
    use crossterm::event::{KeyCode, KeyModifiers};

    pub const QUIT: Bind = ctrl_bind!('c');
    pub const HELP: Bind = ctrl_bind!('h');
    pub const PREV_CHAT: Bind = ctrl_bind!('p');
    pub const NEXT_CHAT: Bind = ctrl_bind!('n');
    pub const SCROLL_HALF_UP: Bind = ctrl_bind!('u');
    pub const SCROLL_HALF_DOWN: Bind = ctrl_bind!('d');
    pub const SCROLL_LINE_UP: Bind = ctrl_bind!('y');
    pub const SCROLL_LINE_DOWN: Bind = ctrl_bind!('e');
    pub const SCROLL_TOP: Bind = ctrl_bind!('g');
    pub const SCROLL_BOTTOM: Bind = ctrl_bind!('b');
    pub const POP_QUEUE: Bind = ctrl_bind!('q');
    pub const DELETE_WORD: Bind = ctrl_bind!('w');
    pub const SEARCH: Bind = ctrl_bind!('f');
    pub const FILE_PICKER: Bind = ctrl_bind!('s');
    pub const OPEN_EDITOR: Bind = ctrl_bind!('o');
    pub const PLAN_TOGGLE: Bind = ctrl_bind!('t');
    pub const TASKS: Bind = ctrl_bind!('x');
    pub const REFRESH: Bind = ctrl_bind!('r');
    pub const SUSPEND: Bind = ctrl_bind!('z');
    pub const DELETE: Bind = ctrl_bind!('d');
    pub const KILL_LINE: Bind = ctrl_bind!('k');
    pub const LINE_START: Bind = ctrl_bind!('a');
    pub const LINE_END: Bind = ctrl_bind!('e');
    pub const EDIT_INPUT: Bind = bind!(KeyCode::Char('o'), KeyModifiers::ALT);

    /// Plain (unmodified) key binds used by rows whose label is a literal string
    /// rather than a `key::X` const. Grouped here so the `binds` field on each
    /// `Keybind` row can match overrides on those keys too.
    pub const ENTER: Bind = bind!(KeyCode::Enter, KeyModifiers::NONE);
    pub const TAB: Bind = bind!(KeyCode::Tab, KeyModifiers::NONE);
    pub const ESC: Bind = bind!(KeyCode::Esc, KeyModifiers::NONE);
    pub const SLASH: Bind = bind!(KeyCode::Char('/'), KeyModifiers::NONE);
    pub const UP: Bind = bind!(KeyCode::Up, KeyModifiers::NONE);
    pub const DOWN: Bind = bind!(KeyCode::Down, KeyModifiers::NONE);
    pub const HOME: Bind = bind!(KeyCode::Home, KeyModifiers::NONE);
    pub const END: Bind = bind!(KeyCode::End, KeyModifiers::NONE);
    pub const PAGE_UP: Bind = bind!(KeyCode::PageUp, KeyModifiers::NONE);
    pub const PAGE_DOWN: Bind = bind!(KeyCode::PageDown, KeyModifiers::NONE);
    pub const ONE: Bind = bind!(KeyCode::Char('1'), KeyModifiers::NONE);
    pub const TWO: Bind = bind!(KeyCode::Char('2'), KeyModifiers::NONE);
    pub const THREE: Bind = bind!(KeyCode::Char('3'), KeyModifiers::NONE);
    pub const FOUR: Bind = bind!(KeyCode::Char('4'), KeyModifiers::NONE);
    /// Tier shortcuts: kitty-protocol reports Shift+digit as the base digit + SHIFT.
    /// Legacy terminals deliver the shifted symbol directly. `format_key`
    /// translates Shift+digit to the US keyboard shifted symbol.
    pub const SHIFT_ONE: Bind = bind!(KeyCode::Char('1'), KeyModifiers::SHIFT);
    pub const SHIFT_TWO: Bind = bind!(KeyCode::Char('2'), KeyModifiers::SHIFT);
    pub const SHIFT_THREE: Bind = bind!(KeyCode::Char('3'), KeyModifiers::SHIFT);
    pub const SHIFT_FOUR: Bind = bind!(KeyCode::Char('4'), KeyModifiers::SHIFT);
    pub const ALT_BACKSPACE: Bind = bind!(KeyCode::Backspace, KeyModifiers::ALT);
    pub const ALT_DELETE: Bind = bind!(KeyCode::Delete, KeyModifiers::ALT);
    pub const ALT_LEFT: Bind = bind!(KeyCode::Left, KeyModifiers::ALT);
    pub const ALT_RIGHT: Bind = bind!(KeyCode::Right, KeyModifiers::ALT);
    pub const SHIFT_ENTER: Bind = bind!(KeyCode::Enter, KeyModifiers::SHIFT);
    pub const CTRL_J: Bind = ctrl_bind!('j');
    pub const ALT_ENTER: Bind = bind!(KeyCode::Enter, KeyModifiers::ALT);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumIter)]
pub enum KeybindContext {
    General,
    Editing,
    Streaming,
    Picker,
    FormInput,
    TaskPicker,
    RewindPicker,
    ThemePicker,
    ModelPicker,
    QueueFocus,
    CommandPalette,
    Search,
    FilePicker,
}

impl KeybindContext {
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Editing => "Editing",
            Self::Streaming => "While Streaming",
            Self::Picker => "Pickers",
            Self::FormInput => "Form",
            Self::TaskPicker => "Task Picker",
            Self::RewindPicker => "Rewind Picker",
            Self::ThemePicker => "Theme Picker",
            Self::ModelPicker => "Model Picker",
            Self::QueueFocus => "Queue",
            Self::CommandPalette => "Commands",
            Self::Search => "Search",
            Self::FilePicker => "File Picker",
        }
    }

    pub const fn parent(self) -> Option<KeybindContext> {
        match self {
            Self::TaskPicker
            | Self::RewindPicker
            | Self::ThemePicker
            | Self::ModelPicker
            | Self::QueueFocus
            | Self::CommandPalette
            | Self::Search
            | Self::FilePicker => Some(Self::Picker),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    All,
    MacOnly,
    UnixOnly,
}

impl Platform {
    pub const fn is_visible(self) -> bool {
        match self {
            Self::All => true,
            Self::MacOnly => cfg!(target_os = "macos"),
            Self::UnixOnly => cfg!(unix),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum KeyLabel {
    /// `format_key(binds[0])`.
    Single,
    /// `format_key(binds[0]) / format_key(binds[1])`.
    Alt,
    /// `format_key(binds[*])` joined.
    Multi,
    /// Mac: `format_key(binds[0]) / mac`. Non-mac: `format_key(binds[0])`.
    MacAlt(&'static str),
    /// Mac: `mac`. Non-mac: `format_key(binds[*])`.
    MacMulti(&'static [&'static str]),
    /// Explicit label, no single-bind correspondence (e.g. "/command", "Esc Esc").
    Display(&'static str),
}

pub const ALT_SEP: &str = " / ";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedLabel {
    Single(String),
    Alt(String, String),
    Multi(Vec<String>),
}

impl ResolvedLabel {
    pub fn display_width(&self) -> usize {
        match self {
            Self::Single(s) => UnicodeWidthStr::width(s.as_str()),
            Self::Alt(a, b) => {
                let sep_w = UnicodeWidthStr::width(ALT_SEP);
                UnicodeWidthStr::width(a.as_str()) + sep_w + UnicodeWidthStr::width(b.as_str())
            }
            Self::Multi(keys) => {
                let sep_w = UnicodeWidthStr::width(ALT_SEP);
                keys.iter()
                    .map(|k| UnicodeWidthStr::width(k.as_str()))
                    .sum::<usize>()
                    + sep_w * keys.len().saturating_sub(1)
            }
        }
    }

    /// Number of distinct key labels this resolved label renders. Used to slice
    /// a row's `binds` to the binds that are actually visible on this platform:
    /// `MacAlt` shows two on mac, one elsewhere; the trailing bind must not
    /// match an override the row does not display.
    pub fn visible_count(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Alt(_, _) => 2,
            Self::Multi(keys) => keys.len(),
        }
    }
}

pub struct Keybind {
    pub label: KeyLabel,
    pub description: &'static str,
    pub context: KeybindContext,
    pub platform: Platform,
    /// Concrete single-key binds this row represents. Used to match live
    /// overrides for display in the help modal. Empty for rows that are not
    /// a single keystroke (e.g. "Type" to filter). Alias rows list every key.
    pub binds: &'static [Bind],
}

impl Keybind {
    pub fn resolved_label(&self) -> ResolvedLabel {
        let label_of = |b: &Bind| format_key(b.code, b.modifiers);
        match self.label {
            KeyLabel::Single => {
                debug_assert!(!self.binds.is_empty(), "Single requires >=1 bind");
                ResolvedLabel::Single(label_of(&self.binds[0]))
            }
            KeyLabel::Alt => {
                debug_assert!(self.binds.len() >= 2, "Alt requires >=2 binds");
                ResolvedLabel::Alt(label_of(&self.binds[0]), label_of(&self.binds[1]))
            }
            KeyLabel::Multi => {
                debug_assert!(!self.binds.is_empty(), "Multi requires >=1 bind");
                ResolvedLabel::Multi(self.binds.iter().map(label_of).collect())
            }
            KeyLabel::MacAlt(mac) => {
                debug_assert!(
                    self.binds.len() >= 2,
                    "MacAlt requires >=2 binds so the trailing mac glyph has a bind to match"
                );
                if cfg!(target_os = "macos") {
                    ResolvedLabel::Alt(label_of(&self.binds[0]), mac.to_string())
                } else {
                    ResolvedLabel::Single(label_of(&self.binds[0]))
                }
            }
            KeyLabel::MacMulti(mac) => {
                debug_assert!(
                    self.binds.len() == mac.len(),
                    "MacMulti binds must match mac labels so override matching aligns"
                );
                if cfg!(target_os = "macos") {
                    ResolvedLabel::Multi(mac.iter().map(|s| s.to_string()).collect())
                } else {
                    ResolvedLabel::Multi(self.binds.iter().map(label_of).collect())
                }
            }
            KeyLabel::Display(s) => ResolvedLabel::Single(s.to_string()),
        }
    }

    #[cfg(test)]
    fn flat_label_str(&self) -> String {
        match self.resolved_label() {
            ResolvedLabel::Single(s) => s,
            ResolvedLabel::Alt(a, b) => format!("{a}/{b}"),
            ResolvedLabel::Multi(keys) => keys.join("/"),
        }
    }
}

/// Single source of truth for displaying a key combination. Used by the help
/// modal for both built-in `Bind`s and plugin-set `KeymapEntry`s, so the two
/// paths can never drift. Allocates per call; cheap relative to render.
pub fn format_key(code: KeyCode, modifiers: KeyModifiers) -> String {
    let is_char = matches!(code, KeyCode::Char(_));
    let mut s = String::new();
    let mut want_shift = modifiers.contains(KeyModifiers::SHIFT) && !is_char;
    if modifiers.contains(KeyModifiers::CONTROL) {
        s.push_str("Ctrl+");
    }
    if modifiers.contains(KeyModifiers::ALT) {
        s.push_str("Alt+");
    }
    if matches!(code, KeyCode::BackTab) {
        want_shift = true;
    }
    if want_shift {
        s.push_str("Shift+");
    }
    match code {
        KeyCode::Char(' ') => s.push_str("Space"),
        KeyCode::Char(c) => {
            if modifiers.contains(KeyModifiers::SHIFT) {
                if let Some(shifted) = shift_symbol(c) {
                    s.push(shifted);
                } else {
                    s.push(c.to_ascii_uppercase());
                }
            } else {
                s.push(c.to_ascii_uppercase());
            }
        }
        KeyCode::Enter => s.push_str("Enter"),
        KeyCode::Esc => s.push_str("Esc"),
        KeyCode::Tab => s.push_str("Tab"),
        KeyCode::Backspace => s.push_str("Bs"),
        KeyCode::Delete => s.push_str("Del"),
        KeyCode::Up => s.push('↑'),
        KeyCode::Down => s.push('↓'),
        KeyCode::Left => s.push('←'),
        KeyCode::Right => s.push('→'),
        KeyCode::Home => s.push_str("Home"),
        KeyCode::End => s.push_str("End"),
        KeyCode::PageUp => s.push_str("PageUp"),
        KeyCode::PageDown => s.push_str("PageDown"),
        KeyCode::Insert => s.push_str("Insert"),
        KeyCode::F(n) => write!(s, "F{n}").unwrap(),
        KeyCode::Null => s.push_str("Null"),
        KeyCode::CapsLock => s.push_str("CapsLock"),
        KeyCode::ScrollLock => s.push_str("ScrollLock"),
        KeyCode::NumLock => s.push_str("NumLock"),
        KeyCode::PrintScreen => s.push_str("PrintScreen"),
        KeyCode::Pause => s.push_str("Pause"),
        KeyCode::Menu => s.push_str("Menu"),
        KeyCode::KeypadBegin => s.push_str("Keypad"),
        KeyCode::Media(_) => s.push_str("Media"),
        KeyCode::Modifier(_) => s.push_str("Modifier"),
        KeyCode::BackTab => s.push_str("Tab"),
    }
    s
}

/// US keyboard Shift+digit → shifted symbol.
fn shift_symbol(c: char) -> Option<char> {
    Some(match c {
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        _ => return None,
    })
}

pub const KEYBINDS: &[Keybind] = &[
    Keybind {
        label: KeyLabel::Single,
        description: "Quit / clear input",
        context: KeybindContext::General,
        platform: Platform::All,
        binds: &[key::QUIT],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Show keybindings",
        context: KeybindContext::General,
        platform: Platform::All,
        binds: &[key::HELP],
    },
    Keybind {
        label: KeyLabel::Alt,
        description: "Next / previous task chat",
        context: KeybindContext::General,
        platform: Platform::All,
        binds: &[key::NEXT_CHAT, key::PREV_CHAT],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Search messages",
        context: KeybindContext::General,
        platform: Platform::All,
        binds: &[key::SEARCH],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "File picker",
        context: KeybindContext::General,
        platform: Platform::All,
        binds: &[key::FILE_PICKER],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Open plan in editor",
        context: KeybindContext::General,
        platform: Platform::All,
        binds: &[key::OPEN_EDITOR],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Toggle plan panel",
        context: KeybindContext::General,
        platform: Platform::All,
        binds: &[key::PLAN_TOGGLE],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Open tasks",
        context: KeybindContext::General,
        platform: Platform::All,
        binds: &[key::TASKS],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Suspend process",
        context: KeybindContext::General,
        platform: Platform::UnixOnly,
        binds: &[key::SUSPEND],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Submit prompt",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::ENTER],
    },
    Keybind {
        label: KeyLabel::MacMulti(&["⇧↵", "⌃J", "⌥↵"]),
        description: "Newline",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::SHIFT_ENTER, key::CTRL_J, key::ALT_ENTER],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Toggle mode",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::TAB],
    },
    Keybind {
        label: KeyLabel::Display("/command"),
        description: "Open command palette",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::SLASH],
    },
    Keybind {
        label: KeyLabel::MacAlt("⌥⌫"),
        description: "Delete word backward",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::DELETE_WORD, key::ALT_BACKSPACE],
    },
    Keybind {
        label: KeyLabel::MacMulti(&["⌥←", "⌥→"]),
        description: "Move word left / right",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::ALT_LEFT, key::ALT_RIGHT],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Delete word forward",
        context: KeybindContext::Editing,
        platform: Platform::MacOnly,
        binds: &[key::ALT_DELETE],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Delete to end of line",
        context: KeybindContext::Editing,
        platform: Platform::MacOnly,
        binds: &[key::KILL_LINE],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Jump to start of line",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::LINE_START],
    },
    Keybind {
        label: KeyLabel::Alt,
        description: "Jump to start/end of line",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::HOME, key::END],
    },
    Keybind {
        label: KeyLabel::Alt,
        description: "Scroll half page up / down",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::SCROLL_HALF_UP, key::SCROLL_HALF_DOWN],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Jump to end of line",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::LINE_END],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Scroll to top",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::SCROLL_TOP],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Scroll to bottom",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::SCROLL_BOTTOM],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Pop queue",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::POP_QUEUE],
    },
    // Double-Esc rows leave `binds` empty so a single-Esc override does not relabel them.
    Keybind {
        label: KeyLabel::Display("Esc Esc"),
        description: "Rewind",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Edit input in external editor",
        context: KeybindContext::Editing,
        platform: Platform::All,
        binds: &[key::EDIT_INPUT],
    },
    Keybind {
        label: KeyLabel::Alt,
        description: "Navigate input history",
        context: KeybindContext::Streaming,
        platform: Platform::All,
        binds: &[key::UP, key::DOWN],
    },
    Keybind {
        label: KeyLabel::Display("Esc Esc"),
        description: "Cancel agent",
        context: KeybindContext::Streaming,
        platform: Platform::All,
        binds: &[],
    },
    Keybind {
        label: KeyLabel::Alt,
        description: "Navigate options",
        context: KeybindContext::FormInput,
        platform: Platform::All,
        binds: &[key::UP, key::DOWN],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Select option",
        context: KeybindContext::FormInput,
        platform: Platform::All,
        binds: &[key::ENTER],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Close",
        context: KeybindContext::FormInput,
        platform: Platform::All,
        binds: &[key::ESC],
    },
    Keybind {
        label: KeyLabel::Alt,
        description: "Navigate",
        context: KeybindContext::Picker,
        platform: Platform::All,
        binds: &[key::UP, key::DOWN],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Select",
        context: KeybindContext::Picker,
        platform: Platform::All,
        binds: &[key::ENTER],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Close",
        context: KeybindContext::Picker,
        platform: Platform::All,
        binds: &[key::ESC],
    },
    Keybind {
        label: KeyLabel::Display("Type"),
        description: "Filter",
        context: KeybindContext::Picker,
        platform: Platform::All,
        binds: &[],
    },
    Keybind {
        label: KeyLabel::Alt,
        description: "Scroll page up / down",
        context: KeybindContext::Picker,
        platform: Platform::All,
        binds: &[key::PAGE_UP, key::PAGE_DOWN],
    },
    Keybind {
        label: KeyLabel::Alt,
        description: "Scroll page up / down",
        context: KeybindContext::Picker,
        platform: Platform::All,
        binds: &[key::SCROLL_HALF_UP, key::SCROLL_HALF_DOWN],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Remove item",
        context: KeybindContext::QueueFocus,
        platform: Platform::All,
        binds: &[key::ENTER],
    },
    Keybind {
        label: KeyLabel::Single,
        description: "Complete command",
        context: KeybindContext::CommandPalette,
        platform: Platform::All,
        binds: &[key::TAB],
    },
    Keybind {
        label: KeyLabel::Multi,
        description: "Set tier (strong/medium/weak/compaction)",
        context: KeybindContext::ModelPicker,
        platform: Platform::All,
        binds: &[
            key::SHIFT_ONE,
            key::SHIFT_TWO,
            key::SHIFT_THREE,
            key::SHIFT_FOUR,
        ],
    },
];

pub fn all_contexts() -> impl Iterator<Item = KeybindContext> {
    use strum::IntoEnumIterator;
    KeybindContext::iter()
}

pub(crate) fn key_event_to_string(key: &KeyEvent) -> String {
    let mut s = String::new();
    let mods = key.modifiers;
    let is_char = matches!(key.code, KeyCode::Char(_));
    if mods.contains(KeyModifiers::CONTROL) {
        s.push_str("ctrl+");
    }
    if mods.contains(KeyModifiers::ALT) {
        s.push_str("alt+");
    }
    if mods.contains(KeyModifiers::SHIFT) && !is_char {
        s.push_str("shift+");
    }
    match key.code {
        KeyCode::Char(' ') => s.push_str("space"),
        KeyCode::Char(c) => s.push(c),
        KeyCode::Enter => s.push_str("enter"),
        KeyCode::Esc => s.push_str("esc"),
        KeyCode::Tab => s.push_str("tab"),
        KeyCode::BackTab => {
            if !s.contains("shift+") {
                s.insert_str(0, "shift+");
            }
            s.push_str("tab");
        }
        KeyCode::Backspace => s.push_str("backspace"),
        KeyCode::Delete => s.push_str("delete"),
        KeyCode::Up => s.push_str("up"),
        KeyCode::Down => s.push_str("down"),
        KeyCode::Left => s.push_str("left"),
        KeyCode::Right => s.push_str("right"),
        KeyCode::Home => s.push_str("home"),
        KeyCode::End => s.push_str("end"),
        KeyCode::PageUp => s.push_str("pageup"),
        KeyCode::PageDown => s.push_str("pagedown"),
        KeyCode::F(n) => write!(s, "f{n}").unwrap(),
        KeyCode::Insert => s.push_str("insert"),
        _ => {}
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use test_case::test_case;

    #[test_case(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL), "ctrl+d")]
    #[test_case(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::ALT), "alt+x")]
    #[test_case(KeyEvent::new(KeyCode::Tab, KeyModifiers::SHIFT), "shift+tab")]
    #[test_case(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), "shift+tab")]
    #[test_case(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE), "space")]
    #[test_case(KeyEvent::new(KeyCode::F(5), KeyModifiers::NONE), "f5")]
    #[test_case(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE), "a")]
    fn key_event_to_string_cases(input: KeyEvent, expected: &str) {
        assert_eq!(key_event_to_string(&input), expected);
    }

    #[test]
    fn bind_requires_exact_modifiers() {
        let bind = key::OPEN_EDITOR; // Ctrl+O
        let exact = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL);
        let extra = KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        let wrong = KeyEvent::new(KeyCode::Char('o'), KeyModifiers::ALT);

        assert!(bind.matches(exact));
        assert!(!bind.matches(extra), "extra modifiers should not match");
        assert!(!bind.matches(wrong), "wrong modifier should not match");
    }

    #[test]
    fn every_context_has_at_least_one_keybind() {
        for ctx in all_contexts() {
            let has_own = KEYBINDS.iter().any(|kb| kb.context == ctx);
            let has_parent = ctx
                .parent()
                .is_some_and(|p| KEYBINDS.iter().any(|kb| kb.context == p));
            assert!(
                has_own || has_parent,
                "context {:?} has no keybinds and no parent with keybinds",
                ctx,
            );
        }
    }

    #[test]
    fn no_duplicate_entries() {
        for (i, a) in KEYBINDS.iter().enumerate() {
            for (j, b) in KEYBINDS.iter().enumerate() {
                if i != j && a.context == b.context {
                    assert!(
                        a.flat_label_str() != b.flat_label_str() || a.description != b.description,
                        "duplicate keybind: {} - {} in {:?}",
                        a.flat_label_str(),
                        a.description,
                        a.context,
                    );
                }
            }
        }
    }

    #[test]
    fn single_label_derives_from_first_bind() {
        let kb = KEYBINDS
            .iter()
            .find(|kb| matches!(kb.label, KeyLabel::Single))
            .unwrap();
        let b = kb.binds[0];
        let ResolvedLabel::Single(s) = kb.resolved_label() else {
            panic!("expected Single");
        };
        assert_eq!(s, format_key(b.code, b.modifiers));
    }

    #[test]
    fn alt_label_derives_from_both_binds() {
        let kb = KEYBINDS
            .iter()
            .find(|kb| matches!(kb.label, KeyLabel::Alt))
            .unwrap();
        let a = kb.binds[0];
        let b = kb.binds[1];
        let ResolvedLabel::Alt(la, lb) = kb.resolved_label() else {
            panic!("expected Alt");
        };
        assert_eq!(la, format_key(a.code, a.modifiers));
        assert_eq!(lb, format_key(b.code, b.modifiers));
    }

    #[test]
    fn display_label_keeps_explicit_string() {
        let kb = KEYBINDS
            .iter()
            .find(|kb| kb.description == "Filter")
            .unwrap();
        let ResolvedLabel::Single(s) = kb.resolved_label() else {
            panic!("expected Single");
        };
        assert_eq!(s, "Type");
    }

    #[test]
    fn mac_multi_derives_labels_from_binds_off_mac() {
        if cfg!(target_os = "macos") {
            return;
        }
        let kb = KEYBINDS
            .iter()
            .find(|kb| matches!(kb.label, KeyLabel::MacMulti(_)))
            .unwrap();
        let ResolvedLabel::Multi(keys) = kb.resolved_label() else {
            panic!("expected Multi");
        };
        let binds: Vec<String> = kb
            .binds
            .iter()
            .map(|b| format_key(b.code, b.modifiers))
            .collect();
        assert_eq!(keys, binds);
    }

    #[test]
    fn multi_label_derives_from_all_binds() {
        let kb = KEYBINDS
            .iter()
            .find(|kb| matches!(kb.label, KeyLabel::Multi))
            .unwrap();
        let ResolvedLabel::Multi(keys) = kb.resolved_label() else {
            panic!("expected Multi");
        };
        // The tier-shortcut row must display the shifted symbols, not the digits.
        assert_eq!(keys, vec!["!", "@", "#", "$"]);
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
