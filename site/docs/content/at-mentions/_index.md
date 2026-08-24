+++
title = "At Mentions"
weight = 24
[extra]
group = "Guides"
+++

# At Mentions

Type `@` in the prompt box and a fuzzy picker opens with the files and directories of your project. Keep typing to filter, use the arrow keys to move, and press Enter to pick.

## What gets inserted

The picker inserts `@path` and a trailing space. Paths are relative to your working directory, so picking `src/main.rs` gives you `@src/main.rs`. Directories get a trailing slash: `@src/`.

Only the path is inserted. The file contents are not attached. When you send the message, the agent sees the path and reads the file itself with the read tool.

## Anchors

By default the search starts in your working directory. A few prefixes change the starting point:

| Prefix | Search starts at |
| ------ | ---------------- |
| `@/`   | filesystem root |
| `@~`   | home folder |
| `@./`  | working directory |
| `@../` | parent directory, and further up |

So `@/usr/local`, `@~/projects/foo`, and `@../docs/plan.md` all do what they look like. The inserted path keeps the anchor: a pick from `@/` inserts `@/usr/local/bin`, a pick from `@~` inserts `@~/projects/foo/main.rs`.

## Fixing a path

Move the cursor into an existing `@path` and the picker opens again. Fix a typo or pick a different file. Tab completes the top match, Esc closes the picker.

## When nothing matches

The picker stays open and Enter is swallowed, so a stray `@` never sends a message by accident. Esc closes the picker.

## Bash mode

Mentions do not trigger in bash input mode, where lines start with `!`.

## Configuration

The at_mention plugin is built in and enabled by default. Turn it off in `init.lua`:

```lua
maki.setup({
    plugins = {
        at_mention = { enabled = false },
    },
})
```

## Honest limits

A search from the filesystem root (`@/`) walks with a depth cap and an entry cap, so on a big system it is best-effort. The walk stops at depth 10 and after 50,000 entries.
