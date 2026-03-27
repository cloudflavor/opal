<script lang="ts">
  const commandSections = [
    {
      title: 'Opal Run',
      command: 'opal run --workdir . --pipeline .gitlab-ci.yml',
      body: 'Runs the local pipeline with the full TUI, live logs, history, and manual-job controls.',
      href: '/docs/quickstart'
    },
    {
      title: 'Opal Run --no-tui',
      command: 'opal run --no-tui',
      body: 'Runs the pipeline in plain terminal mode. Useful for scripts, CI-style local checks, and sharable terminal output.',
      href: '/docs/cli'
    },
    {
      title: 'Opal Plan',
      command: 'opal plan --workdir . --pipeline .gitlab-ci.yml',
      body: 'Shows the evaluated DAG, dependencies, gates, and scheduling decisions without starting containers.',
      href: '/docs/plan'
    },
    {
      title: 'Opal View',
      command: 'opal view',
      body: 'Opens the history/log browser for previous runs so you can inspect logs, artifacts, and cached outputs after the fact.',
      href: '/docs/ui'
    }
  ];
</script>

<svelte:head>
  <title>Opal Docs</title>
</svelte:head>

<section class="hero">
  <p class="eyebrow">Opal</p>
  <h1>Run GitLab-style pipelines locally, with the current working tree you are actually editing.</h1>
  <p class="lede">Opal is a terminal-first local runner for GitLab pipelines. It evaluates `.gitlab-ci.yml`, keeps local Git-aware behavior where it matters, and gives you fast ways to run, inspect, and debug jobs without pushing to a remote runner.</p>
</section>

<section class="intro-grid">
  <div class="intro-card">
    <h2>What Opal is for</h2>
    <p>Use Opal when you want to debug pipeline logic, container behavior, services, artifacts, and job selection locally while staying within reasonable distance of how a runner behaves.</p>
  </div>
  <div class="intro-card">
    <h2>Where to start</h2>
    <p>Start with `Opal Run` for an interactive local run, `Opal Plan` when you only need the evaluated DAG, and `Opal View` when you want to inspect prior run history.</p>
  </div>
</section>

<section class="command-sections">
  {#each commandSections as section}
    <a class="command-card" href={section.href}>
      <div class="command-head">
        <h2>{section.title}</h2>
        <span>Open docs →</span>
      </div>
      <pre>{section.command}</pre>
      <p>{section.body}</p>
    </a>
  {/each}
</section>

<style>
  .hero {
    padding: 3rem 3rem 1.25rem;
    max-width: 980px;
  }
  .eyebrow {
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--accent);
    font-size: 0.75rem;
    font-weight: 700;
  }
  h1 {
    margin: 0.35rem 0 0.85rem;
    font-size: clamp(2rem, 5vw, 3.5rem);
    line-height: 1.05;
    color: var(--text-strong);
  }
  .lede {
    max-width: 68ch;
    color: var(--text-soft);
    font-size: 1.08rem;
    line-height: 1.6;
  }
  .intro-grid,
  .command-sections {
    padding: 0 3rem 3rem;
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(280px, 1fr));
    gap: 1rem;
  }
  .command-sections {
    padding-top: 0.25rem;
  }
  .intro-card,
  .command-card {
    display: block;
    padding: 1.2rem 1.25rem;
    border: 1px solid var(--border);
    border-radius: 18px;
    background: var(--panel);
    box-shadow: var(--shadow);
    backdrop-filter: blur(16px);
  }
  .command-card {
    text-decoration: none;
    color: inherit;
  }
  .command-card:hover {
    border-color: var(--accent);
    transform: translateY(-1px);
  }
  .intro-card h2,
  .command-card h2 {
    margin: 0 0 0.5rem;
    font-size: 1.1rem;
    color: var(--text-strong);
  }
  .intro-card p,
  .command-card p {
    margin: 0;
    color: var(--text-soft);
    line-height: 1.45;
  }
  .command-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    margin-bottom: 0.75rem;
  }
  .command-head span {
    color: var(--accent);
    font-size: 0.9rem;
    font-weight: 600;
  }
  pre {
    margin: 0 0 0.85rem;
    padding: 0.85rem 0.95rem;
    border-radius: 12px;
    border: 1px solid var(--border);
    background: color-mix(in srgb, var(--panel-strong) 82%, transparent);
    color: var(--text-strong);
    overflow: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.95rem;
  }
</style>
