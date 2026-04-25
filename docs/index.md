# Opal Documentation

Opal executes GitLab-style CI pipelines locally so you can iterate without pushing to a remote runner. This documentation gives you the high-level tour of how the system is structured and where to look for specific topics.

## Core ideas

- **Deterministic pipelines** – Opal parses `.gitlab-ci.yml`, resolves `include:` files, and constructs the same DAG GitLab would run. Jobs inherit defaults (`before_script`, `after_script`, variables, and images) so the local run behaves like production.
- **Multiple execution engines** – Use Docker, Podman, Apple `container`, OrbStack, or `sandbox` (Anthropic `srt`) for supported local execution. `nerdctl` remains available as a Linux-oriented option when the underlying environment is directly usable. On macOS, Apple `container` is a particularly good fit for Opal because it runs each container in its own lightweight VM instead of routing everything through one shared Linux VM. The executor normalizes container names (`opal-<pipeline>-<run>-<stage>-<job>`) and manages artifact mounts automatically.
- **Artifact discipline** – Each run gets a session directory under `$XDG_DATA_HOME/opal/<run-id>/` (default `~/.local/share/opal/<run-id>/`). Job artifacts are stored under `$XDG_DATA_HOME/opal/<run-id>/<job>/artifacts/` and shared read-only with downstream jobs that declare `needs: { artifacts: true }`. Host `target/` is never touched by job artifacts; the workspace stays clean.
- **Friendly TUI** – The Ratatui interface shows job tabs, panes for run history and live logs, plus a contextual help overlay. Every action is bound to a key so you can drive the UI without a mouse, and the bundled docs can be opened directly with `?`.

The markdown files in `docs/` are embedded into the Opal binary at build time. Inside the TUI, press `?` to open the help and documentation viewer.

## Folder layout

```
opal/
├─ crates/        # Rust workspace members, currently `opal`
├─ docs/          # Packaged documentation displayed inside the TUI help window
├─ notes/         # Local developer notes (ignored from version control)
└─ .opal/         # Optional repo-scoped config/secrets (runtime data lives in $XDG_DATA_HOME/opal/<run-id>/…)
```

Keep contributor-facing documentation under `docs/`. The help viewer bundles everything in this directory when the binary is built, so end users always have up-to-date references even if they do not clone the repository.

See `docs/install.md` to get Opal onto your machine, `docs/quickstart.md` to run your first pipeline, `docs/cli.md` for the command-line surface, and `docs/ui.md` for the complete keyboard reference. Use `docs/plan.md` for a focused walkthrough of Opal Plan, refer to `docs/pipeline.md` for deeper implementation details, check `docs/gitlab-parity.md` for the current GitLab feature coverage and parity gaps, use `docs/ai.md` for AI-assisted troubleshooting usage, and `docs/ai-config.md` for the AI backend and prompt configuration surface.

For runtime layout and local state, see `docs/storage.md`.
