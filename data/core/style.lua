local common = require "core.common"
local config = require "core.config"

local style = {}

style.themes = {}
style.syntax = {}
style.syntax_fonts = {}
style.log = {}
style.lint = style.lint or {}
style._lazy_font_specs = {}

local default_ui = {
  divider_size = 1,
  scrollbar_size = 4,
  expanded_scrollbar_size = 12,
  minimum_thumb_size = 20,
  contracted_scrollbar_margin = 8,
  expanded_scrollbar_margin = 12,
  caret_width = 2,
  tab_width = 170,
  padding_x = 14,
  padding_y = 7,
}

local default_fonts = {
  ui = {
    path = DATADIR .. "/fonts/Lilex-Regular.ttf",
    size = 15,
    options = {},
  },
  code = {
    path = DATADIR .. "/fonts/Lilex-Medium.ttf",
    size = 15,
    options = {},
  },
  big = {
    size = 46,
    options = {},
  },
  icon = {
    path = DATADIR .. "/fonts/icons.ttf",
    size = 16,
    options = {
      antialiasing = "grayscale",
      hinting = "full",
    },
  },
  icon_big = {
    size = 23,
    options = {},
  },
  syntax = {},
}


local function copy_table(t)
  if type(t) ~= "table" then
    return t
  end
  local out = {}
  for k, v in pairs(t) do
    out[k] = copy_table(v)
  end
  return out
end


local function merge_tables(base, override)
  local out = copy_table(base) or {}
  if type(override) ~= "table" then
    return out
  end
  for k, v in pairs(override) do
    if type(v) == "table" and type(out[k]) == "table" then
      out[k] = merge_tables(out[k], v)
    else
      out[k] = copy_table(v)
    end
  end
  return out
end


local function color_value(value)
  if type(value) == "string" then
    return { common.color(value) }
  end
  if type(value) == "table" then
    if type(value[1]) == "number" then
      return { value[1], value[2], value[3], value[4] or 0xff }
    end
    if value.r then
      return { value.r, value.g, value.b, value.a or 0xff }
    end
  end
  return nil
end


local function apply_color(target, key, value)
  local normalized = color_value(value)
  if normalized then
    target[key] = normalized
  end
end


local function load_single_font(path, size, options)
  local ok, font = pcall(renderer.font.load, path, size, options)
  return ok and font or nil
end


local function load_font_from_spec(spec, fallback)
  local resolved = merge_tables(fallback or {}, spec or {})
  local size = (resolved.size or 14) * SCALE
  local options = resolved.options or {}
  local paths = resolved.paths
  if type(paths) ~= "table" then
    paths = { resolved.path }
  end

  local fonts = {}
  for _, path in ipairs(paths) do
    if type(path) == "string" and path ~= "" then
      local font = load_single_font(path, size, options)
      if font then
        table.insert(fonts, font)
      end
    end
  end

  if #fonts == 0 and fallback then
    local fallback_path = fallback.paths or { fallback.path }
    for _, path in ipairs(fallback_path) do
      if type(path) == "string" and path ~= "" then
        local font = load_single_font(path, size, options)
        if font then
          table.insert(fonts, font)
        end
      end
    end
  end

  assert(#fonts > 0, "unable to load configured font")
  return #fonts == 1 and fonts[1] or renderer.font.group(fonts)
end


local function get_lazy_font(name)
  local spec = style._lazy_font_specs[name]
  if not spec then
    return style[name]
  end

  local font = load_font_from_spec(spec.spec, spec.fallback)
  style[name] = font
  style._lazy_font_specs[name] = nil
  return font
end


function style.get_big_font()
  return get_lazy_font("big_font")
end


function style.get_icon_big_font()
  return get_lazy_font("icon_big_font")
end


function style.register_theme(name, palette)
  style.themes[name] = palette
end


function style.apply_config()
  local ui = merge_tables(default_ui, config.ui)
  style.divider_size = common.round(ui.divider_size * SCALE)
  style.scrollbar_size = common.round(ui.scrollbar_size * SCALE)
  style.expanded_scrollbar_size = common.round(ui.expanded_scrollbar_size * SCALE)
  style.minimum_thumb_size = common.round(ui.minimum_thumb_size * SCALE)
  style.contracted_scrollbar_margin = common.round(ui.contracted_scrollbar_margin * SCALE)
  style.expanded_scrollbar_margin = common.round(ui.expanded_scrollbar_margin * SCALE)
  style.caret_width = common.round(ui.caret_width * SCALE)
  style.tab_width = common.round(ui.tab_width * SCALE)

  style.padding = {
    x = common.round(ui.padding_x * SCALE),
    y = common.round(ui.padding_y * SCALE),
  }

  style.margin = {
    tab = {
      top = common.round(
        ((type(ui.tab_top_margin) == "number" and ui.tab_top_margin) or (-style.divider_size)) * SCALE
      )
    }
  }

  local fonts = merge_tables(default_fonts, config.fonts)
  style.font = load_font_from_spec(fonts.ui, default_fonts.ui)
  style.code_font = load_font_from_spec(fonts.code, default_fonts.code)
  style.icon_font = load_font_from_spec(fonts.icon, default_fonts.icon)
  style.big_font = nil
  style.icon_big_font = nil
  style._lazy_font_specs.big_font = {
    spec = fonts.big,
    fallback = fonts.ui,
  }
  style._lazy_font_specs.icon_big_font = {
    spec = fonts.icon_big,
    fallback = fonts.icon,
  }

  for token, font_spec in pairs(fonts.syntax or {}) do
    style.syntax_fonts[token] = load_font_from_spec(font_spec, fonts.code)
  end

  local theme_name = config.theme or "default"
  if not style.themes[theme_name] and theme_name ~= "default" then
    pcall(require, "colors." .. theme_name)
  end
  theme_name = style.themes[theme_name] and theme_name or "default"
  local palette = merge_tables(style.themes.default or {}, style.themes[theme_name] or {})
  local colors = merge_tables(palette, config.colors)

  local style_keys = {
    "background", "background2", "background3", "text", "caret", "accent", "dim",
    "divider", "selection", "line_number", "line_number2", "line_highlight",
    "scrollbar", "scrollbar2", "scrollbar_track", "nagbar", "nagbar_text",
    "nagbar_dim", "drag_overlay", "drag_overlay_tab", "good", "warn", "error",
    "modified", "guide"
  }
  for _, key in ipairs(style_keys) do
    apply_color(style, key, colors[key])
  end

  for key, value in pairs(colors.syntax or {}) do
    apply_color(style.syntax, key, value)
  end

  for key, value in pairs(colors.lint or {}) do
    apply_color(style.lint, key, value)
  end

  local log_defaults = {
    INFO = { icon = "i", color = style.text },
    WARN = { icon = "!", color = style.warn },
    ERROR = { icon = "!", color = style.error },
  }
  local log_config = colors.log or {}
  for level, default_entry in pairs(log_defaults) do
    local entry = merge_tables(default_entry, log_config[level])
    style.log[level] = {
      icon = entry.icon,
      color = color_value(entry.color) or default_entry.color,
    }
  end

  for level, entry in pairs(log_config) do
    if not log_defaults[level] then
      style.log[level] = {
        icon = entry.icon or "?",
        color = color_value(entry.color) or style.text,
      }
    end
  end

  return style
end


return style
