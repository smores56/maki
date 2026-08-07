mod api;
pub mod docs;
pub mod docs_render;
mod error;
pub mod language;
mod loader;
pub(crate) mod plugin_permissions;
mod runtime;

pub use api::actions::BuiltinAction;
pub use api::context::{
    ActiveContext, ContextKind, ContextRef, IDENTITIES, Identity, IdentityId, applies,
    identity_kind, tier,
};
pub use api::keymap::{EntryKind, KeymapEntry, KeymapReader, KeymapSnapshot};
pub use api::options::{OptionSpec, OptionType, PluginOptionSpecs};
pub use api::util::command::{
    Anchor, Axis, Border, Dimension, Edge, FloatConfig, FloatConfigPatch, HintReader, HintSnapshot,
    LuaCommandInfo, LuaCommandReader, SessionReply, SessionRequest, Split, TitlePos, UiAction,
    WinCommand, WinEvent, WinView,
};
pub use docs::{DocKind, FnDoc, ModuleDoc, ParamDoc, api_docs};
pub use error::PluginError;
pub use loader::{EventHandle, PluginHost};
pub use plugin_permissions::{Permission, PluginPermissions};
pub use runtime::{KILL_GRACE, RestoreItem, WARM_TOOL_CAP};

pub mod test_support {
    use crate::KeymapReader;
    use crate::PluginHost;
    use crate::api::keymap::{KeymapEntry, KeymapWriter};
    use crate::api::util::command::{LuaCommandInfo, LuaCommandReader, LuaCommandWriter};
    use std::sync::Arc;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};

    pub struct LuaCommandWriterHandle(LuaCommandWriter);

    impl LuaCommandWriterHandle {
        pub fn publish(&self, commands: Vec<LuaCommandInfo>) {
            self.0.publish(commands);
        }
    }

    pub fn lua_command_writer_pair() -> (LuaCommandWriterHandle, LuaCommandReader) {
        let (writer, reader) = LuaCommandWriter::new();
        (LuaCommandWriterHandle(writer), reader)
    }

    /// Observes which requests an [`crate::EventHandle`] sends, without a
    /// running plugin host.
    pub struct RequestProbe(flume::Receiver<crate::runtime::Request>);

    impl RequestProbe {
        /// Next request as `(kind, clicks)`: `"click"` carries no clicks,
        /// `"click_fallback"` and `"restore"` carry their restore item's.
        pub fn try_recv(&self) -> Option<(&'static str, Vec<usize>)> {
            use crate::runtime::Request;
            Some(match self.0.try_recv().ok()? {
                Request::ClickTool { fallback: None, .. } => ("click", Vec::new()),
                Request::ClickTool {
                    fallback: Some(fb), ..
                } => ("click_fallback", fb.item.clicks),
                Request::RestoreToolAsync { item, .. } => ("restore", item.clicks),
                _ => ("other", Vec::new()),
            })
        }

        /// Next fired autocmd as `(event, data)`, skipping other requests.
        pub fn try_recv_autocmd(&self) -> Option<(String, serde_json::Value)> {
            use crate::runtime::Request;
            while let Ok(req) = self.0.try_recv() {
                if let Request::FireAutocmd { event, data } = req {
                    return Some((event, data));
                }
            }
            None
        }
    }

    pub fn probed_event_handle() -> (crate::EventHandle, RequestProbe) {
        let (tx, rx) = flume::unbounded();
        (crate::EventHandle::probed_for_test(tx), RequestProbe(rx))
    }

    pub fn keymap_reader_with(entries: Vec<KeymapEntry>) -> KeymapReader {
        let (writer, reader) = KeymapWriter::new();
        writer.publish(entries);
        reader
    }

    static DEFAULT_KEYMAP_ENTRIES: OnceLock<Vec<KeymapEntry>> = OnceLock::new();

    /// The bindings `plugins/keymap/init.lua` registers at startup, from a
    /// real one-time host boot (cached). Test harnesses seed their
    /// `KeymapReader` with these so default keys (quit/help/tasks/scrolls)
    /// dispatch without a live Lua runtime; the boot doubles as the drift
    /// gate that catches accidental changes to the plugin.
    pub fn loaded_default_keymap_entries() -> Vec<KeymapEntry> {
        DEFAULT_KEYMAP_ENTRIES
            .get_or_init(|| {
                let host =
                    PluginHost::with_all_builtins(Arc::new(maki_agent::tools::ToolRegistry::new()))
                        .expect("loading builtins for the default keymap");
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    let entries = host.keymap_reader().load().entries.clone();
                    if !entries.is_empty() {
                        return entries;
                    }
                    assert!(
                        Instant::now() < deadline,
                        "keymap plugin entries never appeared"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
            })
            .clone()
    }
}
