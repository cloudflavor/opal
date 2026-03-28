Job: {{job_name}}
Source job: {{source_name}}
Stage: {{stage}}
Runner: {{runner_summary}}

{{failure_hint}}

Selected job YAML:
```yaml
{{job_yaml}}
```

Pipeline plan summary:
```text
{{pipeline_summary}}
```

Runtime summary:
```text
{{runtime_summary}}
```

Recent job log excerpt:
```text
{{log_excerpt}}
```

Respond with:
1. Root cause
2. Why you think that
3. Concrete next steps
