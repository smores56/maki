+++
title = "Keybindings"
weight = 5
[extra]
group = "Reference"
+++

# Keybindings

On macOS, some bindings use Option or Fn keys instead (run `/help` for exact keybindings).

## General

| Key | Action |
|-----|--------|
| `Ctrl+C` | Quit / clear input |
| `Ctrl+H` | Show keybindings |
| `Ctrl+P` | Previous task chat |
| `Ctrl+N` | Next task chat |
| `Ctrl+U` | Scroll half page up |
| `Ctrl+D` | Scroll half page down |
| `Ctrl+G` | Scroll to top |
| `Ctrl+B` | Scroll to bottom |
| `Ctrl+X` | Open tasks |
| `Ctrl+F` | Search messages |
| `Ctrl+S` | Open file picker |
| `Ctrl+O` | Open plan in editor |
| `Alt+O` | Edit input in editor |
| `Ctrl+Q` | Pop queued message |
| `Ctrl+T` | Toggle todo panel (todo_write) |

## Chat

| Key | Action |
|-----|--------|
| `Enter` | Submit prompt |
| `Shift+Enter` / `Ctrl+Enter` / `Ctrl+J` / `Alt+Enter` | Newline |
| `Tab` | Toggle mode |
| `/command` | Open command palette |
| `Ctrl+W` | Delete word backward |
| `Alt+←` / `Alt+→` | Move word left / right |
| `Ctrl+A` | Jump to start of line |
| `Home` / `End` | Jump to start/end of line |
| `Ctrl+E` | Jump to end of line |
| `Esc Esc` | Rewind |

### macOS-specific

| Key | Action |
|-----|--------|
| `Ctrl+Del` / `⌥Del` | Delete word forward |
| `Ctrl+K` | Delete to end of line |

## Streaming

| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate input history |
| `Esc Esc` | Cancel agent |

## Form

| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate options |
| `Enter` | Select option |
| `Esc` | Close |

## Picker

| Key | Action |
|-----|--------|
| `↑` / `↓` | Navigate |
| `Enter` | Select |
| `Esc` | Close |
| `Type` | Filter |
| `PageUp` / `PageDown` | Scroll page up / down |
| `Ctrl+U` / `Ctrl+D` | Scroll page up / down |

### model_picker

| Key | Action |
|-----|--------|
| `!/@/#/$` | Set tier (strong/medium/weak/compaction) |

### queue

| Key | Action |
|-----|--------|
| `Enter` | Remove item |

### commands

| Key | Action |
|-----|--------|
| `Tab` | Complete command |

## Fixed

Escape hatches hardcoded at the top of `App::handle_key`, above the keymap: not remappable, not routable.

| Key | Action |
|-----|--------|
| `Ctrl+Z` | Suspend process (Unix only) |
| `Ctrl+C` / `Esc` | Stop streaming |

## Context-Specific

Some contexts add extra bindings on top of the defaults:

| Context | Key | Action |
|---------|-----|--------|
| Chat | `Enter` | Submit prompt |
| Chat | `Shift+Enter` / `Ctrl+Enter` / `Ctrl+J` / `Alt+Enter` | Newline |
| Chat | `Tab` | Toggle mode |
| Chat | `/command` | Open command palette |
| Chat | `Ctrl+W` | Delete word backward |
| Chat | `Alt+←` / `Alt+→` | Move word left / right |
| Chat | `Ctrl+Del` / `⌥Del` | Delete word forward |
| Chat | `Ctrl+K` | Delete to end of line |
| Chat | `Ctrl+A` | Jump to start of line |
| Chat | `Home` / `End` | Jump to start/end of line |
| Chat | `Ctrl+E` | Jump to end of line |
| Chat | `Esc Esc` | Rewind |
| Streaming | `↑` / `↓` | Navigate input history |
| Streaming | `Esc Esc` | Cancel agent |
| Form | `↑` / `↓` | Navigate options |
| Form | `Enter` | Select option |
| Form | `Esc` | Close |
| Picker | `↑` / `↓` | Navigate |
| Picker | `Enter` | Select |
| Picker | `Esc` | Close |
| Picker | `Type` | Filter |
| Picker | `PageUp` / `PageDown` | Scroll page up / down |
| Picker | `Ctrl+U` / `Ctrl+D` | Scroll page up / down |
| queue | `Enter` | Remove item |
| commands | `Tab` | Complete command |
| model_picker | `!/@/#/$` | Set tier (strong/medium/weak/compaction) |
| Session Picker | `Ctrl+N` | New session |
| Session Picker | `Ctrl+R` | Rename session |
| Session Picker | `Ctrl+D` | Delete session (press twice) |

## Context Inheritance

Identities inherit their kind's bindings and add their own.

- **Picker** is the base for: task_picker, model_picker, theme_picker, rewind_picker, mcp_picker, login_picker, file_picker, search, queue, commands
- **Form** is the base for: plan_form, permission
- **Modal** is the base for: help, usage, btw, float

## Overriding Keybindings

Plugins and `init.lua` can rebind keys at runtime with `maki.keymap.set` and `maki.keymap.del`. The tables above are the built-in defaults. An override on the same key wins, unless a modal or overlay is open (help, plan form, permission prompt).

Precedence, high to low:

1. **Suspend** (`Ctrl+Z`, Unix). Always wins, non-remappable.
2. **Modal and overlay keys.** An open modal or picker consumes its keys first, so they cannot be shadowed while open.
3. **Lua overrides** from `maki.keymap.set`. Last set wins; binding the same key twice warns.
4. **Built-in defaults.** An override on the same key shadows them; `maki.keymap.del` lifts the override so the default returns. Suspend is the only binding outside this layer, so every key is remappable except `Ctrl+Z`.

Only single-key bindings can be overridden. Multi-key combinations and non-key rows (like `Type` to filter) cannot.

The `/help` modal and the splash show default labels, not live overrides, but pressing the key still runs the override.

### Recovering from a bad keymap

If an override leaves Maki stuck (a rebound `Ctrl+C`, a modal that won't close, a plugin that throws on load), boot without user `init.lua`:

```bash
maki --no-plugins
```

This skips user `init.lua` files (global and project) but keeps the Lua host and every builtin plugin running, so suspend, tools, and the default keymap still work.

Builtin plugins (tools, keymap, slash commands) load alongside the rest of the defaults, unaffected by `--no-plugins`.

## Shell and images

These are input conventions, not remappable key rows:

- Prefix a line with `!` to run a shell command yourself (5 minute timeout). Use `!!` to hide the command and its output from the agent.
- `Ctrl+V` pastes an image from the clipboard into the prompt when the model supports vision. You can also paste image file paths.
