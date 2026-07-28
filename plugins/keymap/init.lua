-- Default keymap. Every non-editing builtin keybind lives here so users
-- can read, override (`maki.keymap.set`), or unmap (`maki.keymap.del`)
-- each one. Editing bindings (Enter, Tab, Ctrl+W/A/E/K, image paste) stay
-- hardcoded in Rust; see `:help keymap` for the full picture.
--
-- `<C-z>` (suspend) and streaming `<C-c>` / `<Esc>` are also hardcoded
-- exempt keys at the top of `App::handle_key` and are not routable here.

maki.keymap.set("n", "<C-c>", maki.actions.quit)
maki.keymap.set("n", "<C-h>", maki.actions.help)
maki.keymap.set("n", "<C-p>", maki.actions.prev_chat)
maki.keymap.set("n", "<C-n>", maki.actions.next_chat)
maki.keymap.set("n", "<C-u>", maki.actions.scroll_half_up)
maki.keymap.set("n", "<C-d>", maki.actions.scroll_half_down)
maki.keymap.set("n", "<C-g>", maki.actions.scroll_top)
maki.keymap.set("n", "<C-b>", maki.actions.scroll_bottom)
maki.keymap.set("n", "<C-t>", maki.actions.plan_toggle)
maki.keymap.set("n", "<C-x>", maki.actions.tasks)
maki.keymap.set("n", "<C-f>", maki.actions.search)
maki.keymap.set("n", "<C-s>", maki.actions.file_picker)
maki.keymap.set("n", "<C-o>", maki.actions.open_editor)
maki.keymap.set("n", "<M-o>", maki.actions.edit_input)
maki.keymap.set("n", "<C-q>", maki.actions.pop_queue)
