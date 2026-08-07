use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use crossterm::event::{KeyCode, KeyModifiers};
use maki_lua_macro::{lua_fn, lua_table};
use mlua::{Lua, RegistryKey, Result as LuaResult, Table, Value};

use crate::api::actions::{BuiltinAction, LuaBuiltinAction};
use crate::api::context::{ContextRef, IDENTITIES, all_names, resolve};

static NEXT_KEYMAP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Debug)]
pub struct KeymapEntry {
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
    pub desc: String,
    pub plugin: Arc<str>,
    pub id: u64,
    pub context: Vec<ContextRef>,
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
            context: Vec::new(),
            kind: EntryKind::Callback,
        }
    }

    pub fn builtin(
        key: KeyCode,
        modifiers: KeyModifiers,
        plugin: Arc<str>,
        desc: impl Into<String>,
        id: u64,
        context: Vec<ContextRef>,
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
    pub context: Vec<ContextRef>,
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
        context: Vec<ContextRef>,
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

    pub fn lookup(&self, key: KeyCode, modifiers: KeyModifiers) -> Option<&StoredKeymap> {
        self.bindings
            .iter()
            .find(|b| b.key == key && b.modifiers == modifiers)
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
                context: b.context.clone(),
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

/// Lowercased names accepted by [`parse_key_name`], minus function keys.
const KNOWN_KEY_NAMES: &[&str] = &[
    "cr",
    "enter",
    "return",
    "space",
    "esc",
    "escape",
    "tab",
    "bs",
    "backspace",
    "del",
    "delete",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "insert",
];

/// Smallest edit distance between two strings; O(len(a) * len(b)).
fn edit_distance(a: &str, b: &str) -> usize {
    let a = a.as_bytes();
    let b = b.as_bytes();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j + 1] + 1)
                .min(cur[j] + 1)
                .min(prev[j] + usize::from(ca != cb));
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Nearest candidate within edit distance 2 of `name` (case-insensitive).
fn did_you_mean<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let name = name.to_lowercase();
    let mut best: Option<(&'a str, usize)> = None;
    for candidate in candidates {
        let d = edit_distance(&name, candidate);
        if d <= 2 && best.is_none_or(|(_, bd)| d < bd) {
            best = Some((candidate, d));
        }
    }
    best.map(|(candidate, _)| candidate)
}

/// `" Did you mean {nearest}?"` when a suggestion exists, else `""`.
fn did_you_mean_suffix<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> String {
    did_you_mean(name, candidates)
        .map(|c| format!(" Did you mean {c}?"))
        .unwrap_or_default()
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

    Err(format!(
        "invalid key notation: {s}{}",
        did_you_mean_suffix(s, KNOWN_KEY_NAMES.iter().copied())
    ))
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
                Err(format!(
                    "unknown key: {name}{}",
                    did_you_mean_suffix(name, KNOWN_KEY_NAMES.iter().copied())
                ))
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

/// `opts.context`: nil (General), one name, or a list of names with AND
/// semantics (every name must resolve; empty list = General).
fn parse_context(opts: Option<&Table>) -> LuaResult<Vec<ContextRef>> {
    let Some(opts) = opts else {
        return Ok(Vec::new());
    };
    let raw: Option<Value> = opts.get("context")?;
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    match raw {
        Value::String(s) => {
            let name = s.to_str()?;
            resolve_context_name(&name).map(|r| vec![r])
        }
        Value::Table(t) => {
            let mut refs = Vec::new();
            for name in t.sequence_values::<String>() {
                refs.push(resolve_context_name(&name?)?);
            }
            Ok(refs)
        }
        other => Err(mlua::Error::runtime(format!(
            "opts.context must be a string or a list of strings, got {}",
            other.type_name()
        ))),
    }
}

fn resolve_context_name(name: &str) -> LuaResult<ContextRef> {
    let name = name.trim();
    resolve(name).ok_or_else(|| {
        let names = all_names();
        mlua::Error::runtime(format!(
            "unknown context {name:?}; expected one of: {}{}",
            names.join(", "),
            did_you_mean_suffix(name, names.into_iter())
        ))
    })
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
        other => {
            let suggestion = match &other {
                Value::String(s) => s
                    .to_str()
                    .ok()
                    .as_ref()
                    .and_then(|name| {
                        did_you_mean(name, BuiltinAction::ALL.iter().map(|(_, n, _)| *n))
                    })
                    .map(|n| format!(" Did you mean maki.actions.{n}?")),
                _ => None,
            };
            Err(mlua::Error::runtime(format!(
                "keymap rhs must be a function or maki.actions.<name>, got {}{}",
                other.type_name(),
                suggestion.unwrap_or_default()
            )))
        }
    }
}

/// Bind a key to a Lua function or builtin action, just like
/// `vim.keymap.set`.
/// If {lhs} is already mapped, the old binding is replaced and a
/// warning is logged.
///
/// @param lhs string Key in Vim notation, e.g. `"<C-t>"`, `"<Space>"`, `"a"`.
/// @param rhs function|userdata Either a Lua function invoked on press, or a `maki.actions.<name>` handle (zero per-keypress Lua traffic).
/// @param opts table? Options:
///   `desc` (string) short description shown in the keymap list.
///   `context` (string|list of strings) where the binding fires: a kind
///     (`"general"`, `"chat"`, `"streaming"`, `"picker"`, `"form"`,
///     `"modal"`) or an identity name (`"task_picker"`, `"search"`,
///     `"help"`, ...). A list means AND — every named context must be
///     active. Defaults to General, which fires everywhere.
/// @example
/// maki.keymap.set("<C-t>", maki.actions.plan_toggle, { desc = "Toggle panel" })
/// @example
/// maki.keymap.set("<C-t>", function()
///   print("toggle!")
/// end, { desc = "Toggle panel" })
#[lua_fn]
fn set(
    lua: &Lua,
    #[ctx] plugin: Arc<str>,
    lhs: String,
    rhs: Value,
    opts: Option<Table>,
) -> LuaResult<()> {
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

/// Remove the mapping for {lhs}. Does nothing if no mapping exists
/// for that key.
///
/// @param lhs string Key to unmap, in Vim notation.
/// @example
/// maki.keymap.del("<C-t>")
#[lua_fn]
fn del(lua: &Lua, #[ctx] plugin: Arc<str>, lhs: String) -> LuaResult<()> {
    let _ = &plugin;
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

/// Return the current mapping for {lhs}, or nil if unmapped.
///
/// @param lhs string Key in Vim notation, e.g. `"<C-t>"`.
/// @return (table?) Entry with `kind` (`"builtin"` or `"callback"`),
///   `action` (lua name, builtins only), `context` (comma-joined names,
///   `"General"` when unbounded), `desc`, and `plugin`.
/// @example
/// local entry = maki.keymap.get("<C-t>")
/// print(entry and entry.kind or "unmapped")
#[lua_fn]
fn get(lua: &Lua, #[ctx] plugin: Arc<str>, lhs: String) -> LuaResult<Option<Table>> {
    let _ = &plugin;
    let (key, modifiers) = parse_key_notation(&lhs).map_err(mlua::Error::runtime)?;
    let Some(store) = lua.app_data_ref::<KeymapStore>() else {
        return Ok(None);
    };
    let Some(entry) = store.lookup(key, modifiers) else {
        return Ok(None);
    };
    let t = lua.create_table()?;
    match &entry.kind {
        KeymapKind::Builtin(action) => {
            t.set("kind", "builtin")?;
            t.set("action", action.lua_name())?;
        }
        KeymapKind::Callback(_) => {
            t.set("kind", "callback")?;
            t.set("action", mlua::Value::Nil)?;
        }
    }
    let context = if entry.context.is_empty() {
        "General".to_string()
    } else {
        entry
            .context
            .iter()
            .map(|r| match r {
                ContextRef::Kind(k) => k.label(),
                ContextRef::Identity(id) => {
                    IDENTITIES
                        .iter()
                        .find(|i| i.id == *id)
                        .expect("identity id must exist in the seed table")
                        .name
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    t.set("context", context)?;
    t.set("desc", entry.desc.as_str())?;
    t.set("plugin", entry.plugin.as_ref())?;
    Ok(Some(t))
}

lua_table! {
    /// Key mappings, modeled after `vim.keymap`. If you have written a
    /// Neovim keymap plugin before, this will feel familiar.
    ///
    /// ```lua
    /// maki.keymap.set("<C-t>", function()
    ///   print("hello")
    /// end, { desc = "Say hello" })
    /// ```
    "maki.keymap" => pub(crate) fn create_keymap_table(plugin: Arc<str>), DOCS [
        set(plugin), del(plugin), get(plugin),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::context::{ActiveContext, ContextKind, IdentityId, applies, tier};
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
            Vec::new(),
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
            Vec::new(),
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
            Vec::new(),
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
            Vec::new(),
        );
        store.set(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
            KeymapKind::Callback(k2),
            Arc::from("b"),
            String::new(),
            Vec::new(),
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
            context: Vec::new(),
            kind: EntryKind::Callback,
        }]);

        let snap = reader.load();
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.generation, 1);
    }

    #[test_case(&[], 0 ; "empty_is_general")]
    #[test_case(&[ContextRef::Kind(ContextKind::General)], 1 ; "kind_general")]
    #[test_case(&[ContextRef::Kind(ContextKind::Picker)], 1 ; "kind_picker")]
    #[test_case(&[ContextRef::Identity(0)], 2 ; "identity")]
    #[test_case(&[ContextRef::Kind(ContextKind::Picker), ContextRef::Identity(0)], 2 ; "kind_plus_identity")]
    #[test_case(&[ContextRef::Identity(0), ContextRef::Identity(1)], 2 ; "two_identities")]
    fn tier_is_max_over_refs(refs: &[ContextRef], expected: u8) {
        assert_eq!(tier(refs), expected);
    }

    fn active(identity: Option<IdentityId>, kinds: u8) -> ActiveContext {
        ActiveContext { identity, kinds }
    }

    fn bits(kinds: &[ContextKind]) -> u8 {
        kinds.iter().fold(0, |bits, k| bits | (1 << *k as u8))
    }

    #[test_case(&[], None, &[], true ; "empty_matches_empty")]
    #[test_case(&[], Some(7), &[ContextKind::Picker], true ; "empty_matches_any")]
    #[test_case(&[ContextRef::Kind(ContextKind::General)], Some(7), &[ContextKind::Picker], true ; "kind_general_matches_any")]
    #[test_case(&[ContextRef::Kind(ContextKind::Picker)], None, &[ContextKind::Picker], true ; "kind_picker_matches_picker")]
    #[test_case(&[ContextRef::Kind(ContextKind::Picker)], None, &[ContextKind::Chat], false ; "kind_picker_not_in_chat")]
    #[test_case(&[ContextRef::Kind(ContextKind::Streaming)], None, &[ContextKind::Streaming, ContextKind::Picker], true ; "kind_streaming_in_union")]
    #[test_case(&[ContextRef::Kind(ContextKind::Chat)], Some(7), &[ContextKind::Picker], false ; "kind_chat_not_in_picker_only")]
    #[test_case(&[ContextRef::Identity(7)], Some(7), &[], true ; "identity_matches_self")]
    #[test_case(&[ContextRef::Identity(7)], Some(8), &[ContextKind::Picker], false ; "identity_does_not_match_sibling")]
    #[test_case(&[ContextRef::Identity(7)], None, &[ContextKind::Picker], false ; "identity_does_not_match_kind_only")]
    #[test_case(&[ContextRef::Kind(ContextKind::Picker), ContextRef::Identity(7)], Some(7), &[ContextKind::Picker], true ; "and_both_match")]
    #[test_case(&[ContextRef::Kind(ContextKind::Picker), ContextRef::Identity(7)], Some(7), &[ContextKind::Chat], false ; "and_requires_every_ref")]
    fn applies_requires_every_ref(
        refs: &[ContextRef],
        identity: Option<IdentityId>,
        kinds: &[ContextKind],
        expected: bool,
    ) {
        assert_eq!(applies(refs, &active(identity, bits(kinds))), expected);
    }

    #[test_case("general", ContextRef::Kind(ContextKind::General) ; "kind_general")]
    #[test_case("picker", ContextRef::Kind(ContextKind::Picker) ; "kind_picker")]
    #[test_case("task_picker", ContextRef::Identity(0) ; "identity_task_picker")]
    #[test_case("help", ContextRef::Identity(12) ; "identity_help")]
    #[test_case("permission", ContextRef::Identity(11) ; "identity_permission")]
    fn resolve_known_names(name: &str, expected: ContextRef) {
        assert_eq!(resolve(name), Some(expected));
    }

    #[test]
    fn resolve_unknown_names() {
        assert_eq!(resolve("Picker"), None, "kind labels are lowercase");
        assert_eq!(resolve("Task Picker"), None, "v2 labels are gone");
        assert_eq!(resolve(""), None);
        assert_eq!(resolve("nonsense"), None);
    }

    #[test]
    fn all_names_covers_kinds_and_identities() {
        let names = all_names();
        for kind in ContextKind::ALL {
            assert!(
                names.contains(&kind.label()),
                "missing kind {}",
                kind.label()
            );
        }
        for identity in IDENTITIES {
            assert!(
                names.contains(&identity.name),
                "missing identity {}",
                identity.name
            );
        }
        assert_eq!(names.len(), ContextKind::ALL.len() + IDENTITIES.len());
    }

    #[test]
    fn seed_identities_have_unique_ids() {
        let mut seen = std::collections::HashSet::new();
        for (i, identity) in IDENTITIES.iter().enumerate() {
            assert_eq!(identity.id as usize, i, "id must be the array index");
            assert!(seen.insert(identity.id), "duplicate id {}", identity.id);
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
        assert_eq!(parse_context(None).unwrap(), Vec::new());
        let lua = Lua::new();
        let empty = lua.create_table().unwrap();
        assert_eq!(parse_context(Some(&empty)).unwrap(), Vec::new());

        let t = lua.create_table().unwrap();
        t.set("context", mlua::Value::Nil).unwrap();
        assert_eq!(parse_context(Some(&t)).unwrap(), Vec::new());

        let t = lua.create_table().unwrap();
        t.set("context", lua.create_table().unwrap()).unwrap();
        assert_eq!(
            parse_context(Some(&t)).unwrap(),
            Vec::new(),
            "empty list = General"
        );
    }

    #[test_case("picker", vec![ContextRef::Kind(ContextKind::Picker)] ; "kind")]
    #[test_case("task_picker", vec![ContextRef::Identity(0)] ; "identity")]
    #[test_case("  help  ", vec![ContextRef::Identity(12)] ; "trimmed")]
    fn parse_context_accepts_known_name(name: &str, expected: Vec<ContextRef>) {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("context", name).unwrap();
        assert_eq!(parse_context(Some(&t)).unwrap(), expected);
    }

    #[test]
    fn parse_context_accepts_name_list_and_semantics() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        let list = lua.create_table().unwrap();
        list.push("picker").unwrap();
        list.push("streaming").unwrap();
        t.set("context", list).unwrap();
        assert_eq!(
            parse_context(Some(&t)).unwrap(),
            vec![
                ContextRef::Kind(ContextKind::Picker),
                ContextRef::Kind(ContextKind::Streaming),
            ]
        );
    }

    #[test]
    fn parse_context_rejects_wrong_type() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("context", 42).unwrap();
        let err = parse_context(Some(&t)).unwrap_err();
        assert!(
            err.to_string().contains("opts.context must be a string"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_context_unknown_name_lists_all_and_suggests() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        t.set("context", "task_piccer").unwrap();
        let err = parse_context(Some(&t)).unwrap_err();
        let msg = err.to_string();
        for name in all_names() {
            assert!(msg.contains(name), "expected {name:?} in error: {msg}");
        }
        assert!(
            msg.contains("Did you mean task_picker?"),
            "expected did-you-mean, got: {msg}"
        );
    }

    #[test]
    fn parse_context_unknown_list_name_errors() {
        let lua = Lua::new();
        let t = lua.create_table().unwrap();
        let list = lua.create_table().unwrap();
        list.push("picker").unwrap();
        list.push("nonsense").unwrap();
        t.set("context", list).unwrap();
        let err = parse_context(Some(&t)).unwrap_err();
        assert!(err.to_string().contains("unknown context \"nonsense\""));
    }

    #[test]
    fn parse_key_notation_unknown_key_suggests() {
        let err = parse_key_notation("<C-escp>").unwrap_err();
        assert!(
            err.contains("unknown key: escp") && err.contains("Did you mean esc?"),
            "got: {err}"
        );
        let err = parse_key_notation("spce").unwrap_err();
        assert!(
            err.contains("invalid key notation: spce") && err.contains("Did you mean space?"),
            "got: {err}"
        );
        assert!(
            !parse_key_notation("xyzzy")
                .unwrap_err()
                .contains("Did you mean"),
            "no suggestion beyond distance 2"
        );
    }

    #[test]
    fn parse_rhs_string_suggests_nearest_action() {
        let lua = Lua::new();
        let s = lua.create_string("file_piccker").unwrap();
        let err = parse_rhs(&lua, mlua::Value::String(s)).unwrap_err();
        assert!(
            err.to_string()
                .contains("Did you mean maki.actions.file_picker?"),
            "got: {err}"
        );
    }

    #[test]
    fn parse_rhs_string_without_near_action_has_no_suggestion() {
        let lua = Lua::new();
        let s = lua.create_string("not a function").unwrap();
        let err = parse_rhs(&lua, mlua::Value::String(s)).unwrap_err();
        assert!(!err.to_string().contains("Did you mean"));
    }

    fn store_app(lua: &Lua) {
        lua.set_app_data(KeymapStore::new());
    }

    #[test]
    fn set_without_mode_binds_and_shadows() {
        let lua = Lua::new();
        store_app(&lua);
        let f = lua.create_function(|_, ()| Ok(())).unwrap();

        set(
            &lua,
            Arc::from("plug"),
            "<C-t>".into(),
            mlua::Value::Function(f),
            None,
        )
        .unwrap();
        assert_eq!(lua.app_data_ref::<KeymapStore>().unwrap().bindings.len(), 1);

        let f2 = lua.create_function(|_, ()| Ok(())).unwrap();
        set(
            &lua,
            Arc::from("plug2"),
            "<C-t>".into(),
            mlua::Value::Function(f2),
            None,
        )
        .unwrap();
        assert_eq!(
            lua.app_data_ref::<KeymapStore>().unwrap().bindings.len(),
            1,
            "same key replaces the old binding"
        );
    }

    #[test]
    fn set_rejects_unknown_context_with_suggestion() {
        let lua = Lua::new();
        store_app(&lua);
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let opts = lua.create_table().unwrap();
        opts.set("context", "task_piccer").unwrap();
        let err = set(
            &lua,
            Arc::from("plug"),
            "<C-t>".into(),
            mlua::Value::Function(f),
            Some(opts),
        )
        .unwrap_err();
        assert!(err.to_string().contains("Did you mean task_picker?"));
    }

    #[test]
    fn get_returns_callback_entry() {
        let lua = Lua::new();
        store_app(&lua);
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let opts = lua.create_table().unwrap();
        opts.set("desc", "my binding").unwrap();
        set(
            &lua,
            Arc::from("plug"),
            "<C-x>".into(),
            mlua::Value::Function(f),
            Some(opts),
        )
        .unwrap();

        let t = get(&lua, Arc::from("plug"), "<C-x>".into())
            .unwrap()
            .unwrap();
        assert_eq!(t.get::<String>("kind").unwrap(), "callback");
        assert_eq!(t.get::<mlua::Value>("action").unwrap(), mlua::Value::Nil);
        assert_eq!(t.get::<String>("context").unwrap(), "General");
        assert_eq!(t.get::<String>("desc").unwrap(), "my binding");
        assert_eq!(t.get::<String>("plugin").unwrap(), "plug");
    }

    #[test]
    fn get_returns_builtin_entry_with_context_names() {
        let lua = Lua::new();
        store_app(&lua);
        let actions = crate::api::actions::create_actions_table(&lua).unwrap();
        let ud: mlua::AnyUserData = actions.get("scroll_top").unwrap();
        let opts = lua.create_table().unwrap();
        let list = lua.create_table().unwrap();
        list.push("picker").unwrap();
        list.push("task_picker").unwrap();
        opts.set("context", list).unwrap();
        set(
            &lua,
            Arc::from("plug"),
            "<C-g>".into(),
            mlua::Value::UserData(ud),
            Some(opts),
        )
        .unwrap();

        let t = get(&lua, Arc::from("plug"), "<C-g>".into())
            .unwrap()
            .unwrap();
        assert_eq!(t.get::<String>("kind").unwrap(), "builtin");
        assert_eq!(t.get::<String>("action").unwrap(), "scroll_top");
        assert_eq!(
            t.get::<String>("context").unwrap(),
            "picker, task_picker",
            "kind labels and identity names joined"
        );
    }

    #[test]
    fn get_returns_nil_when_unmapped() {
        let lua = Lua::new();
        store_app(&lua);
        assert!(
            get(&lua, Arc::from("plug"), "<C-x>".into())
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn del_without_mode_removes_binding() {
        let lua = Lua::new();
        store_app(&lua);
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        set(
            &lua,
            Arc::from("plug"),
            "<C-t>".into(),
            mlua::Value::Function(f),
            None,
        )
        .unwrap();
        assert!(
            get(&lua, Arc::from("plug"), "<C-t>".into())
                .unwrap()
                .is_some()
        );

        del(&lua, Arc::from("plug"), "<C-t>".into()).unwrap();
        assert!(
            get(&lua, Arc::from("plug"), "<C-t>".into())
                .unwrap()
                .is_none()
        );

        del(&lua, Arc::from("plug"), "<C-t>".into()).unwrap();
    }

    /// Real-boot drift gate for `plugins/keymap/init.lua`: boots the full
    /// builtin set, waits for the keymap plugin's entries to be published,
    /// and asserts the loaded store matches the documented defaults. The
    /// `todo_write` builtin registers its own General `<C-t>` callback and
    /// loads after `keymap`, so it shadows `plan_toggle` in the shared
    /// store (last set wins) — the snapshot pins that state too. Replaces
    /// v2's Rust mirror const + regex parse — verification is execution,
    /// not parsing, and catches accidental edits to the plugin file or to
    /// the builtins' load order.
    #[test]
    fn booted_default_keymap_matches_documented_set() {
        use std::collections::BTreeSet;
        use std::time::{Duration, Instant};

        const EXPECTED: &[(&str, &str)] = &[
            ("<C-c>", "quit"),
            ("<C-h>", "help"),
            ("<C-p>", "prev_chat"),
            ("<C-n>", "next_chat"),
            ("<C-u>", "scroll_half_up"),
            ("<C-d>", "scroll_half_down"),
            ("<C-g>", "scroll_top"),
            ("<C-b>", "scroll_bottom"),
            ("<C-t>", "callback:todo_write"),
            ("<C-x>", "tasks"),
            ("<C-f>", "search"),
            ("<C-s>", "file_picker"),
            ("<C-o>", "open_editor"),
            ("<M-o>", "edit_input"),
            ("<C-q>", "pop_queue"),
        ];

        let host =
            crate::PluginHost::with_all_builtins(Arc::new(maki_agent::tools::ToolRegistry::new()))
                .expect("loading builtins");
        let deadline = Instant::now() + Duration::from_secs(10);
        let entries = loop {
            let snap = host.keymap_reader().load();
            if !snap.entries.is_empty() {
                break snap.entries.clone();
            }
            assert!(
                Instant::now() < deadline,
                "keymap plugin entries never appeared"
            );
            std::thread::sleep(Duration::from_millis(20));
        };

        let actual: BTreeSet<_> = entries
            .iter()
            .map(|e| {
                let label = format!("{:?}/{:?}", e.key, e.modifiers);
                match e.kind {
                    EntryKind::Builtin(a) => (label, a.lua_name().to_string()),
                    EntryKind::Callback => (label, format!("callback:{}", e.plugin)),
                }
            })
            .collect();
        let expected: BTreeSet<_> = EXPECTED
            .iter()
            .map(|(notation, name)| {
                let (key, modifiers) =
                    parse_key_notation(notation).expect("default key notation parses");
                (format!("{key:?}/{modifiers:?}"), name.to_string())
            })
            .collect();
        assert_eq!(
            expected, actual,
            "plugins/keymap/init.lua drifted from the documented default set"
        );
    }
}
