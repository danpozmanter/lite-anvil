use std::collections::HashMap;

use crate::editor::event::Modifiers;

/// Modifier key ordering for normalization.
const MOD_ORDER: &[&str] = &["ctrl", "alt", "shift"];

/// Native keymap: maps normalized keystrokes to lists of command names.
pub struct NativeKeymap {
    map: HashMap<String, Vec<String>>,
    reverse: HashMap<String, Vec<String>>,
    /// Currently held modifier keys.
    pub modkeys: Modifiers,
}

impl NativeKeymap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            reverse: HashMap::new(),
            modkeys: Modifiers::default(),
        }
    }

    /// Build keymap with default bindings.
    pub fn with_defaults() -> Self {
        let mut km = Self::new();
        for (stroke, cmds) in DEFAULT_BINDINGS {
            km.add(stroke, cmds);
        }
        km
    }

    /// Add a binding. Commands are tried in order; first valid one wins.
    pub fn add(&mut self, stroke: &str, commands: &[&str]) {
        let norm = normalize_stroke(stroke);
        let cmds: Vec<String> = commands.iter().map(|s| s.to_string()).collect();
        for cmd in &cmds {
            self.reverse
                .entry(cmd.clone())
                .or_default()
                .push(norm.clone());
        }
        self.map.insert(norm, cmds);
    }

    /// Add bindings from TOML config keybindings map.
    pub fn add_from_config(&mut self, bindings: &HashMap<String, toml::Value>) {
        for (stroke, val) in bindings {
            match val {
                toml::Value::String(cmd) => {
                    self.add(stroke, &[cmd.as_str()]);
                }
                toml::Value::Array(arr) => {
                    let cmds: Vec<&str> = arr
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect();
                    if !cmds.is_empty() {
                        self.add(stroke, &cmds);
                    }
                }
                _ => {}
            }
        }
    }

    /// Resolve a key press to a list of command names.
    /// Returns None if no binding matches.
    pub fn on_key_pressed(&mut self, key: &str, mods: Modifiers) -> Option<&[String]> {
        self.modkeys = mods;

        // Build stroke from key + modifiers.
        let stroke = build_stroke(key, &mods);
        let norm = normalize_stroke(&stroke);
        self.map.get(&norm).map(|v| v.as_slice())
    }

    /// Get the first keybinding for a command, formatted for display.
    pub fn get_binding_display(&self, cmd: &str) -> Option<String> {
        self.reverse
            .get(cmd)
            .and_then(|strokes| strokes.first())
            .map(|s| format_stroke(s))
    }

    /// Iterate over all bindings (stroke -> command names).
    pub fn iter_bindings(&self) -> impl Iterator<Item = (&str, &[String])> {
        self.map.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// Get all bindings for a command.
    pub fn get_bindings(&self, cmd: &str) -> Option<&[String]> {
        self.reverse.get(cmd).map(|v| v.as_slice())
    }
}

impl Default for NativeKeymap {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a stroke string from a key name and modifier state.
fn build_stroke(key: &str, mods: &Modifiers) -> String {
    let mut parts = Vec::new();
    if mods.ctrl {
        parts.push("ctrl");
    }
    if mods.alt {
        parts.push("alt");
    }
    if mods.shift {
        parts.push("shift");
    }
    parts.push(key);
    parts.join("+")
}

/// Normalize a keystroke by sorting modifiers in standard order.
pub fn normalize_stroke(stroke: &str) -> String {
    let parts: Vec<&str> = stroke.split('+').collect();
    let mut mods = Vec::new();
    let mut keys = Vec::new();
    for part in &parts {
        if MOD_ORDER.contains(part) {
            mods.push(*part);
        } else {
            keys.push(*part);
        }
    }
    mods.sort_by_key(|m| MOD_ORDER.iter().position(|o| o == m).unwrap_or(99));
    keys.sort();
    mods.extend(keys);
    mods.join("+")
}

/// Format a stroke for display (e.g. "ctrl+shift+a" -> "Ctrl+Shift+A").
fn format_stroke(stroke: &str) -> String {
    stroke
        .split('+')
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                None => String::new(),
                Some(first) => {
                    let mut s = first.to_uppercase().to_string();
                    s.push_str(c.as_str());
                    s
                }
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Default keybindings (matches the Lua build_default_bindings).
const DEFAULT_BINDINGS: &[(&str, &[&str])] = &[
    ("ctrl+p", &["core:find-command"]),
    ("ctrl+q", &["core:quit"]),
    ("ctrl+o", &["core:open-file"]),
    ("ctrl+shift+r", &["core:open-recent-file"]),
    ("ctrl+alt+shift+r", &["core:open-recent-folder"]),
    ("ctrl+n", &["core:new-doc"]),
    ("ctrl+shift+n", &["core:new-window"]),
    ("ctrl+alt+o", &["core:open-file-dialog"]),
    ("ctrl+shift+o", &["core:open-project-folder"]),
    ("ctrl+shift+alt+o", &["core:open-folder-dialog"]),
    ("ctrl+alt+w", &["core:close-project-folder"]),
    ("ctrl+alt+r", &["core:restart"]),
    ("alt+return", &["core:toggle-fullscreen"]),
    ("f11", &["core:toggle-fullscreen"]),
    ("alt+shift+j", &["root:split-left"]),
    ("alt+shift+l", &["root:split-right"]),
    ("alt+shift+i", &["root:split-up"]),
    ("alt+shift+k", &["root:split-down"]),
    ("alt+j", &["root:switch-to-left"]),
    ("alt+l", &["root:switch-to-right"]),
    ("alt+i", &["root:switch-to-up"]),
    ("alt+k", &["root:switch-to-down"]),
    ("ctrl+w", &["root:close"]),
    ("ctrl+tab", &["root:switch-to-next-tab"]),
    ("ctrl+shift+tab", &["root:switch-to-previous-tab"]),
    ("ctrl+shift+f", &["root:toggle-focus-mode"]),
    ("ctrl+b", &["root:toggle-sidebar"]),
    ("ctrl+\\", &["root:toggle-sidebar"]),
    ("ctrl+pageup", &["root:move-tab-left"]),
    ("ctrl+pagedown", &["root:move-tab-right"]),
    ("ctrl+f", &["find-replace:find"]),
    ("ctrl+h", &["find-replace:replace"]),
    ("ctrl+r", &["find-replace:replace"]),
    ("f3", &["find-replace:repeat-find"]),
    ("shift+f3", &["find-replace:previous-find"]),
    ("ctrl+g", &["doc:go-to-line"]),
    ("ctrl+s", &["doc:save"]),
    ("ctrl+shift+s", &["doc:save-as"]),
    ("ctrl+z", &["doc:undo"]),
    ("ctrl+shift+z", &["doc:redo"]),
    ("ctrl+y", &["doc:redo"]),
    ("ctrl+x", &["doc:cut"]),
    ("ctrl+c", &["doc:copy"]),
    ("ctrl+v", &["doc:paste"]),
    (
        "escape",
        &[
            "root:exit-focus-mode",
            "command:escape",
            "doc:select-none",
            "context-menu:hide",
            "dialog:select-no",
        ],
    ),
    ("tab", &["command:complete", "doc:indent"]),
    ("shift+tab", &["doc:unindent"]),
    ("backspace", &["doc:backspace"]),
    ("shift+backspace", &["doc:backspace"]),
    ("ctrl+backspace", &["doc:delete-to-previous-word-start"]),
    ("delete", &["doc:delete"]),
    ("ctrl+delete", &["doc:delete-to-next-word-end"]),
    (
        "return",
        &[
            "command:submit",
            "context-menu:submit",
            "doc:newline",
            "dialog:select",
        ],
    ),
    ("ctrl+return", &["doc:newline-below"]),
    ("ctrl+shift+return", &["doc:newline-above"]),
    ("ctrl+j", &["doc:join-lines"]),
    ("ctrl+a", &["doc:select-all"]),
    ("ctrl+d", &["find-replace:select-add-next", "doc:select-word"]),
    ("ctrl+l", &["doc:select-lines"]),
    ("ctrl+/", &["doc:toggle-line-comments"]),
    ("ctrl+up", &["doc:move-lines-up"]),
    ("ctrl+down", &["doc:move-lines-down"]),
    ("ctrl+shift+d", &["doc:duplicate-lines"]),
    ("ctrl+shift+k", &["doc:delete-lines"]),
    ("left", &["doc:move-to-previous-char", "dialog:previous-entry"]),
    ("right", &["doc:move-to-next-char", "dialog:next-entry"]),
    (
        "up",
        &[
            "command:select-previous",
            "context-menu:focus-previous",
            "doc:move-to-previous-line",
        ],
    ),
    (
        "down",
        &[
            "command:select-next",
            "context-menu:focus-next",
            "doc:move-to-next-line",
        ],
    ),
    ("ctrl+left", &["doc:move-to-previous-word-start"]),
    ("ctrl+right", &["doc:move-to-next-word-end"]),
    ("ctrl+[", &["doc:move-to-previous-block-start"]),
    ("ctrl+]", &["doc:move-to-next-block-end"]),
    ("home", &["doc:move-to-start-of-indentation"]),
    ("end", &["doc:move-to-end-of-line"]),
    ("ctrl+home", &["doc:move-to-start-of-doc"]),
    ("ctrl+end", &["doc:move-to-end-of-doc"]),
    ("pageup", &["command:select-previous-page", "doc:move-to-previous-page"]),
    ("pagedown", &["command:select-next-page", "doc:move-to-next-page"]),
    ("shift+left", &["doc:select-to-previous-char"]),
    ("shift+right", &["doc:select-to-next-char"]),
    ("shift+up", &["doc:select-to-previous-line"]),
    ("shift+down", &["doc:select-to-next-line"]),
    ("ctrl+shift+left", &["doc:select-to-previous-word-start"]),
    ("ctrl+shift+right", &["doc:select-to-next-word-end"]),
    ("shift+home", &["doc:select-to-start-of-indentation"]),
    ("shift+end", &["doc:select-to-end-of-line"]),
    ("ctrl+shift+home", &["doc:select-to-start-of-doc"]),
    ("ctrl+shift+end", &["doc:select-to-end-of-doc"]),
    ("shift+pageup", &["doc:select-to-previous-page"]),
    ("shift+pagedown", &["doc:select-to-next-page"]),
    ("ctrl+shift+up", &["doc:create-cursor-previous-line"]),
    ("ctrl+shift+down", &["doc:create-cursor-next-line"]),
    ("ctrl+=", &["scale:increase"]),
    ("ctrl+equals", &["scale:increase"]),
    ("ctrl++", &["scale:increase"]),
    ("ctrl+-", &["scale:decrease"]),
    ("ctrl+minus", &["scale:decrease"]),
    ("ctrl+0", &["scale:reset"]),
    ("ctrl+`", &["core:toggle-terminal"]),
    ("f5", &["core:toggle-terminal"]),
    ("ctrl+shift+t", &["core:new-terminal"]),
    ("ctrl+shift+w", &["core:close-terminal"]),
    ("ctrl+k", &["lsp:hover"]),
    ("f12", &["lsp:go-to-definition"]),
    ("ctrl+f12", &["lsp:go-to-implementation"]),
    ("shift+f12", &["lsp:find-references"]),
    ("ctrl+shift+f12", &["lsp:go-to-type-definition"]),
    ("ctrl+m", &["core:toggle-minimap"]),
    ("alt+z", &["core:toggle-line-wrapping"]),
    ("ctrl+shift+h", &["core:toggle-whitespace"]),
    ("ctrl+shift+f", &["core:project-search"]),
    ("ctrl+shift+[", &["doc:fold"]),
    ("ctrl+shift+]", &["doc:unfold"]),
    ("ctrl+shift+\\", &["doc:unfold-all"]),
    ("ctrl+shift+p", &["core:cycle-theme"]),
    ("ctrl+shift+g", &["core:git-status"]),
];

// Compatibility wrappers for editor/ module callers.

/// Split a keystroke string into parts.
pub fn split_stroke(stroke: &str) -> Vec<String> {
    stroke.split('+').map(|s| s.to_string()).collect()
}

/// Capitalize the first character of a string.
pub fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let mut result = String::with_capacity(s.len());
            for upper in c.to_uppercase() {
                result.push(upper);
            }
            result.push_str(chars.as_str());
            result
        }
    }
}

/// Format a command name for display.
pub fn prettify_name(name: &str) -> String {
    // Strip namespace prefix (core:, doc:, root:, etc).
    let stripped = name.split_once(':').map(|(_, rest)| rest).unwrap_or(name);
    stripped
        .replace('-', " ")
        .split_whitespace()
        .map(capitalize_first)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_stroke_sorts_modifiers() {
        assert_eq!(normalize_stroke("a+shift+ctrl"), "ctrl+shift+a");
    }

    #[test]
    fn normalize_stroke_single_key() {
        assert_eq!(normalize_stroke("escape"), "escape");
    }

    #[test]
    fn build_stroke_with_mods() {
        let mods = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
            gui: false,
        };
        assert_eq!(build_stroke("a", &mods), "ctrl+shift+a");
    }

    #[test]
    fn keymap_with_defaults_has_bindings() {
        let km = NativeKeymap::with_defaults();
        assert!(km.map.contains_key("ctrl+q"));
        assert!(km.map.contains_key("ctrl+s"));
    }

    #[test]
    fn on_key_pressed_resolves() {
        let mut km = NativeKeymap::with_defaults();
        let mods = Modifiers {
            ctrl: true,
            ..Default::default()
        };
        let cmds = km.on_key_pressed("q", mods);
        assert!(cmds.is_some());
        assert_eq!(cmds.unwrap()[0], "core:quit");
    }

    #[test]
    fn get_binding_display_works() {
        let km = NativeKeymap::with_defaults();
        let display = km.get_binding_display("core:quit");
        assert_eq!(display, Some("Ctrl+Q".to_string()));
    }

    #[test]
    fn format_stroke_capitalizes() {
        assert_eq!(format_stroke("ctrl+shift+a"), "Ctrl+Shift+A");
    }
}
