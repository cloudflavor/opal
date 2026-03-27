<script lang="ts">
  import AsciinemaEmbed from '$lib/AsciinemaEmbed.svelte';

  const demos = [
    {
      title: 'Opal Run',
      command: 'opal run',
      body: 'Use the full TUI when you want to watch jobs live, inspect history, browse artifacts, and step through the pipeline interactively.',
      href: '/docs/quickstart#run-the-pipeline',
      cast: 'LxDIEb87AyDtDM5c'
    },
    {
      title: 'Opal Plan',
      command: 'opal plan',
      body: 'Print the evaluated DAG, dependencies, gates, and scheduling decisions without starting containers.',
      href: '/docs/plan#preview-the-dag',
      cast: '6rP1p3H4vtA7Orr8'
    },
    {
      title: 'Opal Run --no-tui',
      command: 'opal run --no-tui',
      body: 'Use plain terminal mode for scriptable local checks, sharable terminal output, and CI-like runs without the interactive interface.',
      href: '/docs/quickstart#run-without-the-tui',
      cast: '2Q2mFyHkwXMoI9e0'
    },
    {
      title: 'Opal View',
      command: 'opal view',
      body: 'Open prior run history, logs, artifacts, and cache metadata after the fact.',
      href: '/docs/ui#layout',
      cast: '3eTpVFphkhQKDZB9'
    }
  ];

  const references = [
    {
      title: 'CLI Reference',
      command: 'opal --help',
      body: 'See the complete command-line surface, flags, environment variables, and common examples.',
      href: '/docs/cli'
    },
    {
      title: 'Pipeline Model',
      command: '.gitlab-ci.yml',
      body: 'Understand how Opal evaluates includes, rules, services, caches, artifacts, and local workspaces.',
      href: '/docs/pipeline'
    }
  ];

  const downloads = [
    {
      title: 'macOS · Apple Silicon',
      target: 'aarch64-apple-silicon',
      install: 'curl -L <placeholder> | tar xz',
      href: '#download-placeholder-macos'
    },
    {
      title: 'Linux · ARM64',
      target: 'aarch64-unknown-linux-gnu',
      install: 'curl -L <placeholder> | tar xz',
      href: '#download-placeholder-linux-arm64'
    },
    {
      title: 'Linux · AMD64',
      target: 'x86_64-unknown-linux-gnu',
      install: 'curl -L <placeholder> | tar xz',
      href: '#download-placeholder-linux-amd64'
    }
  ];
</script>

<svelte:head>
  <title>Opal Docs</title>
</svelte:head>

<section class="hero shell">
  <div class="hero-copy">
    <p class="eyebrow">Opal Documentation</p>
    <h1>Run GitLab-style pipelines locally.</h1>
    <p class="lede">Opal is a terminal-first local runner for GitLab pipelines. It evaluates `.gitlab-ci.yml`, keeps local Git-aware behavior where it matters, and gives you fast ways to run, inspect, and debug jobs without pushing to a remote runner.</p>
    <div class="hero-links">
      <a href="/docs/quickstart">Quick Start</a>
      <a href="/docs/cli">CLI Reference</a>
      <a href="/docs/gitlab-parity">GitLab Parity</a>
    </div>
  </div>
  <div class="hero-panel">
    <div class="panel-block">
      <span>Built for</span>
      <strong>local debugging</strong>
      <p>Run against the working tree you are actively editing instead of waiting on a remote runner.</p>
    </div>
    <div class="panel-block">
      <span>Main modes</span>
      <strong>run · run --no-tui · plan · view</strong>
      <p>Use interactive runs, plain terminal runs, DAG-only planning, or post-run history browsing depending on the task.</p>
    </div>
  </div>
</section>

<section class="section shell">
  <div class="section-headline">
    <p class="eyebrow">Install</p>
    <h2 id="download"><a class="section-anchor" href="#download" aria-label="Link to download">#</a>Download the build that matches your machine.</h2>
  </div>

  <div class="install-intro">
    <div>
      <h3>Install from crates.io</h3>
      <pre>cargo install opal</pre>
    </div>
    <a class="reference-doc-link" href="/docs/install">Open install docs →</a>
  </div>

  <div class="install-list">
    {#each downloads as item}
      <section class="install-row" id={item.href.slice(1)}>
        <div class="install-main">
          <h3><a class="section-anchor" href={`#${item.href.slice(1)}`} aria-label={`Link to ${item.title}`}>#</a>{item.title}</h3>
          <p>{item.target}</p>
        </div>
        <pre>{item.install}</pre>
        <a class="reference-doc-link" href={item.href}>Placeholder link →</a>
      </section>
    {/each}
  </div>
</section>

<section class="section shell">
  <div class="section-headline">
    <p class="eyebrow">Demos</p>
    <h2 id="demos"><a class="section-anchor" href="#demos" aria-label="Link to demos">#</a>The main ways people actually use Opal.</h2>
  </div>

  <div class="demo-stack">
    {#each demos as demo}
      <article class="demo-row" id={demo.title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}>
        <div class="demo-text">
          <p class="demo-kicker">Demo</p>
          <h3><a class="section-anchor" href={`#${demo.title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`} aria-label={`Link to ${demo.title}`}>#</a>{demo.title}</h3>
          <p>{demo.body}</p>
          <pre>{demo.command}</pre>
          <a class="demo-link" href={demo.href}>Open docs →</a>
        </div>
        <div class="demo-player">
          <AsciinemaEmbed castId={demo.cast} title={`${demo.title} demo`} />
        </div>
      </article>
    {/each}
  </div>
</section>

<section class="section shell">
  <div class="section-headline">
    <p class="eyebrow">Reference</p>
    <h2 id="reference"><a class="section-anchor" href="#reference" aria-label="Link to reference">#</a>Use these docs when you need precision, not just a demo.</h2>
  </div>

  <div class="reference-list">
    {#each references as reference}
      <section class="reference-row" id={reference.title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}>
        <div class="reference-main">
          <h3><a class="section-anchor" href={`#${reference.title.toLowerCase().replace(/[^a-z0-9]+/g, '-')}`} aria-label={`Link to ${reference.title}`}>#</a>{reference.title}</h3>
          <p>{reference.body}</p>
        </div>
        <pre>{reference.command}</pre>
        <a class="reference-doc-link" href={reference.href}>Open docs →</a>
      </section>
    {/each}
  </div>
</section>

<style>
  .shell {
    max-width: 1260px;
    margin: 0 auto;
    padding-left: 2rem;
    padding-right: 2rem;
  }
  .hero {
    display: grid;
    grid-template-columns: minmax(0, 1.2fr) minmax(320px, 0.8fr);
    gap: 2rem;
    padding-top: 3rem;
    padding-bottom: 3rem;
  }
  .eyebrow,
  .demo-kicker {
    margin: 0 0 0.75rem;
    text-transform: uppercase;
    letter-spacing: 0.14em;
    color: var(--accent);
    font-size: 0.75rem;
    font-weight: 700;
  }
  h1 {
    margin: 0 0 1rem;
    font-size: clamp(2rem, 4vw, 3.6rem);
    line-height: 1.02;
    color: var(--text-strong);
    max-width: 10ch;
  }
  .lede {
    margin: 0;
    max-width: 62ch;
    color: var(--text-soft);
    font-size: 1rem;
    line-height: 1.65;
  }
  .hero-links {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem;
    margin-top: 1.5rem;
  }
  .hero-links a {
    text-decoration: none;
    color: var(--text-strong);
    border: 1px solid var(--border);
    border-radius: 999px;
    padding: 0.55rem 0.85rem;
    background: color-mix(in srgb, var(--panel-strong) 82%, transparent);
  }
  .hero-links a:hover,
  .demo-link:hover,
  .reference-doc-link:hover {
    color: var(--accent);
    border-color: var(--accent);
  }
  .section-anchor {
    display: inline-block;
    margin-right: 0.55rem;
    color: var(--accent);
    text-decoration: none;
    opacity: 0.28;
  }
  .section-headline h2:hover .section-anchor,
  .demo-text h3:hover .section-anchor,
  .reference-main h3:hover .section-anchor,
  .section-anchor:focus {
    opacity: 1;
  }
  .hero-panel {
    display: grid;
    gap: 1rem;
    align-content: start;
  }
  .panel-block {
    padding: 1.2rem 1.25rem;
    border: 1px solid var(--border);
    border-radius: 18px;
    background: color-mix(in srgb, var(--panel) 94%, transparent);
  }
  .panel-block span {
    display: block;
    color: var(--text-soft);
    font-size: 0.85rem;
    margin-bottom: 0.35rem;
  }
  .panel-block strong {
    display: block;
    color: var(--text-strong);
    font-size: 1.05rem;
    margin-bottom: 0.45rem;
  }
  .panel-block p,
  .section-headline p,
  .demo-text p,
  .reference-main p {
    margin: 0;
    color: var(--text-soft);
    line-height: 1.55;
  }
  .section {
    padding-bottom: 3rem;
  }
  .section-headline {
    padding: 1rem 0 1.25rem;
    border-top: 1px solid var(--border);
  }
  .section-headline h2 {
    margin: 0;
    color: var(--text-strong);
    font-size: clamp(1.25rem, 2vw, 1.9rem);
  }
  .demo-stack {
    display: grid;
    gap: 2.25rem;
  }
  .demo-row {
    display: grid;
    grid-template-columns: minmax(300px, 420px) minmax(0, 1fr);
    gap: 1.5rem;
    align-items: start;
  }
  .demo-text {
    display: grid;
    gap: 0.9rem;
  }
  .demo-text h3,
  .reference-main h3 {
    margin: 0;
    color: var(--text-strong);
    font-size: 1.2rem;
  }
  .demo-link,
  .reference-doc-link {
    color: var(--accent);
    text-decoration: none;
    font-weight: 700;
  }
  pre {
    margin: 0;
    padding: 0.8rem 0.95rem;
    border-radius: 12px;
    border: 1px solid var(--border);
    background: color-mix(in srgb, var(--panel-strong) 82%, transparent);
    color: var(--text-strong);
    overflow: auto;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    font-size: 0.95rem;
  }
  .reference-list {
    display: grid;
    gap: 0;
  }
  .install-list {
    display: grid;
    gap: 0;
    padding-bottom: 1rem;
  }
  .install-intro {
    display: flex;
    align-items: end;
    justify-content: space-between;
    gap: 1.5rem;
    padding-bottom: 1.25rem;
  }
  .install-intro h3 {
    margin: 0 0 0.75rem;
    color: var(--text-strong);
    font-size: 1.05rem;
  }
  .install-row,
  .reference-row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(240px, 320px) auto;
    gap: 1.25rem;
    align-items: center;
    padding: 1.1rem 0;
    border-top: 1px solid var(--border);
  }
  .install-row:last-child,
  .reference-row:last-child {
    border-bottom: 1px solid var(--border);
  }
  .install-main h3,
  .install-main p {
    margin: 0;
  }
  .install-main p {
    color: var(--text-soft);
    margin-top: 0.35rem;
  }
  @media (max-width: 1100px) {
    .hero,
    .demo-row,
    .install-row,
    .reference-row {
      grid-template-columns: 1fr;
    }
    .install-intro {
      display: grid;
      align-items: start;
    }
    .shell {
      padding-left: 1.25rem;
      padding-right: 1.25rem;
    }
    h1 {
      max-width: 13ch;
    }
  }
</style>
