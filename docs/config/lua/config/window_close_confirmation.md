---
tags:
  - exit_behavior
---
# `window_close_confirmation`

Whether to display a confirmation prompt when the window is closed by the
windowing environment, either because the user closed it with the window
decorations, or instructed their window manager to close it.

The prompt is displayed as a native system dialog. On Linux, the dialog works
under both X11 and Wayland through the desktop's Zenity integration.

Before prompting, WezTerm inspects the pane process trees on Linux, macOS and
Windows. If every process is listed in
[skip_close_confirmation_for_processes_named](../config/skip_close_confirmation_for_processes_named.md),
the window closes without a prompt. If process inspection is unavailable,
WezTerm prompts rather than risk terminating a running process silently.

Set this to `"NeverPrompt"` if you don't like confirming closing
windows every time.

```lua
config.window_close_confirmation = 'AlwaysPrompt'
```

See also
[skip_close_confirmation_for_processes_named](../config/skip_close_confirmation_for_processes_named.md).

Note that this `window_close_confirmation` option doesn't apply to the default
`CTRL-SHIFT-W` or `CMD-w` key assignments; if you want to change prompts for
those, you will need to override the key shortcut as shown in the
[CloseCurrentTab](../keyassignment/CloseCurrentTab.md) documentation.
