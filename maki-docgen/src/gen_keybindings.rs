use maki_ui::keybindings::{ALT_SEP, KEYBINDS, KeyLabel, KeybindContext, Platform, all_contexts};

const FRONTMATTER: &str = "\
+++
title = \"Keybindings\"
weight = 7
[extra]
group = \"Reference\"
+++";

const MAIN_CONTEXTS: &[KeybindContext] = &[
    KeybindContext::General,
    KeybindContext::Editing,
    KeybindContext::Streaming,
    KeybindContext::FormInput,
    KeybindContext::Picker,
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

fn write_table_2col(out: &mut String, rows: &[(String, &str)]) {
    out.push_str("| Key | Action |\n|-----|--------|\n");
    for (key, desc) in rows {
        out.push_str(&format!("| {key} | {desc} |\n"));
    }
}

fn write_section(out: &mut String, ctx: KeybindContext) {
    out.push_str(&format!("\n## {}\n\n", ctx.label()));

    let all_rows: Vec<_> = KEYBINDS.iter().filter(|kb| kb.context == ctx).collect();

    let normal: Vec<_> = all_rows
        .iter()
        .filter(|kb| kb.platform == Platform::All)
        .map(|kb| (label_str(kb.label), kb.description))
        .collect();

    if !normal.is_empty() {
        write_table_2col(out, &normal);
    }

    let mac_only: Vec<_> = all_rows
        .iter()
        .filter(|kb| kb.platform == Platform::MacOnly)
        .map(|kb| (label_str(kb.label), kb.description))
        .collect();

    if !mac_only.is_empty() {
        out.push_str("\n### macOS-specific\n\n");
        write_table_2col(out, &mac_only);
    }
}

fn write_context_specific(out: &mut String) {
    let child_binds: Vec<_> = KEYBINDS
        .iter()
        .filter(|kb| kb.context.parent().is_some())
        .collect();

    if child_binds.is_empty() {
        return;
    }

    out.push_str("\n## Context-Specific\n\n");
    out.push_str("Some pickers add extra bindings on top of the defaults:\n\n");
    out.push_str("| Context | Key | Action |\n|---------|-----|--------|\n");

    for kb in &child_binds {
        let key = label_str(kb.label);
        out.push_str(&format!(
            "| {} | {key} | {} |\n",
            kb.context.label(),
            kb.description
        ));
    }
}

fn write_inheritance(out: &mut String) {
    let children: Vec<_> = all_contexts()
        .filter(|ctx| ctx.parent().is_some())
        .collect();

    if children.is_empty() {
        return;
    }

    out.push_str("\n## Context Inheritance\n\n");
    out.push_str("Child contexts inherit their parent's bindings and add their own.\n\n");

    let mut by_parent: Vec<(KeybindContext, Vec<&str>)> = Vec::new();
    for child in &children {
        let parent = child.parent().unwrap();
        if let Some(entry) = by_parent.iter_mut().find(|(p, _)| *p == parent) {
            entry.1.push(child.label());
        } else {
            by_parent.push((parent, vec![child.label()]));
        }
    }

    for (parent, kids) in &by_parent {
        let list = kids.join(", ");
        out.push_str(&format!(
            "- **{}** is the base for: {list}\n",
            parent.label()
        ));
    }
}

pub fn generate() -> String {
    let mut out = String::from(FRONTMATTER);
    out.push_str("\n\n# Keybindings\n\n");
    out.push_str("On macOS, some bindings use Option or Fn keys instead (run `/help` for exact keybindings).\n");

    for &ctx in MAIN_CONTEXTS {
        write_section(&mut out, ctx);
    }

    write_context_specific(&mut out);
    write_inheritance(&mut out);
    write_overrides(&mut out);

    out
}

fn write_overrides(out: &mut String) {
    out.push_str("\n## Overriding Keybindings\n\n");
    out.push_str(
        "Plugins and your init files can rebind keys at runtime through `maki.keymap.set` and \
         `maki.keymap.del` in Lua. The tables above show the built-in defaults; an override on the \
         same key takes precedence when no overlay or modal (help modal, plan form, permission \
         prompt) is open.\n\n",
    );
    out.push_str("Precedence, from highest to lowest:\n\n");
    out.push_str(
        "1. **Suspend** (`Ctrl+Z` on Unix): non-remappable, always wins.\n\
         2. **Overlay and modal focus keys**: help modal, plan form, permission prompt, and pickers \
         consume their keys before overrides run, so those modal keys cannot be shadowed while the \
         modal is open. This includes `Esc` cancel inside a non-main chat and `Tab` mode-toggle.\n\
         3. **Lua overrides**: bindings registered via `maki.keymap.set`. Last set wins; binding the \
         same key warns.\n\
         4. **Built-in defaults**: the keys listed in the tables above. `maki.keymap.del` restores \
         the default for a calling plugin's own binding by fallthrough on the next keypress.\n\n",
    );
    out.push_str(
        "Display caveat: the in-app `/help` modal and the startup splash currently show built-in \
         default labels, not live overrides. An override is effective for dispatch even when the \
         label shown still reflects the default.\n\n",
    );
    out.push_str(
        "Multi-key and non-key rows ( Alt+O / Alt+Enter newline, `Tab`, `/command`, word-left/right) \
         cannot be overridden because the override store keys on a single `KeyCode` + `KeyModifiers`.\n\n",
    );
    out.push_str(
        "See the Permissions page for the `keymap` plugin permission and `--no-plugins` to boot with \
         the full default keymap and no Lua at all.\n",
    );
}
