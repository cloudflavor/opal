# opal

Opal is a terminal-first GitLab runner that lets you inspect plans, stream logs, and dogfood pipelines locally. It embeds your documentation, renders a detailed history pane, and provides enough context to debug failing jobs without leaving the console.

## Features

- Rich Ratatui interface with help overlays, searchable history, and log previews.
- `opal run` to execute the full pipeline against your `.gitlab-ci.yml`.
- `opal plan` for a dry-run preview that respects `rules`, `needs`, and artifacts.
- Directory and artifact viewers so you can dig into build outputs immediately.
- Pager integration for the pipeline plan and embedded Markdown docs.

## Usage

```bash
cargo install --path .
opal run --pipeline .gitlab-ci.yml
```

See the docs in `docs/` (e.g. `docs/plan.md`, `docs/ui.md`) for deeper walkthroughs, shortcuts, and design notes.

## License

Licensed under the Apache License, Version 2.0. See the `LICENSE` file for details.
