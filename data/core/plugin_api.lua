local core = require "core"

local plugin_api = {}

plugin_api.session = {
  on_load = function(name, hook)
    return core.register_session_load_hook(name, hook)
  end,
  on_save = function(name, hook)
    return core.register_session_save_hook(name, hook)
  end,
}

plugin_api.threads = {
  spawn = function(weak_ref, fn, ...)
    return core.add_thread(fn, weak_ref, ...)
  end,
}

plugin_api.views = {
  active = function()
    return core.active_view
  end,
  set_active = function(view)
    return core.set_active_view(view)
  end,
  open_doc = function(path_or_doc)
    return core.plugin_open_doc(path_or_doc)
  end,
  children = function()
    return core.plugin_children()
  end,
  get_node_for_view = function(view)
    return core.plugin_get_node_for_view(view)
  end,
  update_layout = function()
    return core.plugin_update_layout()
  end,
  root_size = function()
    return core.plugin_root_size()
  end,
  defer_draw = function(fn, ...)
    return core.root_view:defer_draw(fn, ...)
  end,
  get_active_node_default = function()
    return core.root_view:get_active_node_default()
  end,
  get_primary_node = function()
    return core.root_view:get_primary_node()
  end,
  add_view = function(view, placement)
    return core.root_view:add_view(view, placement)
  end,
  close_all_docviews = function(keep_active)
    return core.root_view:close_all_docviews(keep_active)
  end,
}

plugin_api.prompt = {
  enter = function(label, options)
    return core.plugin_enter_prompt(label, options)
  end,
  update_suggestions = function()
    return core.plugin_update_prompt_suggestions()
  end,
}

plugin_api.status = {
  constants = {
    RIGHT = function()
      return core.status_view.Item.RIGHT
    end,
    separator2 = function()
      return core.status_view.separator2
    end,
  },
  add_item = function(item)
    return core.plugin_add_status_item(item)
  end,
  show_message = function(icon, color, text)
    return core.plugin_show_status_message(icon, color, text)
  end,
  show_tooltip = function(text)
    return core.plugin_show_status_tooltip(text)
  end,
  remove_tooltip = function()
    return core.plugin_remove_status_tooltip()
  end,
}

return plugin_api
