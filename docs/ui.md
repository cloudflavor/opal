# User Interface

The Ratatui-based UI is optimized for keyboard navigation. This document complements the in-app shortcut list and explains each pane in detail.

The documentation files under `docs/` are embedded into the Opal binary at build time. Press `?` anywhere in the TUI to open the help and documentation viewer.

## Layout

1. **Runs sidebar** – Shows the current run plus history. It starts hidden by default; press `H` to show or hide it. Collapse/expand tree nodes with `←/→/h/l`. Press `Enter` to load a past run’s jobs and logs into the main view.
2. **Job tabs** – Each tab represents a job or a job variant. Use `j/k` or the arrow keys to change tabs. Colors indicate status:
   - Cyan: waiting on dependencies
   - Yellow: running
   - Green: success
   - Red: failed
3. **Info panel** – Displays metadata for the selected job (stage, image, runtime, error message, manual/needs state, and runner details such as engine, architecture, plus CPU and memory when Opal controls them).
4. **Job YAML pane** – Shows the selected job definition from `.gitlab-ci.yml`. Use `Tab` to focus it, `↑/↓` or `PgUp/PgDn` to scroll, `y` to open it in your pager, and `Y` to hide or restore the pane.
5. **Plan pane** – Shows the pipeline plan that Opal evaluated for this run (stage order, dependencies, manual/delayed gates, artifact summaries). Scroll with `[` / `]`, page with `{` / `}`, jump to top/bottom with `\` / `|`, and press `p` to open the full plan in your pager when you need more context.
6. **Log pane** – Streams job output live. Scroll with `↑/↓`, `PgUp/PgDn`, `Ctrl+u/d`, `g/G`, etc. Press `o` to open the current log or AI analysis view in your pager. Press `a` to analyze the selected job, and press `a` again to toggle between the normal log view and the saved/streamed analysis view. Press `A` to preview the exact rendered AI prompt that Opal will send, and press `A` again to close that preview. These actions also work when you are viewing a job loaded from history.
7. **Shortcuts footer** – The bottom footer stays under the main pane so the active shortcuts remain visible even when the history sidebar is hidden.
8. **Job resources** – When you expand a run in the history sidebar, each job now lists its artifacts and caches. Press `Enter` on any artifact or cache directory to render a tree in the preview pane, or press `Enter` on an artifact file to read it directly.
   When runtime-object preservation is enabled, this section also surfaces the recorded job container, service network, service container names, and a generated runtime summary file for inspection. Press `Enter` on the summary file or runtime info rows to load the full text into the preview pane.

## Help overlay

- Press `?` to toggle the overlay.
- The footer always reminds you that `?` or `Esc` will close the window.
- Use `1-9` to open embedded docs. When a document is open, `←/→` switch between files, `S` jumps back to the shortcuts view, and the usual scrolling keys work inside the reader.

## Mouse support

Mouse events are optional but supported:

- Scroll wheel over the log pane scrolls output.
- Clicking on tabs or sidebar items moves focus accordingly.
- Double-clicking a job tab opens the log in the pager.

## Troubleshooting

- If the UI freezes, ensure your terminal supports alternate-screen mode and 256 colors.
- Press `Ctrl+C` to exit immediately; Opal will attempt to stop running jobs.
- Logs live under `$OPAL_HOME/<run-id>/logs/` (default `~/.opal/<run-id>/logs/`) if you need to inspect them outside the UI.
- Saved AI analysis lives under `$OPAL_HOME/<run-id>/<job-slug>/analysis/` when AI analysis saving is enabled.
