use crate::docs::{DocKind, FnDoc, ModuleDoc};
use mlua::{Lua, Result as LuaResult, Table, UserData, UserDataMethods};

pub(crate) const DOCS: ModuleDoc = ModuleDoc {
    name: "maki.actions",
    kind: DocKind::Table,
    desc: "Closed-set of maki builtin action handles. Each field is an opaque userdata: pass it as the third argument to maki.keymap.set to bind a builtin action without a per-keypress Lua callback. Constructed in Rust; cannot be forged from Lua.",
    fns: &ACTION_FN_DOCS,
};

const fn action_doc(name: &'static str, desc: &'static str) -> FnDoc {
    FnDoc {
        name,
        args: "",
        desc,
        params: &[],
        returns: "",
        example: "",
    }
}

const ACTION_FN_DOCS: [FnDoc; BuiltinAction::ALL.len()] = [
    action_doc("quit", "Quit / clear input"),
    action_doc("help", "Show keybindings"),
    action_doc("prev_chat", "Previous task chat"),
    action_doc("next_chat", "Next task chat"),
    action_doc("scroll_half_up", "Scroll half page up"),
    action_doc("scroll_half_down", "Scroll half page down"),
    action_doc("scroll_line_up", "Scroll one line up"),
    action_doc("scroll_line_down", "Scroll one line down"),
    action_doc("scroll_top", "Scroll to top"),
    action_doc("scroll_bottom", "Scroll to bottom"),
    action_doc("plan_toggle", "Toggle plan panel"),
    action_doc("tasks", "Open tasks"),
    action_doc("search", "Search messages"),
    action_doc("file_picker", "Open file picker"),
    action_doc("open_editor", "Open plan in editor"),
    action_doc("edit_input", "Edit input in editor"),
    action_doc("pop_queue", "Pop queued message"),
    action_doc("new_session", "New session"),
    action_doc("compact", "Compact context"),
    action_doc("model_picker", "Open model picker"),
    action_doc("theme_picker", "Open theme picker"),
    action_doc("mcp_picker", "Open MCP picker"),
    action_doc("usage", "Open usage modal"),
    action_doc("refresh", "Refresh focused picker"),
    action_doc("reload", "Reload maki"),
];

/// Closed-set identifier for a maki builtin action. Single source of
/// truth for the `maki.actions.<name>` Lua table, the keymap RHS
/// parser, `App::dispatch_builtin`, the help modal, and docgen.
///
/// Each entry is `(variant, lua_name, description)`. `lua_name` is
/// the field name under `maki.actions.*`; `description` is the
/// human-readable label used by the help modal and the generated docs.
/// Variants without a Lua-side entry cannot be bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinAction {
    Quit,
    Help,
    PrevChat,
    NextChat,
    ScrollHalfUp,
    ScrollHalfDown,
    ScrollLineUp,
    ScrollLineDown,
    ScrollTop,
    ScrollBottom,
    PlanToggle,
    Tasks,
    Search,
    FilePicker,
    OpenEditor,
    EditInput,
    PopQueue,
    NewSession,
    Compact,
    ModelPicker,
    ThemePicker,
    McpPicker,
    UsageModal,
    Refresh,
    Reload,
}

impl BuiltinAction {
    pub const ALL: &[(BuiltinAction, &'static str, &'static str)] = &[
        (BuiltinAction::Quit, "quit", "Quit / clear input"),
        (BuiltinAction::Help, "help", "Show keybindings"),
        (BuiltinAction::PrevChat, "prev_chat", "Previous task chat"),
        (BuiltinAction::NextChat, "next_chat", "Next task chat"),
        (
            BuiltinAction::ScrollHalfUp,
            "scroll_half_up",
            "Scroll half page up",
        ),
        (
            BuiltinAction::ScrollHalfDown,
            "scroll_half_down",
            "Scroll half page down",
        ),
        (
            BuiltinAction::ScrollLineUp,
            "scroll_line_up",
            "Scroll one line up",
        ),
        (
            BuiltinAction::ScrollLineDown,
            "scroll_line_down",
            "Scroll one line down",
        ),
        (BuiltinAction::ScrollTop, "scroll_top", "Scroll to top"),
        (
            BuiltinAction::ScrollBottom,
            "scroll_bottom",
            "Scroll to bottom",
        ),
        (
            BuiltinAction::PlanToggle,
            "plan_toggle",
            "Toggle plan panel",
        ),
        (BuiltinAction::Tasks, "tasks", "Open tasks"),
        (BuiltinAction::Search, "search", "Search messages"),
        (BuiltinAction::FilePicker, "file_picker", "Open file picker"),
        (
            BuiltinAction::OpenEditor,
            "open_editor",
            "Open plan in editor",
        ),
        (
            BuiltinAction::EditInput,
            "edit_input",
            "Edit input in editor",
        ),
        (BuiltinAction::PopQueue, "pop_queue", "Pop queued message"),
        (BuiltinAction::NewSession, "new_session", "New session"),
        (BuiltinAction::Compact, "compact", "Compact context"),
        (
            BuiltinAction::ModelPicker,
            "model_picker",
            "Open model picker",
        ),
        (
            BuiltinAction::ThemePicker,
            "theme_picker",
            "Open theme picker",
        ),
        (BuiltinAction::McpPicker, "mcp_picker", "Open MCP picker"),
        (BuiltinAction::UsageModal, "usage", "Open usage modal"),
        (BuiltinAction::Refresh, "refresh", "Refresh focused picker"),
        (BuiltinAction::Reload, "reload", "Reload maki"),
    ];

    pub fn lua_name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(a, _, _)| *a == self)
            .map(|(_, n, _)| *n)
            .expect("every BuiltinAction variant has an entry in ALL")
    }

    pub fn description(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(a, _, _)| *a == self)
            .map(|(_, _, d)| *d)
            .expect("every BuiltinAction variant has an entry in ALL")
    }

    pub fn from_lua_name(s: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find_map(|(action, name, _)| (*name == s).then_some(*action))
    }
}

/// Opaque handle to a builtin action identity. Pass it as the second
/// argument to `maki.keymap.set` instead of a function:
///
/// ```lua
/// maki.keymap.set("<C-c>", maki.actions.quit)
/// ```
///
/// Cannot be constructed from Lua. The only way to obtain one is via
/// `maki.actions.<name>`. A typo'd name (`maki.actions.file_piccker`)
/// is `nil` and is rejected by `maki.keymap.set` at registration time.
#[derive(Clone, Copy)]
pub struct LuaBuiltinAction(pub BuiltinAction);

impl UserData for LuaBuiltinAction {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        m.add_meta_method(mlua::MetaMethod::ToString, |_, this: &Self, ()| {
            Ok(format!("maki.actions.{}", this.0.lua_name()))
        });
    }
}

pub(crate) fn create_actions_table(lua: &Lua) -> LuaResult<Table> {
    let t = lua.create_table()?;
    for (action, lua_name, _desc) in BuiltinAction::ALL {
        t.set(*lua_name, LuaBuiltinAction(*action))?;
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    #[test_case(BuiltinAction::Quit, "quit" ; "quit")]
    #[test_case(BuiltinAction::ScrollHalfUp, "scroll_half_up" ; "scroll_half_up")]
    #[test_case(BuiltinAction::McpPicker, "mcp_picker" ; "mcp_picker")]
    #[test_case(BuiltinAction::Refresh, "refresh" ; "refresh")]
    fn lua_name_roundtrips(action: BuiltinAction, name: &str) {
        assert_eq!(action.lua_name(), name);
        assert_eq!(
            BuiltinAction::from_lua_name(action.lua_name()),
            Some(action)
        );
    }

    #[test]
    fn all_entries_have_unique_names() {
        let mut seen = std::collections::HashSet::new();
        for (_, name, _) in BuiltinAction::ALL {
            assert!(seen.insert(*name), "duplicate lua name: {name}");
        }
    }

    #[test]
    fn from_lua_name_unknown_returns_none() {
        assert!(BuiltinAction::from_lua_name("file_piccker").is_none());
        assert!(BuiltinAction::from_lua_name("").is_none());
    }

    #[test]
    fn actions_table_has_every_variant() {
        let lua = Lua::new();
        let t = create_actions_table(&lua).unwrap();
        for (action, name, _) in BuiltinAction::ALL {
            let ud: mlua::AnyUserData = t.get(*name).unwrap();
            let handle = *ud.borrow::<LuaBuiltinAction>().unwrap();
            assert_eq!(handle.0, *action, "mismatch for {name}");
        }
    }

    #[test]
    fn actions_table_typo_is_nil() {
        let lua = Lua::new();
        let t = create_actions_table(&lua).unwrap();
        let got: mlua::Value = t.get("file_piccker").unwrap();
        assert_eq!(got, mlua::Value::Nil);
    }

    #[test]
    fn to_string_emits_dotted_name() {
        let lua = Lua::new();
        let t = create_actions_table(&lua).unwrap();
        lua.globals().set("maki_actions", t).unwrap();
        let s: String = lua
            .load("return tostring(maki_actions.help)")
            .eval()
            .unwrap();
        assert_eq!(s, "maki.actions.help");
    }
}
