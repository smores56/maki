use maki_agent::tools::ToolRegistry;
use maki_lua::{ContextKind, ContextRef, EntryKind, IDENTITIES, KeymapEntry, PluginHost};
use maki_ui::keybindings::{
    ALT_SEP, KEYBINDS, KeyLabel, Platform, display_key_label, entry_in_kind, section_title,
};
use std::sync::Arc;
use std::time::{Duration, Instant};

const FRONTMATTER: &str = "\
+++
title = \"Keybindings\"
weight = 5
[extra]
group = \"Reference\"
+++";

const LUA_CONTEXT_BINDS: &[(&str, &str, &str)] = &[
    ("Session Picker", "`Ctrl+N`", "New session"),
    ("Session Picker", "`Ctrl+R`", "Rename session"),
    ("Session Picker", "`Ctrl+D`", "Delete session (press twice)"),
];

const MAIN_CONTEXTS: &[ContextKind] = &[
    ContextKind::General,
    ContextKind::Chat,
    ContextKind::Streaming,
    ContextKind::Form,
    ContextKind::Picker,
];

fn label_str(label: KeyLabel) -> String {
    match label {
        KeyLabel::Single(s) => format!("`{s}`"),
        KeyLabel::Alt(a, b) => format!("`{a}`{ALT_SEP}`{b}`"),
        KeyLabel::MacAlt(a, _) => format!("`{a}`"),
        KeyLabel::MacMulti(normal, _) => normal
            .iter()
            .map(|s| format!("`{s}`"))
            .collect::<Vec<_>>()
            .join(ALT_SEP),
    }
}

fn context_label(context: ContextRef) -> String {
    match context {
        ContextRef::Kind(kind) => section_title(kind),
        ContextRef::Identity(id) => IDENTITIES
            .iter()
            .find(|i| i.id == id)
            .expect("identity must exist in the seed table")
            .name
            .to_string(),
    }
}

fn write_table_2col(out: &mut String, rows: &[(String, String)]) {
    out.push_str("| Key | Action |\n|-----|--------|\n");
    for (key, desc) in rows {
        out.push_str(&format!("| {key} | {desc} |\n"));
    }
}

/// The bindings `plugins/keymap/init.lua` registered at startup, from a
/// real boot of the full builtin set. The docs render from the loaded
/// store so they can never drift from dispatch.
fn boot_default_keymap_entries() -> Vec<KeymapEntry> {
    let host = PluginHost::with_all_builtins(Arc::new(ToolRegistry::new()))
        .expect("loading builtins for the keybindings docs");
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
}

fn write_section(out: &mut String, ctx: ContextKind, defaults: &[KeymapEntry]) {
    out.push_str(&format!("\n## {}\n\n", section_title(ctx)));

    let default_rows: Vec<_> = defaults
        .iter()
        .filter(|e| entry_in_kind(e, ctx))
        .map(|e| {
            let key = format!("`{}`", display_key_label(e.key, e.modifiers));
            let desc = match &e.kind {
                EntryKind::Builtin(action) => action.description().to_string(),
                EntryKind::Callback if e.desc.is_empty() => {
                    format!("[{}] callback", e.plugin)
                }
                EntryKind::Callback => format!("{} ({})", e.desc, e.plugin),
            };
            (key, desc)
        })
        .collect();

    let all_rows: Vec<_> = KEYBINDS
        .iter()
        .filter(|kb| kb.context == ContextRef::Kind(ctx))
        .collect();

    let normal: Vec<_> = default_rows
        .iter()
        .cloned()
        .chain(
            all_rows
                .iter()
                .filter(|kb| kb.platform == Platform::All)
                .map(|kb| (label_str(kb.label), kb.description.to_string())),
        )
        .collect();

    if !normal.is_empty() {
        write_table_2col(out, &normal);
    }

    let mac_only: Vec<_> = all_rows
        .iter()
        .filter(|kb| kb.platform == Platform::MacOnly)
        .map(|kb| (label_str(kb.label), kb.description.to_string()))
        .collect();

    if !mac_only.is_empty() {
        out.push_str("\n### macOS-specific\n\n");
        write_table_2col(out, &mac_only);
    }

    for identity in IDENTITIES.iter().filter(|i| i.kind == ctx) {
        let identity_rows: Vec<_> = KEYBINDS
            .iter()
            .filter(|kb| kb.context == ContextRef::Identity(identity.id))
            .collect();
        if identity_rows.is_empty() {
            continue;
        }
        out.push_str(&format!("\n### {}\n\n", identity.name));
        let normal: Vec<_> = identity_rows
            .iter()
            .filter(|kb| kb.platform == Platform::All)
            .map(|kb| (label_str(kb.label), kb.description.to_string()))
            .collect();
        if !normal.is_empty() {
            write_table_2col(out, &normal);
        }
    }
}

fn write_fixed(out: &mut String) {
    out.push_str("\n## Fixed\n\n");
    out.push_str(
        "Escape hatches hardcoded at the top of `App::handle_key`, above \
         the keymap: not remappable, not routable.\n\n",
    );
    write_table_2col(
        out,
        &[
            (
                "`Ctrl+Z`".to_string(),
                "Suspend process (Unix only)".to_string(),
            ),
            ("`Ctrl+C` / `Esc`".to_string(), "Stop streaming".to_string()),
        ],
    );
}

fn write_context_specific(out: &mut String) {
    let child_binds: Vec<_> = KEYBINDS
        .iter()
        .filter(|kb| kb.context != ContextRef::Kind(ContextKind::General))
        .collect();

    if child_binds.is_empty() {
        return;
    }

    out.push_str("\n## Context-Specific\n\n");
    out.push_str("Some contexts add extra bindings on top of the defaults:\n\n");
    out.push_str("| Context | Key | Action |\n|---------|-----|--------|\n");

    for kb in &child_binds {
        let key = label_str(kb.label);
        out.push_str(&format!(
            "| {} | {key} | {} |\n",
            context_label(kb.context),
            kb.description
        ));
    }

    for (ctx, key, desc) in LUA_CONTEXT_BINDS {
        out.push_str(&format!("| {ctx} | {key} | {desc} |\n"));
    }
}

fn write_inheritance(out: &mut String) {
    out.push_str("\n## Context Inheritance\n\n");
    out.push_str("Identities inherit their kind's bindings and add their own.\n\n");

    for kind in ContextKind::ALL {
        let identities: Vec<_> = IDENTITIES
            .iter()
            .filter(|i| i.kind == kind)
            .map(|i| i.name)
            .collect();
        if identities.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "- **{}** is the base for: {}\n",
            section_title(kind),
            identities.join(", ")
        ));
    }
}

pub fn generate() -> String {
    let defaults = boot_default_keymap_entries();
    let mut out = String::from(FRONTMATTER);
    out.push_str("\n\n# Keybindings\n\n");
    out.push_str("On macOS, some bindings use Option or Fn keys instead (run `/help` for exact keybindings).\n");

    for &ctx in MAIN_CONTEXTS {
        write_section(&mut out, ctx, &defaults);
    }

    write_fixed(&mut out);
    write_context_specific(&mut out);
    write_inheritance(&mut out);
    write_overrides(&mut out);

    out
}

fn write_overrides(out: &mut String) {
    out.push_str("\n## Overriding Keybindings\n\n");
    out.push_str(
        "Plugins and `init.lua` can rebind keys at runtime with \
         `maki.keymap.set` and `maki.keymap.del`. The tables above are the \
         built-in defaults. An override on the same key wins, unless a \
         modal or overlay is open (help, plan form, permission prompt).\n\n",
    );
    out.push_str("Precedence, high to low:\n\n");
    out.push_str(
        "1. **Suspend** (`Ctrl+Z`, Unix). Always wins, non-remappable.\n\
         2. **Modal and overlay keys.** An open modal or picker consumes \
         its keys first, so they cannot be shadowed while open.\n\
         3. **Lua overrides** from `maki.keymap.set`. Last set wins; \
         binding the same key twice warns.\n\
         4. **Built-in defaults.** An override on the same key shadows \
         them; `maki.keymap.del` lifts the override so the default returns. \
         Suspend is the only binding outside this layer, so every key is \
         remappable except `Ctrl+Z`.\n\n",
    );
    out.push_str(
        "Only single-key bindings can be overridden. Multi-key combinations \
         and non-key rows (like `Type` to filter) cannot.\n\n",
    );
    out.push_str(
        "The `/help` modal and the splash show default labels, not live \
         overrides, but pressing the key still runs the override.\n\n",
    );
    out.push_str("### Recovering from a bad keymap\n\n");
    out.push_str(
        "If an override leaves Maki stuck (a rebound `Ctrl+C`, a modal \
         that won't close, a plugin that throws on load), boot without \
         user `init.lua`:\n\n",
    );
    out.push_str("```bash\nmaki --no-plugins\n```\n\n");
    out.push_str(
        "This skips user `init.lua` files (global and project) but keeps \
         the Lua host and every builtin plugin running, so suspend, \
         tools, and the default keymap still work.\n\n",
    );
    out.push_str(
        "Builtin plugins (tools, keymap, slash commands) load alongside \
         the rest of the defaults, unaffected by `--no-plugins`.\n\n",
    );
    out.push_str("## Shell and images\n\n");
    out.push_str(
        "These are input conventions, not remappable key rows:\n\n\
         - Prefix a line with `!` to run a shell command yourself (5 minute \
         timeout). Use `!!` to hide the command and its output from the agent.\n\
         - `Ctrl+V` pastes an image from the clipboard into the prompt when the \
         model supports vision. You can also paste image file paths.\n",
    );
}
