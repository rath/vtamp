# tmux status bar

Keep the current track visible after detaching the TUI:

```text
▶ Training Montage · 0:56 / 3:39    22:41 30-Sep-26
```

The optional plugin displays the title and elapsed / total time. Pausing keeps the
track visible with `Ⅱ`; stopping playback, clearing the current track, or shutting
down the server hides the segment. It inherits your status-bar colors and needs no
Nerd Font, jq, Python, or additional background service.

Install the latest vtamp binary with [Homebrew or from source](../README.md#install),
then add this to `~/.tmux.conf`, **after any theme or other status-bar settings**:

```tmux
set -g status-interval 1
set -g status-right-length 120
set -g status-right '#{vtamp} %H:%M %d-%b-%y'
run-shell '/absolute/path/to/vtamp/vtamp.tmux'
```

Replace the path with your source checkout and reload with `tmux source-file ~/.tmux.conf`.
You can put `#{vtamp}` anywhere in your existing `status-right` or `status-left`
instead of replacing its contents. The plugin only substitutes that placeholder;
it does not change your colors, keys, refresh interval, or status-bar length.
Repeated loading does not insert duplicate segments.

For [TPM](https://github.com/tmux-plugins/tpm), use `set -g @plugin 'rath/vtamp'`
instead of the `run-shell` line above, before your existing TPM initialization.
Press your tmux prefix followed by `I` to install the plugin. TPM downloads the
repository but does not build Rust code: if vtamp is not installed yet, run
`brew install rath/tap/vtamp` or `cargo install --locked --path ~/.tmux/plugins/vtamp`
(adjust for a custom TPM directory).

Optional settings, placed before loading the plugin:

```tmux
# Executable path only, not a shell command. Useful if tmux has an older PATH.
set -g @vtamp-bin '/absolute/path/to/vtamp'
set -g @vtamp-max-width 50
set -g @vtamp-show-artist off
```

Without `@vtamp-bin`, the plugin searches tmux's PATH, then `~/.cargo/bin/vtamp`.
The width includes the indicator and times (20–200 terminal cells, default 50).
Long titles are shortened with `…`, preserving whole Unicode graphemes and the
times. Set `@vtamp-show-artist on` to include the artist after the title. Missing
binaries produce an empty segment; diagnose installation with `vtamp --version`.

The underlying command is also available directly:

```sh
vtamp tmux status
vtamp tmux status --max-width 70 --show-artist
vtamp tmux status --json
```

It reads the existing server with a 500 ms deadline and never starts one. An
unreachable, busy, incompatible, or unresponsive server produces an empty line
instead of leaving stale text or errors in the bar. JSON output uses the usual
response envelope with `data.text`; both modes return tmux-escaped text, so a
literal `#` in metadata becomes `##`. Use `vtamp status --json` for raw metadata
or connection diagnostics. `VTAMP_HOME` is supported; for tmux jobs, set it in
the tmux server environment with `tmux set-environment -g VTAMP_HOME /absolute/path`.
