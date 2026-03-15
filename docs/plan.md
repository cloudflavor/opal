# `opal plan`

`opal plan` lets you explore the pipeline graph without spinning up containers. It uses the same parser and rule evaluation as `opal run`, so the output is a faithful preview of what will execute when you launch a real run.

```
opal plan --pipeline .gitlab-ci.yml --workdir .
```

## What you see

For each stage (printed in order), Opal lists:

- **Job** – The final expanded job name, including any `parallel`/`matrix` labels.
- **Info line** – Whether the job runs `on_success`, `manual`, `delayed`, etc., together with retry/allow-failure hints.
- **Dependencies** – The jobs that must finish first. When no explicit `needs:` exist, the line reads “stage ordering”.
- **Needs** – The original `needs:` entries, annotated with `(artifacts)` or `(external)` so you know what will be fetched.
- **Artifacts/Environment** – Any declared artifact paths or environments, including URLs and auto-stop timers.
- **Timeout/resource group** – Optional constraints that affect scheduling.

Jobs filtered out by `workflow`, top-level `only/except`, or job-level `rules:when:never` simply do not appear, so the remaining list is exactly what Opal would try to execute.

## Tips

- Set `OPAL_RUN_MANUAL=1` before running `opal plan` if you want manual jobs to be marked as auto-triggered.
- Use `CI_PIPELINE_SOURCE`, `CI_COMMIT_BRANCH`, and related GitLab variables in your shell to see how different contexts affect the plan.
- Pair `opal plan` with the TUI help overlay’s documentation to share pipeline behavior with teammates who don’t have the repo checked out.
