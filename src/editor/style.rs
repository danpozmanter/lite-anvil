use std::collections::HashMap;

/// All color keys used in the editor style system.
pub const STYLE_COLOR_KEYS: &[&str] = &[
    "background",
    "background2",
    "background3",
    "text",
    "caret",
    "accent",
    "dim",
    "divider",
    "selection",
    "line_number",
    "line_number2",
    "line_highlight",
    "scrollbar",
    "scrollbar2",
    "scrollbar_track",
    "nagbar",
    "nagbar_text",
    "nagbar_dim",
    "drag_overlay",
    "drag_overlay_tab",
    "good",
    "warn",
    "error",
    "modified",
    "guide",
];

/// Round a value by scale, using half-away-from-zero rounding.
pub fn round_scaled(val: f64, scale: f64) -> f64 {
    crate::editor::common::round(val * scale)
}

/// A parsed theme palette: color key -> color string (e.g. "#rrggbb").
/// Nested sub-palettes (like "syntax") are stored as sub-maps.
#[derive(Debug, Clone, Default)]
pub struct ThemePalette {
    pub colors: HashMap<String, String>,
    pub sub_palettes: HashMap<String, HashMap<String, String>>,
}

/// Load a JSON theme file and extract the palette.
pub fn load_theme_palette(path: &str) -> Result<ThemePalette, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("cannot read theme {path}: {e}"))?;
    let json: serde_json::Value =
        serde_json::from_str(&content).map_err(|e| format!("invalid JSON in {path}: {e}"))?;
    let palette = json
        .get("palette")
        .ok_or_else(|| "theme missing 'palette'".to_string())?;
    parse_palette(palette)
}

fn parse_palette(val: &serde_json::Value) -> Result<ThemePalette, String> {
    let mut theme = ThemePalette::default();
    let Some(map) = val.as_object() else {
        return Ok(theme);
    };
    for (k, v) in map {
        match v {
            serde_json::Value::String(s) => {
                theme.colors.insert(k.clone(), s.clone());
            }
            serde_json::Value::Object(sub) => {
                let mut sub_map = HashMap::new();
                for (sk, sv) in sub {
                    if let serde_json::Value::String(s) = sv {
                        sub_map.insert(sk.clone(), s.clone());
                    }
                }
                theme.sub_palettes.insert(k.clone(), sub_map);
            }
            _ => {}
        }
    }
    Ok(theme)
}

/// List built-in theme names.
pub fn builtin_theme_names() -> &'static [&'static str] {
    &[
        "default",
        "dark_default",
        "light_default",
        "fall",
        "summer",
        "textadept",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn style_color_keys_not_empty() {
        assert!(STYLE_COLOR_KEYS.len() > 20);
        assert!(STYLE_COLOR_KEYS.contains(&"background"));
        assert!(STYLE_COLOR_KEYS.contains(&"text"));
    }

    #[test]
    fn round_scaled_basic() {
        assert_eq!(round_scaled(14.0, 2.0), 28.0);
        assert_eq!(round_scaled(7.5, 1.0), 8.0);
    }

    #[test]
    fn load_theme_palette_from_file() {
        for candidate in ["data", "../data"] {
            let path = format!("{candidate}/assets/themes/dark_default.json");
            if let Ok(palette) = load_theme_palette(&path) {
                assert!(!palette.colors.is_empty(), "palette should have colors");
                assert!(
                    palette.colors.contains_key("background")
                        || palette.sub_palettes.contains_key("syntax"),
                    "should have standard keys"
                );
                return;
            }
        }
        panic!("cannot locate data/assets/themes/dark_default.json");
    }

    #[test]
    fn builtin_theme_names_has_default() {
        assert!(builtin_theme_names().contains(&"default"));
        assert!(builtin_theme_names().contains(&"dark_default"));
    }
}
