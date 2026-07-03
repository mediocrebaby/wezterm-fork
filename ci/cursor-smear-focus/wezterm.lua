local wezterm = require 'wezterm'

local config = {}

if wezterm.config_builder then
  config = wezterm.config_builder()
end

local focus_log = os.getenv 'WEZTERM_SMEAR_FOCUS_LOG'

local function append_focus_event(window, pane)
  if not focus_log or focus_log == '' then
    return
  end

  local file, err = io.open(focus_log, 'a')
  if not file then
    wezterm.log_error('cursor-smear harness failed to open focus log: ' .. tostring(err))
    return
  end

  local pane_id = pane and pane:pane_id() or -1
  local line = string.format(
    '{"event":"window-focus-changed","ts":"%s","window_id":%d,"pane_id":%d,"focused":%s}\n',
    os.date('!%Y-%m-%dT%H:%M:%SZ'),
    window:window_id(),
    pane_id,
    tostring(window:is_focused())
  )

  file:write(line)
  file:close()
end

wezterm.on('window-focus-changed', append_focus_event)

config.enable_tab_bar = false
config.use_fancy_tab_bar = false
config.enable_scroll_bar = false
config.window_padding = {
  left = 0,
  right = 0,
  top = 0,
  bottom = 0,
}
config.initial_cols = 80
config.initial_rows = 24
config.font_size = 16.0
config.line_height = 1.0
config.animation_fps = 60
config.cursor_blink_rate = 0
config.default_cursor_style = 'SteadyBar'
config.cursor_smear_duration_ms = 1000
config.cursor_trail_size = 0.7
config.check_for_updates = false
config.automatically_reload_config = false
config.window_close_confirmation = 'NeverPrompt'
config.adjust_window_size_when_changing_font_size = false
config.window_background_opacity = 1.0
config.text_background_opacity = 1.0
config.colors = {
  foreground = '#000000',
  background = '#000000',
  cursor_bg = '#ffffff',
  cursor_border = '#ffffff',
  cursor_fg = '#000000',
  selection_fg = '#000000',
  selection_bg = '#202020',
}

return config
