use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use arc_swap::ArcSwap;
use crossterm::event::{KeyCode, KeyModifiers};
use maki_lua_macro::{lua_fn, lua_table};
use mlua::{Lua, RegistryKey, Result as LuaResult, Table};

use crate::api::util::command::BuiltinAction;

static NEXT_KEYMAP_ID: AtomicU64 = AtomicU64::new(1);

pub const BUILTIN_PLUGIN: &str = "maki.builtin";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinDefaultBinding {
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
    pub action: BuiltinAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handler {
    Lua(u64),
    Builtin(BuiltinAction),
}

#[derive(Clone, Debug)]
pub struct KeymapEntry {
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
    pub desc: String,
    pub plugin: Arc<str>,
    pub handler: Handler,
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

pub(crate) struct StoredKeymap {
    pub key: KeyCode,
    pub modifiers: KeyModifiers,
    pub plugin: Arc<str>,
    pub desc: String,
    pub handler: StoredHandler,
}

pub(crate) enum StoredHandler {
    Lua { id: u64, callback: RegistryKey },
    Builtin(BuiltinAction),
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

    pub fn set_lua(
        &mut self,
        key: KeyCode,
        modifiers: KeyModifiers,
        callback: RegistryKey,
        plugin: Arc<str>,
        desc: String,
    ) -> (u64, Option<RegistryKey>) {
        let id = NEXT_KEYMAP_ID.fetch_add(1, Ordering::Relaxed);
        let old = self.remove_at(key, modifiers);
        self.bindings.push(StoredKeymap {
            key,
            modifiers,
            plugin,
            desc,
            handler: StoredHandler::Lua { id, callback },
        });
        (id, old)
    }

    pub fn set_builtin(
        &mut self,
        key: KeyCode,
        modifiers: KeyModifiers,
        action: BuiltinAction,
        plugin: Arc<str>,
        desc: String,
    ) -> Vec<RegistryKey> {
        let mut dropped = self.remove_at(key, modifiers).into_iter().collect::<Vec<_>>();
        let mut i = 0;
        while i < self.bindings.len() {
            if let StoredHandler::Builtin(existing) = &self.bindings[i].handler
                && *existing == action
            {
                dropped.extend(self.bindings.remove(i).callback_if_lua());
            } else {
                i += 1;
            }
        }
        self.bindings.push(StoredKeymap {
            key,
            modifiers,
            plugin,
            desc,
            handler: StoredHandler::Builtin(action),
        });
        dropped
    }

    pub fn del(&mut self, key: KeyCode, modifiers: KeyModifiers) -> Option<RegistryKey> {
        self.bindings
            .iter()
            .position(|b| b.key == key && b.modifiers == modifiers)
            .and_then(|pos| self.bindings.remove(pos).callback_if_lua())
    }

    pub fn del_action(&mut self, action: BuiltinAction) -> Vec<RegistryKey> {
        let mut dropped = Vec::new();
        let mut i = 0;
        while i < self.bindings.len() {
            if let StoredHandler::Builtin(existing) = &self.bindings[i].handler
                && *existing == action
            {
                dropped.extend(self.bindings.remove(i).callback_if_lua());
            } else {
                i += 1;
            }
        }
        dropped
    }

    pub fn clear_plugin(&mut self, plugin: &str) -> Vec<RegistryKey> {
        if plugin == BUILTIN_PLUGIN {
            return Vec::new();
        }
        let mut keys = Vec::new();
        let mut i = 0;
        while i < self.bindings.len() {
            if self.bindings[i].plugin.as_ref() == plugin {
                keys.extend(self.bindings.remove(i).callback_if_lua());
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
                handler: match &b.handler {
                    StoredHandler::Lua { id, .. } => Handler::Lua(*id),
                    StoredHandler::Builtin(action) => Handler::Builtin(*action),
                },
            })
            .collect()
    }

    pub fn callback_for_id(&self, id: u64) -> Option<&RegistryKey> {
        self.bindings
            .iter()
            .find_map(|b| match &b.handler {
                StoredHandler::Lua { id: lid, callback } if *lid == id => Some(callback),
                _ => None,
            })
    }

    fn remove_at(&mut self, key: KeyCode, modifiers: KeyModifiers) -> Option<RegistryKey> {
        self.bindings
            .iter()
            .position(|b| b.key == key && b.modifiers == modifiers)
            .and_then(|pos| self.bindings.remove(pos).callback_if_lua())
    }
}

impl StoredKeymap {
    fn callback_if_lua(self) -> Option<RegistryKey> {
        match self.handler {
            StoredHandler::Lua { callback, .. } => Some(callback),
            StoredHandler::Builtin(_) => None,
        }
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

pub(crate) fn publish_keymap_snapshot(lua: &Lua) {
    if let Some(store) = lua.app_data_ref::<KeymapStore>() {
        let entries = store.snapshot_entries();
        if let Some(writer) = lua.app_data_ref::<KeymapWriter>() {
            writer.publish(entries);
        }
    }
}

/// Bind {lhs} to {rhs} in {mode}. Only normal mode (`"n"`) is
/// supported right now. {rhs} is either a Lua function (called when
/// the key is pressed) or a string naming a built-in UI action such
/// as `"FilePicker"`, `"EditInputInEditor"`, or `"OpenEditor"`. A
/// built-in rhs moves the binding: any existing binding for the same
/// built-in action at another key is removed.
///
/// @param mode string Mode letter. Currently only `"n"` is accepted.
/// @param lhs string Key in Vim notation, e.g. `"<C-t>"`, `"<Space>"`, `"a"`.
/// @param rhs function|string Lua callback or built-in action name.
/// @param opts table? Options:
///   `desc` (string) short description shown in the keymap list.
/// @example
/// maki.keymap.set("n", "<A-y>", "FilePicker")
/// maki.keymap.set("n", "<C-t>", function()
///   print("toggle!")
/// end, { desc = "Toggle panel" })
#[lua_fn]
fn set(
    lua: &Lua,
    #[ctx] plugin: Arc<str>,
    mode: String,
    lhs: String,
    rhs: mlua::Value,
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
    let dropped: Vec<RegistryKey> = match rhs {
        mlua::Value::Function(func) => {
            let registry_key = lua.create_registry_value(func)?;
            let (_, old) = lua
                .app_data_mut::<KeymapStore>()
                .ok_or_else(|| mlua::Error::runtime("keymap store not initialized"))?
                .set_lua(key, modifiers, registry_key, Arc::clone(&plugin), desc);
            old.into_iter().collect()
        }
        mlua::Value::String(s) => {
            let name = s.to_string_lossy();
            let action = BuiltinAction::parse(&name).map_err(mlua::Error::runtime)?;
            lua.app_data_mut::<KeymapStore>()
                .ok_or_else(|| mlua::Error::runtime("keymap store not initialized"))?
                .set_builtin(key, modifiers, action, Arc::clone(&plugin), desc)
        }
        other => {
            return Err(mlua::Error::runtime(format!(
                "rhs must be a function or a built-in action name (string), got {}",
                other.type_name()
            )));
        }
    };
    if !dropped.is_empty() {
        tracing::warn!(key = %lhs, plugin = %plugin, "keymap shadowed by plugin");
        for old_key in dropped {
            let _ = lua.remove_registry_value(old_key);
        }
    }
    publish_keymap_snapshot(lua);
    Ok(())
}

/// Remove the mapping for {lhs} in {mode}. Does nothing if no mapping
/// exists for that key. Does not restore a default binding.
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

/// Disable a built-in UI action by name, regardless of which key it
/// is currently bound to. Useful when another plugin has moved the
/// binding and you want to disable it without tracking its current key.
///
/// @param mode string Mode letter (reserved for future modes).
/// @param name string Built-in action name, e.g. `"FilePicker"`.
/// @example
/// maki.keymap.del_action("n", "FilePicker")
#[lua_fn]
fn del_action(lua: &Lua, mode: String, name: String) -> LuaResult<()> {
    let _ = mode;
    let action = BuiltinAction::parse(&name).map_err(mlua::Error::runtime)?;
    let dropped = lua
        .app_data_mut::<KeymapStore>()
        .map(|mut store| store.del_action(action))
        .unwrap_or_default();
    for old_key in dropped {
        let _ = lua.remove_registry_value(old_key);
    }
    publish_keymap_snapshot(lua);
    Ok(())
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
        set(plugin), del(plugin), del_action,
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
        let (id1, old1) = store.set_lua(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
            k1,
            Arc::from("plug"),
            "toggle".into(),
        );
        assert!(old1.is_none());

        let f2 = lua.create_function(|_, ()| Ok(())).unwrap();
        let k2 = lua.create_registry_value(f2).unwrap();
        let (id2, old2) = store.set_lua(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
            k2,
            Arc::from("plug2"),
            "toggle v2".into(),
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
        store.set_lua(
            KeyCode::Char('x'),
            KeyModifiers::ALT,
            k,
            Arc::from("p"),
            String::new(),
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
        store.set_lua(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
            k1,
            Arc::from("a"),
            String::new(),
        );
        store.set_lua(
            KeyCode::Char('x'),
            KeyModifiers::CONTROL,
            k2,
            Arc::from("b"),
            String::new(),
        );

        let removed = store.clear_plugin("a");
        assert_eq!(removed.len(), 1);
        assert_eq!(store.bindings.len(), 1);
        assert_eq!(store.bindings[0].plugin.as_ref(), "b");
    }

    #[test]
    fn set_builtin_moves_binding_no_duplicate_variant() {
        let mut store = KeymapStore::new();
        store.set_builtin(
            KeyCode::Char('o'),
            KeyModifiers::ALT,
            BuiltinAction::EditInputInEditor,
            Arc::from(BUILTIN_PLUGIN),
            String::new(),
        );
        store.set_builtin(
            KeyCode::Char('e'),
            KeyModifiers::ALT,
            BuiltinAction::EditInputInEditor,
            Arc::from("user"),
            String::new(),
        );

        let entries = store.snapshot_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, KeyCode::Char('e'));
        assert_eq!(entries[0].handler, Handler::Builtin(BuiltinAction::EditInputInEditor));
    }

    #[test]
    fn set_builtin_replaces_lua_at_same_key() {
        let lua = Lua::new();
        let mut store = KeymapStore::new();
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let k = lua.create_registry_value(f).unwrap();
        store.set_lua(
            KeyCode::Char('e'),
            KeyModifiers::ALT,
            k,
            Arc::from("user"),
            String::new(),
        );

        let dropped = store.set_builtin(
            KeyCode::Char('e'),
            KeyModifiers::ALT,
            BuiltinAction::OpenEditor,
            Arc::from("user"),
            String::new(),
        );
        assert_eq!(dropped.len(), 1);
        assert_eq!(store.bindings.len(), 1);
        assert_eq!(
            store.snapshot_entries()[0].handler,
            Handler::Builtin(BuiltinAction::OpenEditor)
        );
    }

    #[test]
    fn set_lua_does_not_move_builtin_at_other_key() {
        let lua = Lua::new();
        let mut store = KeymapStore::new();
        store.set_builtin(
            KeyCode::Char('e'),
            KeyModifiers::ALT,
            BuiltinAction::EditInputInEditor,
            Arc::from(BUILTIN_PLUGIN),
            String::new(),
        );
        let f = lua.create_function(|_, ()| Ok(())).unwrap();
        let k = lua.create_registry_value(f).unwrap();
        store.set_lua(
            KeyCode::Char('q'),
            KeyModifiers::ALT,
            k,
            Arc::from("user"),
            String::new(),
        );

        let entries = store.snapshot_entries();
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.handler
            == Handler::Builtin(BuiltinAction::EditInputInEditor)
            && e.key == KeyCode::Char('e')));
    }

    #[test]
    fn del_action_removes_builtin_at_any_key() {
        let mut store = KeymapStore::new();
        store.set_builtin(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
            BuiltinAction::FilePicker,
            Arc::from(BUILTIN_PLUGIN),
            String::new(),
        );
        store.set_builtin(
            KeyCode::Char('y'),
            KeyModifiers::ALT,
            BuiltinAction::FilePicker,
            Arc::from("user"),
            String::new(),
        );

        let cleared = store.del_action(BuiltinAction::FilePicker);
        assert!(cleared.is_empty());
        assert!(store.bindings.is_empty());
    }

    #[test]
    fn clear_plugin_skips_builtin_defaults() {
        let mut store = KeymapStore::new();
        store.set_builtin(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
            BuiltinAction::FilePicker,
            Arc::from(BUILTIN_PLUGIN),
            String::new(),
        );

        let removed = store.clear_plugin(BUILTIN_PLUGIN);
        assert!(removed.is_empty());
        assert_eq!(store.bindings.len(), 1);
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
            handler: Handler::Lua(1),
        }]);

        let snap = reader.load();
        assert_eq!(snap.entries.len(), 1);
        assert_eq!(snap.generation, 1);
    }
}
