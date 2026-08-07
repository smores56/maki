use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::fmt::Write;
use std::sync::LazyLock;
use unicode_width::UnicodeWidthStr;

macro_rules! mod_key {
    ($suffix:expr) => {
        concat!("Ctrl+", $suffix)
    };
}

macro_rules! upper {
    ('a') => {
        "A"
    };
    ('b') => {
        "B"
    };
    ('c') => {
        "C"
    };
    ('d') => {
        "D"
    };
    ('e') => {
        "E"
    };
    ('f') => {
        "F"
    };
    ('g') => {
        "G"
    };
    ('h') => {
        "H"
    };
    ('i') => {
        "I"
    };
    ('j') => {
        "J"
    };
    ('k') => {
        "K"
    };
    ('l') => {
        "L"
    };
    ('m') => {
        "M"
    };
    ('n') => {
        "N"
    };
    ('o') => {
        "O"
    };
    ('p') => {
        "P"
    };
    ('q') => {
        "Q"
    };
    ('r') => {
        "R"
    };
    ('s') => {
        "S"
    };
    ('t') => {
        "T"
    };
    ('u') => {
        "U"
    };
    ('v') => {
        "V"
    };
    ('w') => {
        "W"
    };
    ('x') => {
        "X"
    };
    ('y') => {
        "Y"
    };
    ('z') => {
        "Z"
    };
}

macro_rules! ctrl_bind {
    ($char:tt) => {
        Bind {
            code: KeyCode::Char($char),
            modifiers: KeyModifiers::CONTROL,
            label: mod_key!(upper!($char)),
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bind {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
    pub label: &'static str,
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
    pub const EDIT_INPUT: Bind = Bind {
        code: KeyCode::Char('o'),
        modifiers: KeyModifiers::ALT,
        label: "Alt+O",
    };
}

pub use maki_lua::BuiltinAction;
use maki_lua::{ContextKind, ContextRef, IDENTITIES};

/// Resolve a seed identity by name; panics on names outside the seed
/// table so a typo here breaks at startup, not at render time.
fn identity_ref(name: &'static str) -> ContextRef {
    ContextRef::Identity(
        IDENTITIES
            .iter()
            .find(|i| i.name == name)
            .expect("identity must exist in the seed table")
            .id,
    )
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
    Single(&'static str),
    Alt(&'static str, &'static str),
    /// Alt on Mac, Single (first) on other platforms
    MacAlt(&'static str, &'static str),
    /// Multi on Mac, Multi (first slice) on other platforms
    MacMulti(&'static [&'static str], &'static [&'static str]),
}

pub const ALT_SEP: &str = " / ";

#[derive(Debug, Clone, Copy)]
pub enum ResolvedLabel {
    Single(&'static str),
    Alt(&'static str, &'static str),
    Multi(&'static [&'static str]),
}

impl ResolvedLabel {
    pub fn display_width(self) -> usize {
        match self {
            Self::Single(s) => UnicodeWidthStr::width(s),
            Self::Alt(a, b) => {
                let sep_w = UnicodeWidthStr::width(ALT_SEP);
                UnicodeWidthStr::width(a) + sep_w + UnicodeWidthStr::width(b)
            }
            Self::Multi(keys) => {
                let sep_w = UnicodeWidthStr::width(ALT_SEP);
                keys.iter()
                    .map(|k| UnicodeWidthStr::width(*k))
                    .sum::<usize>()
                    + sep_w * keys.len().saturating_sub(1)
            }
        }
    }
}

impl KeyLabel {
    pub fn resolve(self) -> ResolvedLabel {
        match self {
            Self::Single(s) => ResolvedLabel::Single(s),
            Self::Alt(a, b) => ResolvedLabel::Alt(a, b),
            Self::MacAlt(a, b) => {
                if cfg!(target_os = "macos") {
                    ResolvedLabel::Alt(a, b)
                } else {
                    ResolvedLabel::Single(a)
                }
            }
            Self::MacMulti(normal, mac) => {
                if cfg!(target_os = "macos") {
                    ResolvedLabel::Multi(mac)
                } else {
                    ResolvedLabel::Multi(normal)
                }
            }
        }
    }

    #[cfg(test)]
    fn flat_str(&self) -> String {
        match self.resolve() {
            ResolvedLabel::Single(s) => s.to_string(),
            ResolvedLabel::Alt(a, b) => format!("{a}/{b}"),
            ResolvedLabel::Multi(keys) => keys.join("/"),
        }
    }
}

/// Display title for a kind section: the lowercase label, capitalized.
pub fn section_title(kind: ContextKind) -> String {
    let mut chars = kind.label().chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Display form of a snapshot keybind's key: `Ctrl+C`, `Alt+O`, `Esc`,
/// `Enter`, `Shift+Tab`. Used by the help modal and docgen to render
/// bindings whose source is the live `KeymapReader` snapshot rather than
/// the static `KEYBINDS` table.
pub fn display_key_label(code: KeyCode, mods: KeyModifiers) -> String {
    let is_char = matches!(code, KeyCode::Char(_));
    let mut parts: Vec<String> = Vec::new();
    if mods.contains(KeyModifiers::CONTROL) {
        parts.push("Ctrl".to_string());
    }
    if mods.contains(KeyModifiers::ALT) {
        parts.push("Alt".to_string());
    }
    if mods.contains(KeyModifiers::SHIFT) && !is_char {
        parts.push("Shift".to_string());
    }
    let name = match code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => c.to_ascii_uppercase().to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BackTab => {
            parts.insert(0, "Shift".to_string());
            "Tab".to_string()
        }
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        KeyCode::Insert => "Insert".to_string(),
        _ => String::new(),
    };
    parts.push(name);
    parts.join("+")
}

/// Whether a snapshot entry belongs in a kind's section. General bindings
/// carry no context refs (the empty set); non-General bindings carry
/// exactly the kind they fire in.
pub fn entry_in_kind(entry: &maki_lua::KeymapEntry, kind: ContextKind) -> bool {
    if entry.context.is_empty() {
        return kind == ContextKind::General;
    }
    entry.context == [ContextRef::Kind(kind)]
}

/// Whether a snapshot entry belongs in an identity's section: it must
/// name exactly that identity.
pub fn entry_in_identity(entry: &maki_lua::KeymapEntry, id: u16) -> bool {
    entry.context == [ContextRef::Identity(id)]
}

pub struct Keybind {
    pub label: KeyLabel,
    pub description: &'static str,
    pub context: ContextRef,
    pub platform: Platform,
}

/// Static documentation of widget/component keys that are NOT
/// `BuiltinAction`s and stay hardcoded in Rust (editing keys, overlay
/// navigation). The help modal merges this with the live `KeymapReader`
/// snapshot so both remappable builtins and hardcoded keys are visible.
/// The escape hatches (`Ctrl+Z`, streaming stop) render as their own
/// fixed section, not here.
pub static KEYBINDS: LazyLock<Vec<Keybind>> = LazyLock::new(|| {
    vec![
        Keybind {
            label: KeyLabel::Single("Enter"),
            description: "Submit prompt",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::MacMulti(
                &["Shift+Enter", "Ctrl+Enter", "Ctrl+J", "Alt+Enter"],
                &["⇧↵", "⌃↵", "⌃J", "⌥↵"],
            ),
            description: "Newline",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Tab"),
            description: "Toggle mode",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("/command"),
            description: "Open command palette",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::MacAlt(key::DELETE_WORD.label, "⌥⌫"),
            description: "Delete word backward",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::MacMulti(&["Alt+←", "Alt+→"], &["⌥←", "⌥→"]),
            description: "Move word left / right",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Alt(mod_key!("Del"), "⌥Del"),
            description: "Delete word forward",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::MacOnly,
        },
        Keybind {
            label: KeyLabel::Single(key::KILL_LINE.label),
            description: "Delete to end of line",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::MacOnly,
        },
        Keybind {
            label: KeyLabel::Single(key::LINE_START.label),
            description: "Jump to start of line",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Alt("Home", "End"),
            description: "Jump to start/end of line",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single(key::LINE_END.label),
            description: "Jump to end of line",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Esc Esc"),
            description: "Rewind",
            context: ContextRef::Kind(ContextKind::Chat),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Alt("↑", "↓"),
            description: "Navigate input history",
            context: ContextRef::Kind(ContextKind::Streaming),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Esc Esc"),
            description: "Cancel agent",
            context: ContextRef::Kind(ContextKind::Streaming),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Alt("↑", "↓"),
            description: "Navigate options",
            context: ContextRef::Kind(ContextKind::Form),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Enter"),
            description: "Select option",
            context: ContextRef::Kind(ContextKind::Form),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Esc"),
            description: "Close",
            context: ContextRef::Kind(ContextKind::Form),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Alt("↑", "↓"),
            description: "Navigate",
            context: ContextRef::Kind(ContextKind::Picker),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Enter"),
            description: "Select",
            context: ContextRef::Kind(ContextKind::Picker),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Esc"),
            description: "Close",
            context: ContextRef::Kind(ContextKind::Picker),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Type"),
            description: "Filter",
            context: ContextRef::Kind(ContextKind::Picker),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Alt("PageUp", "PageDown"),
            description: "Scroll page up / down",
            context: ContextRef::Kind(ContextKind::Picker),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Alt(key::SCROLL_HALF_UP.label, key::SCROLL_HALF_DOWN.label),
            description: "Scroll page up / down",
            context: ContextRef::Kind(ContextKind::Picker),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Enter"),
            description: "Remove item",
            context: identity_ref("queue"),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("Tab"),
            description: "Complete command",
            context: identity_ref("commands"),
            platform: Platform::All,
        },
        Keybind {
            label: KeyLabel::Single("!/@/#/$"),
            description: "Set tier (strong/medium/weak/compaction)",
            context: identity_ref("model_picker"),
            platform: Platform::All,
        },
    ]
});

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
        for kind in ContextKind::ALL {
            let has_own = KEYBINDS
                .iter()
                .any(|kb| kb.context == ContextRef::Kind(kind));
            let has_identity = IDENTITIES.iter().any(|i| {
                i.kind == kind
                    && KEYBINDS
                        .iter()
                        .any(|kb| kb.context == ContextRef::Identity(i.id))
            });
            match kind {
                // Plugin-owned: the default bindings live in
                // plugins/keymap/init.lua, not in this table.
                ContextKind::General => assert!(
                    !has_own && !has_identity,
                    "General keybinds belong in plugins/keymap/init.lua",
                ),
                // Display-only kind: no widget keys exist yet.
                ContextKind::Modal => assert!(
                    !has_own && !has_identity,
                    "Modal should have no keybinds yet",
                ),
                _ => assert!(
                    has_own || has_identity,
                    "kind {:?} has no keybinds and no identity with keybinds",
                    kind,
                ),
            }
        }
    }

    #[test]
    fn identity_refs_exist_in_seed_table() {
        for kb in KEYBINDS.iter() {
            if let ContextRef::Identity(id) = kb.context {
                assert!(
                    IDENTITIES.iter().any(|i| i.id == id),
                    "unknown identity id {id}"
                );
            }
        }
    }

    #[test]
    fn no_duplicate_entries() {
        for (i, a) in KEYBINDS.iter().enumerate() {
            for (j, b) in KEYBINDS.iter().enumerate() {
                if i != j && a.context == b.context {
                    assert!(
                        a.label.flat_str() != b.label.flat_str() || a.description != b.description,
                        "duplicate keybind: {} - {} in {:?}",
                        a.label.flat_str(),
                        a.description,
                        a.context,
                    );
                }
            }
        }
    }
}
