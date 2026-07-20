//! `maki.action`: fire-and-forget runner for built-in UI actions. The action
//! runs asynchronously on the UI thread, so callers cannot observe completion;
//! for synchronous remap semantics use `maki.keymap.set(mode, lhs, "<Name>")`.

use maki_lua_macro::{lua_fn, lua_table};
use mlua::{Lua, Result as LuaResult, Value};

use crate::api::util::command::{BuiltinAction, UiAction};

const NO_UI_ERR: &str = "no interactive UI attached";

type Pair = (Value, Option<String>);

fn err_pair(err: impl ToString) -> Pair {
    (Value::Nil, Some(err.to_string()))
}

/// Fire a built-in UI action by name. Fire-and-forget: the call
/// returns immediately and the action runs on the UI thread; callers
/// cannot observe completion or ordering relative to subsequent keys.
///
/// Names match `BuiltinAction` variants exactly and are case-sensitive:
/// `"EditInputInEditor"`, `"FilePicker"`, `"OpenEditor"`. Use this to
/// compose built-ins inside your own Lua callbacks; for synchronous
/// remap of a built-in to a key, prefer
/// `maki.keymap.set(mode, lhs, "<Name>")`.
///
/// @param name string Built-in action name.
/// @return (nil|boolean, string|nil) true on success, or (nil, err).
/// @example
/// maki.keymap.set("n", "<Leader>e", function()
///   maki.action.run("EditInputInEditor")
/// end)
#[lua_fn]
fn run(
    _lua: &Lua,
    #[ctx] tx: Option<flume::Sender<UiAction>>,
    name: String,
) -> LuaResult<Pair> {
    let action = match BuiltinAction::parse(&name) {
        Ok(a) => a,
        Err(e) => return Ok(err_pair(e)),
    };
    match tx.as_ref() {
        Some(tx) if !tx.is_disconnected() => {
            let _ = tx.try_send(UiAction::InvokeBuiltin { action });
            Ok((Value::Boolean(true), None))
        }
        _ => Ok(err_pair(NO_UI_ERR)),
    }
}

lua_table! {
    /// Fire-and-forget runner for built-in UI actions, useful for
    /// composing built-ins inside your own Lua callbacks.
    ///
    /// ```lua
    /// maki.keymap.set("n", "<Leader>e", function()
    ///   maki.action.run("EditInputInEditor")
    /// end)
    /// ```
    "maki.action" => pub(crate) fn create_action_table(tx: Option<flume::Sender<UiAction>>), DOCS [
        run(tx),
    ]
}
