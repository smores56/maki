use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use crossterm::event::{KeyCode, KeyModifiers};
use maki_lua_macro::{lua_fn, lua_table};
use mlua::{Lua, RegistryKey, Result as LuaResult, Table, Value};
use strum::{EnumIter, IntoEnumIterator};

use crate::api::actions::{BuiltinAction, LuaBuiltinAction};

static NEXT_KEYMAP_ID: AtomicU64 = AtomicU64::new(1);

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
            Self::Editing | Self::Streaming | Self::FormInput | Self::Picker => Some(Self::General),
            Self::TaskPicker
            | Self::RewindPicker
            | Self::ThemePicker
            | Self::ModelPicker
            | Self::QueueFocus
            | Self::CommandPalette
            | Self::Search
            | Self::FilePicker => Some(Self::Picker),
            Self::General => None,
        }
    }

    pub fn from_label(s: &str) -> Option<Self> {
        Self::iter().find(|c| c.label() == s)
    }

    pub fn all_labels() -> Vec<&'static str> {
        Self::iter().map(|c| c.label()).collect()
    }

    pub fn applies_in(self, active: KeybindContext) -> bool {
        let mut ctx = Some(active);
        while let Some(c) = ctx {
            if c == self {
                return true;
            }
            ctx = c.parent();
        }
        false
    }
}

#[derive(Clone, Debug)]
pub struct KeymapEntry {
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
    pub desc: String,
    pub plugin: Arc<str>,
    pub id: u64,
    pub context: KeybindContext,
    pub kind: EntryKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryKind {
    Callback,
    Builtin(BuiltinAction),
}

impl KeymapEntry {
    pub fn callback(
        key: KeyCode,
        modifiers: KeyModifiers,
        plugin: Arc<str>,
        desc: impl Into<String>,
        id: u64,
    ) -> Self {
        Self {
            key,
            modifiers,
            desc: desc.into(),
            plugin,
            id,
            context: KeybindContext::General,
            kind: EntryKind::Callback,
        }
    }

    pub fn builtin(
        key: KeyCode,
        modifiers: KeyModifiers,
        plugin: Arc<str>,
        desc: impl Into<String>,
        id: u64,
        context: KeybindContext,
        action: BuiltinAction,
    ) -> Self {
        Self {
            key,
            modifiers,
            desc: desc.into(),
            plugin,
            id,
            context,
            kind: EntryKind::Builtin(action),
        }
    }
}

#[derive(Clone, Default)]
pub struct KeymapSnapshot {
    pub entries: Vec<KeymapEntry>,
    pub generation: u64,
}

#[derive(Clone)]
pub struct KeymapReader(Arc<ArcSwap<KeymapSnapshot>>);

impl KeymapReader {
    pub fn empty() -> Self {
        Self(Arc::new(ArcSwap::from_pointee(KeymapSnapshot::default())))
    }

    pub fn load(&self) -> arc_swap::Guard<Arc<KeymapSnapshot>> {
        self.0.load()
    }
}

pub(crate) struct KeymapWriter {
    store: Arc<ArcSwap<KeymapSnapshot>>,
    generation: AtomicU64,
}

impl KeymapWriter {
    pub fn new() -> (Self, KeymapReader) {
        let inner = Arc::new(ArcSwap::from_pointee(KeymapSnapshot::default()));
        (
            Self {
                store: Arc::clone(&inner),
                generation: AtomicU64::new(0),
            },
            KeymapReader(inner),
        )
    }

    pub fn publish(&self, entries: Vec<KeymapEntry>) {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.store.store(Arc::new(KeymapSnapshot {
            entries,
            generation,
        }));
    }
}

pub(crate) enum KeymapKind {
    Callback(RegistryKey),
    Builtin(BuiltinAction),
}

impl std::fmt::Debug for KeymapKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KeymapKind::Callback(_) => write!(f, "KeymapKind::Callback(<registry key>)"),
            KeymapKind::Builtin(a) => write!(f, "KeymapKind::Builtin({a:?})"),
        }
    }
}

impl KeymapKind {
    fn into_callback_key(self) -> Option<RegistryKey> {
        match self {
            KeymapKind::Callback(k) => Some(k),
            KeymapKind::Builtin(_) => None,
        }
    }
}

pub(crate) struct StoredKeymap {
    pub id: u64,
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
    pub kind: KeymapKind,
    pub context: KeybindContext,
    pub plugin: Arc<str>,
    pub desc: String,
}

pub(crate) struct KeymapStore {
    bindings: Vec<StoredKeymap>,
}

impl KeymapStore {
    pub fn new() -> Self {
        Self {
            bindings: Vec::new(),
        }
    }

    pub fn set(
        &mut self,
        key: KeyCode,
        modifiers: KeyModifiers,
        kind: KeymapKind,
        plugin: Arc<str>,
        desc: String,
        context: KeybindContext,
    ) -> (u64, Option<RegistryKey>) {
        let id = NEXT_KEYMAP_ID.fetch_add(1, Ordering::Relaxed);
        let old = self
            .bindings
            .iter()
            .position(|b| b.key == key && b.modifiers == modifiers)
            .map(|pos| self.bindings.remove(pos).kind)
            .and_then(|k| k.into_callback_key());
        self.bindings.push(StoredKeymap {
            id,
            key,
            modifiers,
            kind,
            context,
            plugin,
            desc,
        });
        (id, old)
    }

    pub fn del(&mut self, key: KeyCode, modifiers: KeyModifiers) -> Option<RegistryKey> {
        self.bindings
            .iter()
            .position(|b| b.key == key && b.modifiers == modifiers)
            .map(|pos| self.bindings.remove(pos).kind)
            .and_then(|k| k.into_callback_key())
    }

    pub fn clear_plugin(&mut self, plugin: &str) -> Vec<RegistryKey> {
        let mut keys = Vec::new();
        let mut i = 0;
        while i < self.bindings.len() {
            if self.bindings[i].plugin.as_ref() == plugin {
                let removed = self.bindings.remove(i);
                if let Some(k) = removed.kind.into_callback_key() {
                    keys.push(k);
                }
            } else {
                i += 1;
            }
        }
        keys
    }

    pub fn snapshot_entries(&self) -> Vec<KeymapEntry> {
        self.bindings
            .iter()
            .map(|b| KeymapEntry {
                key: b.key,
                modifiers: b.modifiers,
                desc: b.desc.clone(),
                plugin: Arc::clone(&b.plugin),
                id: b.id,
                context: b.context,
                kind: match &b.kind {
                    KeymapKind::Callback(_) => EntryKind::Callback,
                    KeymapKind::Builtin(a) => EntryKind::Builtin(*a),
                },
            })
            .collect()
    }

    pub fn callback_for_id(&self, id: u64) -> Option<&RegistryKey> {
        self.bindings
            .iter()
            .find(|b| b.id == id)
            .and_then(|b| match &b.kind {
                KeymapKind::Callback(k) => Some(k),
                KeymapKind::Builtin(_) => None,
            })
    }
}

pub fn parse_key_notation(input: &str) -> Result<(KeyCode, KeyModifiers), String> {
    let s = input.trim();
    if s.is_empty() {
        return Err("empty key notation".into());
    }

    if s.starts_with('<') && s.ends_with('>') {
        let inner = &s[1..s.len() - 1];
        return parse_bracketed(inner);
    }

    if s.len() == 1 {
        let c = s.chars().next().unwrap();
        return Ok((KeyCode::Char(c), KeyModifiers::NONE));
    }

    Err(format!("invalid key notation: {s}"))
}

fn parse_bracketed(inner: &str) -> Result<(KeyCode, KeyModifiers), String> {
    if inner.is_empty() {
        return Err("empty angle-bracket key notation".into());
    }

    let mut modifiers = KeyModifiers::NONE;
    let mut rest = inner;

    loop {
        let lower = rest.to_lowercase();
        if lower.starts_with("c-") {
            modifiers |= KeyModifiers::CONTROL;
            rest = &rest[2..];
        } else if lower.starts_with("ctrl-") {
            modifiers |= KeyModifiers::CONTROL;
            rest = &rest[5..];
        } else if lower.starts_with("a-") {
            modifiers |= KeyModifiers::ALT;
            rest = &rest[2..];
        } else if lower.starts_with("alt-") {
            modifiers |= KeyModifiers::ALT;
            rest = &rest[4..];
        } else if lower.starts_with("m-") {
            modifiers |= KeyModifiers::ALT;
            rest = &rest[2..];
        } else if lower.starts_with("s-") {
            modifiers |= KeyModifiers::SHIFT;
            rest = &rest[2..];
        } else if lower.starts_with("shift-") {
            modifiers |= KeyModifiers::SHIFT;
            rest = &rest[6..];
        } else {
            break;
        }
    }

    let key = parse_key_name(rest)?;
    Ok((key, modifiers))
}

fn parse_key_name(name: &str) -> Result<KeyCode, String> {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "cr" | "enter" | "return" => Ok(KeyCode::Enter),
        "space" => Ok(KeyCode::Char(' ')),
        "esc" | "escape" => Ok(KeyCode::Esc),
        "tab" => Ok(KeyCode::Tab),
        "bs" | "backspace" => Ok(KeyCode::Backspace),
        "del" | "delete" => Ok(KeyCode::Delete),
        "up" => Ok(KeyCode::Up),
        "down" => Ok(KeyCode::Down),
        "left" => Ok(KeyCode::Left),
        "right" => Ok(KeyCode::Right),
        "home" => Ok(KeyCode::Home),
        "end" => Ok(KeyCode::End),
        "pageup" => Ok(KeyCode::PageUp),
        "pagedown" => Ok(KeyCode::PageDown),
        "insert" => Ok(KeyCode::Insert),
        s if s.starts_with('f') && s.len() > 1 => {
            let n: u8 = s[1..]
                .parse()
                .map_err(|_| format!("invalid function key: {name}"))?;
            if !(1..=12).contains(&n) {
                return Err(format!("function key out of range: {name}"));
            }
            Ok(KeyCode::F(n))
        }
        _ => {
            if name.len() == 1 {
                Ok(KeyCode::Char(name.chars().next().unwrap()))
            } else {
                Err(format!("unknown key: {name}"))
            }
        }
    }
}

fn publish_keymap_snapshot(lua: &Lua) {
    if let Some(store) = lua.app_data_ref::<KeymapStore>() {
        let entries = store.snapshot_entries();
        if let Some(writer) = lua.app_data_ref::<KeymapWriter>() {
            writer.publish(entries);
        }
    }
}

fn parse_context(opts: Option<&Table>) -> LuaResult<KeybindContext> {
    let Some(opts) = opts else {
        return Ok(KeybindContext::General);
    };
    let raw: Option<String> = opts.get("context")?;
    match raw {
        None => Ok(KeybindContext::General),
        Some(s) => KeybindContext::from_label(&s).ok_or_else(|| {
            mlua::Error::runtime(format!(
                "unknown opts.context {s:?}; expected one of: {}",
                KeybindContext::all_labels().join(", ")
            ))
        }),
    }
}

/// The single entry on the Lua-side RHS parser. Accepts (1) a
/// `maki.actions.*` userdata, or (2) a Lua function. Strings,
/// numbers, and everything else (including `nil` from a typo'd
/// `maki.actions.<name>` lookup) throw with a clear message.
pub(crate) fn parse_rhs(lua: &Lua, rhs: Value) -> LuaResult<KeymapKind> {
    match rhs {
        Value::Function(f) => {
            let key = lua.create_registry_value(f)?;
            Ok(KeymapKind::Callback(key))
        }
        Value::UserData(ud) => {
            let handle = ud.borrow::<LuaBuiltinAction>()?;
            Ok(KeymapKind::Builtin(handle.0))
        }
        Value::Nil => Err(mlua::Error::runtime(
            "keymap rhs is nil. Did you typo a maki.actions.* name? \
             Pass a function or a maki.actions.<name> value.",
        )),
        other => Err(mlua::Error::runtime(format!(
            "keymap rhs must be a function or maki.actions.<name>, got {}",
            other.type_name()
        ))),
    }
}

/// Bind a key to a Lua function or builtin action, just like
/// `vim.keymap.set`. Only normal mode (`"n"`) is supported right now.
/// If {lhs} is already mapped, the old binding is replaced and a
/// warning is logged.
///
/// @param mode string Mode letter. Currently only `"n"` is accepted.
/// @param lhs string Key in Vim notation, e.g. `"<C-t>"`, `"<Space>"`, `"a"`.
/// @param rhs function|userdata Either a Lua function invoked on press, or a `maki.actions.<name>` handle (zero per-keypress Lua traffic).
/// @param opts table? Options:
///   `desc` (string) short description shown in the keymap list.
///   `context` (string) where the binding fires; one of the
///     `KeybindContext` labels (case-sensitive). Defaults to `"General"`,
///     which fires everywhere.
/// @example
/// maki.keymap.set("n", "<C-t>", maki.actions.plan_toggle, { desc = "Toggle panel" })
/// @example
/// maki.keymap.set("n", "<C-t>", function()
///   print("toggle!")
/// end, { desc = "Toggle panel" })
#[lua_fn]
fn set(
    lua: &Lua,
    #[ctx] plugin: Arc<str>,
    mode: String,
    lhs: String,
    rhs: Value,
    opts: Option<Table>,
) -> LuaResult<()> {
    if mode != "n" {
        return Err(mlua::Error::runtime(format!(
            "unsupported keymap mode: {mode}"
        )));
    }
    let (key, modifiers) = parse_key_notation(&lhs).map_err(mlua::Error::runtime)?;
    let desc = opts
        .as_ref()
        .and_then(|o| o.get::<String>("desc").ok())
        .unwrap_or_default();
    let context = parse_context(opts.as_ref())?;
    let kind = parse_rhs(lua, rhs)?;
    let (_, old) = lua
        .app_data_mut::<KeymapStore>()
        .ok_or_else(|| mlua::Error::runtime("keymap store not initialized"))?
        .set(key, modifiers, kind, Arc::clone(&plugin), desc, context);
    if let Some(old_key) = old {
        tracing::warn!(key = %lhs, plugin = %plugin, "keymap shadowed by plugin");
        let _ = lua.remove_registry_value(old_key);
    }
    publish_keymap_snapshot(lua);
    Ok(())
}

/// Remove the mapping for {lhs} in {mode}. Does nothing if no mapping
/// exists for that key.
///
/// @param mode string Mode letter (reserved for future modes).
/// @param lhs string Key to unmap, in Vim notation.
/// @example
/// maki.keymap.del("n", "<C-t>")
#[lua_fn]
fn del(lua: &Lua, #[ctx] plugin: Arc<str>, mode: String, lhs: String) -> LuaResult<()> {
    let _ = (mode, &plugin);
    let (key, modifiers) = parse_key_notation(&lhs).map_err(mlua::Error::runtime)?;
    let old = lua
        .app_data_mut::<KeymapStore>()
        .and_then(|mut store| store.del(key, modifiers));
    if let Some(old_key) = old {
        let _ = lua.remove_registry_value(old_key);
    }
    publish_keymap_snapshot(lua);
    Ok(())
}

/// The set of bindings `plugins/keymap/init.lua` registers at startup.
/// Single Rust source of truth mirrored 1:1 by the Lua plugin; the
/// `default_keymap_matches_plugin` drift gate asserts they stay in sync.
/// Used by the test harness to pre-populate a `KeymapReader` without a
/// running Lua runtime.
pub(crate) const DEFAULT_KEYBINDS: &[(&str, BuiltinAction, KeybindContext)] = &[
    ("<C-c>", BuiltinAction::Quit, KeybindContext::General),
    ("<C-h>", BuiltinAction::Help, KeybindContext::General),
    ("<C-p>", BuiltinAction::PrevChat, KeybindContext::General),
    ("<C-n>", BuiltinAction::NextChat, KeybindContext::General),
    (
        "<C-u>",
        BuiltinAction::ScrollHalfUp,
        KeybindContext::General,
    ),
    (
        "<C-d>",
        BuiltinAction::ScrollHalfDown,
        KeybindContext::General,
    ),
    ("<C-g>", BuiltinAction::ScrollTop, KeybindContext::General),
    (
        "<C-b>",
        BuiltinAction::ScrollBottom,
        KeybindContext::General,
    ),
    ("<C-t>", BuiltinAction::PlanToggle, KeybindContext::General),
    ("<C-x>", BuiltinAction::Tasks, KeybindContext::General),
    ("<C-f>", BuiltinAction::Search, KeybindContext::General),
    ("<C-s>", BuiltinAction::FilePicker, KeybindContext::General),
    ("<C-o>", BuiltinAction::OpenEditor, KeybindContext::General),
    ("<M-o>", BuiltinAction::EditInput, KeybindContext::General),
    ("<C-q>", BuiltinAction::PopQueue, KeybindContext::General),
];

pub fn default_keymap_entries() -> Vec<KeymapEntry> {
    DEFAULT_KEYBINDS
        .iter()
        .map(|(notation, action, ctx)| {
            let (key, modifiers) =
                parse_key_notation(notation).expect("default key notation parses");
            KeymapEntry::builtin(
                key,
                modifiers,
                Arc::from("keymap"),
                action.description(),
                0,
                *ctx,
                *action,
            )
        })
        .collect()
}

lua_table! {
    /// Key mappings, modeled after `vim.keymap`. If you have written a
    /// Neovim keymap plugin before, this will feel familiar.
    ///
    /// ```lua
    /// maki.keymap.set("n", "<C-t>", function()
    ///   print("hello")
    /// end, { desc = "Say hello" })
    /// ```
    "maki.keymap" => pub(crate) fn create_keymap_table(plugin: Arc<str>), DOCS [
        set(plugin), del(plugin),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use test_case::test_case;

    #[test_case("<C-t>", KeyCode::Char('t'), KeyModifiers::CONTROL ; "ctrl_t")]
    #[test_case("<C-T>", KeyCode::Char('T'), KeyModifiers::CONTROL ; "ctrl_shift_t")]
    #[test_case("<A-x>", KeyCode::Char('x'), KeyModifiers::ALT ; "alt_x")]
    #[test_case("<M-x>", KeyCode::Char('x'), KeyModifiers::ALT ; "meta_x")]
    #[test_case("<S-Tab>", KeyCode::Tab, KeyModifiers::SHIFT ; "shift_tab")]
    #[test_case("<CR>", KeyCode::Enter, KeyModifiers::NONE ; "enter_cr")]
    #[test_case("<Enter>", KeyCode::Enter, KeyModifiers::NONE ; "enter_full")]
    #[test_case("<Space>", KeyCode::Char(' '), KeyModifiers::NONE ; "space")]
    #[test_case("<Esc>", KeyCode::Esc, KeyModifiers::NONE ; "escape")]
    #[test_case("<Tab>", KeyCode::Tab, KeyModifiers::NONE ; "tab")]
    #[test_case("<BS>", KeyCode::Backspace, KeyModifiers::NONE ; "backspace_short")]
    #[test_case("<Backspace>", KeyCode::Backspace, KeyModifiers::NONE ; "backspace_full")]
    #[test_case("<Del>", KeyCode::Delete, KeyModifiers::NONE ; "delete_short")]
    #[test_case("<Delete>", KeyCode::Delete, KeyModifiers::NONE ; "delete_full")]
    #[test_case("<Up>", KeyCode::Up, KeyModifiers::NONE ; "up")]
    #[test_case("<Down>", KeyCode::Down, KeyModifiers::NONE ; "down")]
    #[test_case("<Left>", KeyCode::Left, KeyModifiers::NONE ; "left")]
    #[test_case("<Right>", KeyCode::Right, KeyModifiers::NONE ; "right")]
    #[test_case("<Home>", KeyCode::Home, KeyModifiers::NONE ; "home")]
    #[test_case("<End>", KeyCode::End, KeyModifiers::NONE ; "end_key")]
    #[test_case("<PageUp>", KeyCode::PageUp, KeyModifiers::NONE ; "page_up")]
    #[test_case("<PageDown>", KeyCode::PageDown, KeyModifiers::NONE ; "page_down")]
    #[test_case("<Insert>", KeyCode::Insert, KeyModifiers::NONE ; "insert")]
    #[test_case("<F1>", KeyCode::F(1), KeyModifiers::NONE ; "f1")]
    #[test_case("<F12>", KeyCode::F(12), KeyModifiers::NONE ; "f12")]
    #[test_case("a", KeyCode::Char('a'), KeyModifiers::NONE ; "plain_a")]
    #[test_case("z", KeyCode::Char('z'), KeyModifiers::NONE ; "plain_z")]
    #[test_case("<C-S-a>", KeyCode::Char('a'), KeyModifiers::from_bits_truncate(KeyModifiers::CONTROL.bits() | KeyModifiers::SHIFT.bits()) ; "ctrl_shift_a")]
    #[test_case("<Ctrl-x>", KeyCode::Char('x'), KeyModifiers::CONTROL ; "ctrl_long_x")]
    #[test_case("<Alt-j>", KeyCode::Char('j'), KeyModifiers::ALT ; "alt_long_j")]
    #[test_case("<Shift-Tab>", KeyCode::Tab, KeyModifiers::SHIFT ; "shift_long_tab")]
    #[test_case("<Return>", KeyCode::Enter, KeyModifiers::NONE ; "return_key")]
    #[test_case("<Escape>", KeyCode::Esc, KeyModifiers::NONE ; "escape_full")]
    fn parse_key_notation_cases(input: &str, code: KeyCode, mods: KeyModifiers) {
        let (key, modifiers) = parse_key_notation(input).unwrap();
        assert_eq!(key, code);
        assert_eq!(modifiers, mods);
    }

    #[test]
    fn parse_key_notation_errors() {
        assert!(parse_key_notation("").is_err());
        assert!(parse_key_notation("<>").is_err());
        assert!(parse_key_notation("<F0>").is_err());
        assert!(parse_key_notation("<F13>").is_err());
        assert!(parse_key_notation("abc").is_err());
    }

    #[test]
    fn keymap_store_set_and_shadow() {
        let lua = Lua::new();
        let mut store = KeymapStore::new();

        let f1 = lua.create_function(|_, ()| Ok(())).unwrap();
        let k1 = lua.create_registry_value(f1).unwrap();
        let (id1, old1) = store.set(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
            KeymapKind::Callback(k1),
            Arc::from("plug"),
            "toggle".into(),
            KeybindContext::General,
        );
        assert!(old1.is_none());

        let f2 = lua.create_function(|_, ()| Ok(())).unwrap();
        let k2 = lua.create_registry_value(f2).unwrap();
        let (id2, old2) = store.set(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
            KeymapKind::Callback(k2),
            Arc::from("plug2"),
            "toggle v2".into(),
            KeybindContext::General,
        );
        assert!(old2.is_some());
        assert_ne!(id1, id2);
        assert_eq!(store.bindings.len(), 1);
    }

    #[test]
    fn keymap_store_del() {
        let lua = Lua::new();
        let mut store = KeymapStore::new();

        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let k = lua.create_registry_value(f).unwrap();
        store.set(
            KeyCode::Char('x'),
            KeyModifiers::ALT,
            KeymapKind::Callback(k),
            Arc::from("p"),
            String::new(),
            KeybindContext::General,
        );
        assert_eq!(store.bindings.len(), 1);

        let removed = store.del(KeyCode::Char('x'), KeyModifiers::ALT);
        assert!(removed.is_some());
        assert!(store.bindings.is_empty());

        let missing = store.del(KeyCode::Char('x'), KeyModifiers::ALT);
        assert!(missing.is_none());
    }

    #[test]
    fn keymap_store_clear_plugin() {
        let lua = Lua::new();
        let mut store = KeymapStore::new();

        let f1 = lua.create_function(|_, ()| Ok(())).unwrap();
        let f2 = lua.create_function(|_, ()| Ok(())).unwrap();
        let k1 = lua.create_registry_value(f1).unwrap();
        let k2 = lua.create_registry_value(f2).unwrap();
        store.set(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
            KeymapKind::Callback(k1),
            Arc::from("a"),
            String::new(),
            KeybindContext::General,
        );
        store.set(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
            KeymapKind::Callback(k2),
            Arc::from("b"),
            String::new(),
            KeybindContext::General,
        );

        let removed = store.clear_plugin("a");
        assert_eq!(removed.len(), 1);
        assert_eq!(store.bindings.len(), 1);
        assert_eq!(store.bindings[0].plugin.as_ref(), "b");
    }

    #[test]
    fn snapshot_reader_writer() {
        let (writer, reader) = KeymapWriter::new();
        assert!(reader.load().entries.is_empty());

        writer.publish(vec![KeymapEntry {
            key: KeyCode::Char('t'),
            modifiers: KeyModifiers::CONTROL,
            desc: "test".into(),
            plugin: Arc::from("p"),
            id: 1,
            context: KeybindContext::General,
            kind: EntryKind::Callback,
        }]);

        let snap = reader.load();
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.generation, 1);
    }

    #[test_case(KeybindContext::General, KeybindContext::General, true ; "general_self")]
    #[test_case(KeybindContext::General, KeybindContext::Editing, true ; "general_to_editing")]
    #[test_case(KeybindContext::General, KeybindContext::FilePicker, true ; "general_to_picker")]
    #[test_case(KeybindContext::Picker, KeybindContext::FilePicker, true ; "picker_to_child")]
    #[test_case(KeybindContext::Picker, KeybindContext::TaskPicker, true ; "picker_to_other_child")]
    #[test_case(KeybindContext::Picker, KeybindContext::General, false ; "picker_not_in_general")]
    #[test_case(KeybindContext::FilePicker, KeybindContext::FilePicker, true ; "child_self")]
    #[test_case(KeybindContext::Editing, KeybindContext::General, false ; "editing_not_in_general")]
    #[test_case(KeybindContext::Editing, KeybindContext::Picker, false ; "editing_not_in_picker")]
    #[test_case(KeybindContext::FilePicker, KeybindContext::ThemePicker, false ; "sibling_not_applies")]
    fn applies_in_predicate(binding: KeybindContext, active: KeybindContext, expected: bool) {
        assert_eq!(binding.applies_in(active), expected);
    }

    #[test]
    fn from_label_roundtrips() {
        for ctx in KeybindContext::iter() {
            assert_eq!(KeybindContext::from_label(ctx.label()), Some(ctx));
        }
        assert_eq!(KeybindContext::from_label("nonexistent"), None);
    }

    #[test]
    fn all_labels_covers_every_variant() {
        let labels = KeybindContext::all_labels();
        assert_eq!(labels.len(), KeybindContext::iter().count());
        for ctx in KeybindContext::iter() {
            assert!(labels.contains(&ctx.label()));
        }
    }

    #[test]
    fn parent_roots_at_general() {
        for ctx in KeybindContext::iter() {
            let mut cur = Some(ctx);
            while let Some(c) = cur {
                cur = c.parent();
                if c == KeybindContext::General {
                    assert!(cur.is_none(), "General must be the root");
                    break;
                }
            }
        }
    }

    #[test]
    fn parse_rhs_accepts_function() {
        let lua = Lua::new();
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let kind = parse_rhs(&lua, mlua::Value::Function(f)).unwrap();
        assert!(matches!(kind, KeymapKind::Callback(_)));
    }

    #[test]
    fn parse_rhs_accepts_builtin_userdata() {
        let lua = Lua::new();
        let t = crate::api::actions::create_actions_table(&lua).unwrap();
        let ud: mlua::AnyUserData = t.get("scroll_top").unwrap();
        let kind = parse_rhs(&lua, mlua::Value::UserData(ud)).unwrap();
        match kind {
            KeymapKind::Builtin(BuiltinAction::ScrollTop) => {}
            other => panic!("expected Builtin(ScrollTop), got {other:?}"),
        }
    }

    #[test]
    fn parse_rhs_rejects_nil_with_hint() {
        let lua = Lua::new();
        let err = parse_rhs(&lua, mlua::Value::Nil).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("nil") && msg.contains("maki.actions"),
            "expected typo hint, got: {msg}"
        );
    }

    #[test]
    fn parse_rhs_rejects_string() {
        let lua = Lua::new();
        let s = lua.create_string("not a function").unwrap();
        let err = parse_rhs(&lua, mlua::Value::String(s)).unwrap_err();
        assert!(
            err.to_string().contains("keymap rhs must be"),
            "expected type rejection, got: {err}"
        );
    }

    #[test]
    fn parse_rhs_rejects_number() {
        let lua = Lua::new();
        let err = parse_rhs(&lua, mlua::Value::Integer(42)).unwrap_err();
        assert!(
            err.to_string().contains("keymap rhs must be"),
            "expected type rejection, got: {err}"
        );
    }

    #[test]
    fn parse_rhs_rejects_bool() {
        let lua = Lua::new();
        let err = parse_rhs(&lua, mlua::Value::Boolean(true)).unwrap_err();
        assert!(
            err.to_string().contains("keymap rhs must be"),
            "expected type rejection, got: {err}"
        );
    }

    #[test]
    fn parse_context_defaults_to_general() {
        assert_eq!(parse_context(None).unwrap(), KeybindContext::General);
        let lua = Lua::new();
        let empty = lua.create_table().unwrap();
        assert_eq!(
            parse_context(Some(&empty)).unwrap(),
            KeybindContext::General
        );
    }

    #[test_case("Editing", KeybindContext::Editing ; "editing")]
    #[test_case("Pickers", KeybindContext::Picker ; "pickers_label")]
    #[test_case("File Picker", KeybindContext::FilePicker ; "file_picker_label")]
    fn parse_context_accepts_known_label(label: &str, expected: KeybindContext) {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("context", label).unwrap();
        assert_eq!(parse_context(Some(&t)).unwrap(), expected);
    }

    #[test]
    fn default_keymap_matches_plugin() {
        const PLUGIN: &str = include_str!("../../../plugins/keymap/init.lua");
        let expected: std::collections::BTreeSet<(&str, &str, &str)> = DEFAULT_KEYBINDS
            .iter()
            .map(|(lhs, action, ctx)| (*lhs, action.lua_name(), ctx.label()))
            .collect();
        let set_re = regex::Regex::new(
            r#"maki\.keymap\.set\("n",\s*"([^"]+)",\s*maki\.actions\.(\w+)(?:,\s*\{\s*context\s*=\s*"([^"]+)"\s*\})?\)"#,
        )
        .unwrap();
        let mut actual: std::collections::BTreeSet<(&str, &str, &str)> =
            std::collections::BTreeSet::new();
        for cap in set_re.captures_iter(PLUGIN) {
            let lhs = cap.get(1).unwrap().as_str();
            let action = cap.get(2).unwrap().as_str();
            let ctx = cap.get(3).map(|m| m.as_str()).unwrap_or("General");
            actual.insert((lhs, action, ctx));
        }
        assert_eq!(
            expected, actual,
            "plugins/keymap/init.lua drifted from DEFAULT_KEYBINDS"
        );
        expected.iter().for_each(|(_, action, _)| {
            assert!(
                BuiltinAction::from_lua_name(action).is_some(),
                "plugin references unknown action {action}"
            );
        });
    }
}
