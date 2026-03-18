# Opal Documentation

Opal executes GitLab-style CI pipelines locally so you can iterate without pushing to a remote runner. This documentation gives you the high-level tour of how the system is structured and where to look for specific topics.

## Core ideas

- **Deterministic pipelines** – Opal parses `.gitlab-ci.yml`, resolves `include:` files, and constructs the same DAG GitLab would run. Jobs inherit defaults (`before_script`, `after_script`, variables, and images) so the local run behaves like production.
- **Multiple execution engines** – Use Docker, Podman, NerdCTL, or the built-in sandbox runtime. The executor normalizes container names (`opal-<pipeline>-<run>-<stage>-<job>`) and manages artifact mounts automatically.
- **Artifact discipline** – Job outputs land under `$OPAL_HOME/<pipeline>/<job>/` (defaults to `~/.opal/<pipeline>/<job>/`) and are shared read-only with downstream jobs that declare `needs: { artifacts: true }`. Host `target/` is never touched; the workspace stays clean.
- **Friendly TUI** – The Ratatui interface shows job tabs, panes for run history and live logs, plus a contextual help overlay. Every action is bound to a key so you can drive the UI without a mouse.

## Folder layout

```
opal/
├─ docs/          # Packaged documentation displayed inside the TUI help window
├─ notes/         # Local developer notes (ignored from version control)
├─ src/           # Application source
└─ .opal/         # Optional repo-scoped config/secrets (runtime data lives in $OPAL_HOME/<pipeline>/<job>/…)
```

Keep contributor-facing documentation under `docs/`. The help viewer bundles everything in this directory when the binary is built, so end users always have up-to-date references even if they do not clone the repository.

See `docs/quickstart.md` to run your first pipeline and `docs/ui.md` for the complete keyboard reference. Use `docs/plan.md` for a focused walkthrough of `opal plan`, refer to `docs/pipeline.md` for deeper implementation details, and check `docs/gitlab-parity.md` for the current GitLab feature coverage and parity gaps.
