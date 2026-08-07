-- Default keymap. Every non-editing builtin keybind lives here so users
-- can read, override (`maki.keymap.set`), or unmap (`maki.keymap.del`)
-- each one. Editing bindings (Enter, Tab, Ctrl+W/A/E/K, image paste) stay
-- hardcoded in Rust; see `:help keymap` for the full picture.
--
-- `<C-z>` (suspend) and streaming `<C-c>` / `<Esc>` are also hardcoded
-- exempt keys at the top of `App::handle_key` and are not routable here.
--
-- All bindings are General context (no `context` option, no mode arg): they
-- fire in every context unless a more specific binding shadows them.

maki.keymap.set("<C-c>", maki.actions.quit)
maki.keymap.set("<C-h>", maki.actions.help)
maki.keymap.set("<C-p>", maki.actions.prev_chat)
maki.keymap.set("<C-n>", maki.actions.next_chat)
maki.keymap.set("<C-u>", maki.actions.scroll_half_up)
maki.keymap.set("<C-d>", maki.actions.scroll_half_down)
maki.keymap.set("<C-g>", maki.actions.scroll_top)
maki.keymap.set("<C-b>", maki.actions.scroll_bottom)
maki.keymap.set("<C-t>", maki.actions.plan_toggle)
maki.keymap.set("<C-x>", maki.actions.tasks)
maki.keymap.set("<C-f>", maki.actions.search)
maki.keymap.set("<C-s>", maki.actions.file_picker)
maki.keymap.set("<C-o>", maki.actions.open_editor)
maki.keymap.set("<M-o>", maki.actions.edit_input)
maki.keymap.set("<C-q>", maki.actions.pop_queue)
