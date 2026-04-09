/// Keyboard modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub gui: bool,
}

/// Every event the editor can receive from the windowing system.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorEvent {
    KeyPressed {
        key: String,
        modifiers: Modifiers,
    },
    KeyReleased {
        key: String,
        modifiers: Modifiers,
    },
    TextInput(String),
    MousePressed {
        button: MouseButton,
        x: f64,
        y: f64,
        clicks: u32,
    },
    MouseReleased {
        button: MouseButton,
        x: f64,
        y: f64,
    },
    MouseMoved {
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    MouseWheel {
        x: f64,
        y: f64,
    },
    TouchPressed {
        id: u64,
        x: f64,
        y: f64,
    },
    TouchMoved {
        id: u64,
        x: f64,
        y: f64,
        dx: f64,
        dy: f64,
    },
    TouchReleased {
        id: u64,
        x: f64,
        y: f64,
    },
    FileDropped(std::path::PathBuf),
    Resized {
        w: f64,
        h: f64,
    },
    Exposed,
    FocusGained,
    FocusLost,
    MouseLeft,
    Quit,
}

/// Mouse button identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    X1,
    X2,
}

/// Result of event handling in a view or plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventResult {
    /// Event was consumed; stop propagating.
    Consumed,
    /// Event was ignored; continue propagating.
    Ignored,
}
