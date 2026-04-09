use std::ffi::CString;

use crate::editor::terminal::{spawn_terminal, ensure_terminal_env, TerminalInner, TerminalSpawnOptions};
use crate::editor::terminal_buffer::{TerminalBufferInner, DEFAULT_SCROLLBACK};

pub(crate) const MAX_TERMINALS: usize = 10;

/// Single terminal instance within the panel.
pub(crate) struct TerminalInstance {
    pub(crate) inner: TerminalInner,
    pub(crate) tbuf: TerminalBufferInner,
    pub(crate) title: String,
}

/// Standard 16-color ANSI palette.
pub(crate) fn default_16_color_palette() -> [[u8; 4]; 16] {
    [
        [0, 0, 0, 255],       // black
        [170, 0, 0, 255],     // red
        [0, 170, 0, 255],     // green
        [170, 85, 0, 255],    // yellow/brown
        [0, 0, 170, 255],     // blue
        [170, 0, 170, 255],   // magenta
        [0, 170, 170, 255],   // cyan
        [170, 170, 170, 255], // white
        [85, 85, 85, 255],    // bright black
        [255, 85, 85, 255],   // bright red
        [85, 255, 85, 255],   // bright green
        [255, 255, 85, 255],  // bright yellow
        [85, 85, 255, 255],   // bright blue
        [255, 85, 255, 255],  // bright magenta
        [85, 255, 255, 255],  // bright cyan
        [255, 255, 255, 255], // bright white
    ]
}

/// Multi-terminal panel managing several terminal instances.
pub(crate) struct TerminalPanel {
    pub(crate) terminals: Vec<TerminalInstance>,
    pub(crate) active: usize,
    pub(crate) visible: bool,
    pub(crate) focused: bool,
}

impl TerminalPanel {
    pub(crate) fn new() -> Self {
        Self {
            terminals: Vec::new(),
            active: 0,
            visible: false,
            focused: false,
        }
    }

    /// Spawn a new terminal instance. Returns false if at the limit.
    pub(crate) fn spawn(&mut self, project_root: &str) -> bool {
        if self.terminals.len() >= MAX_TERMINALS {
            return false;
        }
        let shell =
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let cmd = vec![CString::new(shell).unwrap_or_default()];
        let mut env = Vec::new();
        let _ = ensure_terminal_env(&mut env);
        let opts = TerminalSpawnOptions {
            cwd: Some(CString::new(project_root).unwrap_or_default()),
            env,
            cols: 80,
            rows: 24,
        };
        match spawn_terminal(&cmd, &opts) {
            Ok(inner) => {
                let idx = self.terminals.len();
                let title = format!("Terminal {}", idx + 1);
                let palette = default_16_color_palette();
                let default_fg = [200, 200, 200, 255];
                let tbuf = TerminalBufferInner::new(
                    80, 24, DEFAULT_SCROLLBACK, palette, default_fg,
                );
                let inst = TerminalInstance { inner, tbuf, title };
                self.terminals.push(inst);
                self.active = idx;
                self.visible = true;
                self.focused = true;
                true
            }
            Err(e) => {
                eprintln!("[terminal] Spawn failed: {e}");
                false
            }
        }
    }

    /// Close the active terminal. Returns true if panel should stay visible.
    pub(crate) fn close_active(&mut self) -> bool {
        if self.terminals.is_empty() {
            return false;
        }
        self.terminals[self.active].inner.cleanup();
        self.terminals.remove(self.active);
        if self.terminals.is_empty() {
            self.active = 0;
            self.visible = false;
            self.focused = false;
            return false;
        }
        if self.active >= self.terminals.len() {
            self.active = self.terminals.len() - 1;
        }
        true
    }

    /// Get the active terminal instance, if any.
    pub(crate) fn active_terminal(&mut self) -> Option<&mut TerminalInstance> {
        self.terminals.get_mut(self.active)
    }

    /// Switch to next terminal tab.
    pub(crate) fn next_tab(&mut self) {
        if !self.terminals.is_empty() {
            self.active = (self.active + 1) % self.terminals.len();
        }
    }

    /// Switch to previous terminal tab.
    pub(crate) fn prev_tab(&mut self) {
        if !self.terminals.is_empty() {
            self.active = if self.active == 0 {
                self.terminals.len() - 1
            } else {
                self.active - 1
            };
        }
    }
}
