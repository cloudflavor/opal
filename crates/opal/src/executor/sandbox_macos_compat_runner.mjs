import fs from 'node:fs';
import path from 'node:path';
import { spawn, spawnSync } from 'node:child_process';
import { pathToFileURL } from 'node:url';

const [settingsPath, jobScript] = process.argv.slice(1);

if (!settingsPath || !jobScript) {
  console.error('usage: <settings-path> <job-script>');
  process.exit(2);
}

if (!fs.existsSync(settingsPath)) {
  console.error(`sandbox settings not found: ${settingsPath}`);
  process.exit(1);
}

const which = spawnSync('which', ['srt'], { encoding: 'utf8' });
if (which.status !== 0) {
  console.error('srt is required for sandbox engine execution');
  process.exit(1);
}

const srtBin = which.stdout.trim();
const srtReal = fs.realpathSync(srtBin);
const srtIndex = path.join(path.dirname(srtReal), 'index.js');
if (!fs.existsSync(srtIndex)) {
  console.error(`failed to locate sandbox runtime index.js from ${srtReal}`);
  process.exit(1);
}

const { SandboxManager } = await import(pathToFileURL(srtIndex).href);
const settings = JSON.parse(fs.readFileSync(settingsPath, 'utf8'));

function patchProfileCommand(wrappedCommand) {
  let patched = wrappedCommand;

  if (!patched.includes('com.apple.container.apiserver')) {
    patched = insertAfterGlobalName(
      patched,
      'com.apple.coreservices.launchservicesd',
      [
        'com.apple.container.apiserver',
        'com.apple.container.cli',
        'com.apple.container.defaults',
        'com.apple.container.network',
        'com.apple.container.plugin',
        'com.apple.container.registry',
        'com.apple.container.resource.role',
        'com.apple.container.resource.anonymous',
        'com.apple.container.xpc.route',
        'com.apple.container.xpc.error',
        'com.apple.containerization',
        'com.apple.containerization.socket',
        'com.apple.containerization.socket-relay',
        'com.apple.containerization.bidirectional-relay',
        'com.apple.containerization.vzvm',
        'com.apple.container.core.container-core-images',
        'com.apple.container.autostart',
        'com.apple.containermanagerd',
      ]
    );
  }

  if (!patched.includes('sysctl.proc_translated')) {
    patched = insertAfterSysctlName(
      patched,
      'sysctl.proc_cputype',
      ['sysctl.proc_translated']
    );
  }

  if (!patched.includes('com.apple.diagnosticd')) {
    patched = insertAfterAllowMachLookup(
      patched,
      'com.apple.SecurityServer',
      ['com.apple.diagnosticd', 'com.apple.analyticsd']
    );
  }

  return patched;
}

function insertAfterGlobalName(source, anchorName, namesToInsert) {
  return insertWithQuoteVariants(
    source,
    quote => `(global-name ${quote}${anchorName}${quote})`,
    namesToInsert.map(name => quote => `  (global-name ${quote}${name}${quote})`)
  );
}

function insertAfterSysctlName(source, anchorName, namesToInsert) {
  return insertWithQuoteVariants(
    source,
    quote => `(sysctl-name ${quote}${anchorName}${quote})`,
    namesToInsert.map(name => quote => `  (sysctl-name ${quote}${name}${quote})`)
  );
}

function insertAfterAllowMachLookup(source, anchorName, namesToInsert) {
  return insertWithQuoteVariants(
    source,
    quote =>
      `(allow mach-lookup (global-name ${quote}${anchorName}${quote}))`,
    namesToInsert.map(
      name => quote =>
        `(allow mach-lookup (global-name ${quote}${name}${quote}))`
    )
  );
}

function insertWithQuoteVariants(source, anchorBuilder, lineBuilders) {
  const variants = ['"', '\\"'];
  for (const quote of variants) {
    const anchor = anchorBuilder(quote);
    if (!source.includes(anchor)) {
      continue;
    }
    const additions = lineBuilders.map(build => build(quote)).join('\n');
    return source.replace(anchor, `${anchor}\n${additions}`);
  }
  return source;
}

function shellQuote(value) {
  return `'${value.replace(/'/g, `'"'"'`)}'`;
}

function debugPatchedProfile(patchedCommand) {
  if (!process.env.SRT_DEBUG_VERBOSE) {
    return;
  }
  const markers = [
    'com.apple.container.apiserver',
    'com.apple.container.cli',
    'com.apple.container.defaults',
    'com.apple.container.network',
    'com.apple.container.registry',
    'sysctl.proc_translated',
    'com.apple.diagnosticd',
  ];
  for (const marker of markers) {
    const present = patchedCommand.includes(marker);
    console.error(`[SandboxDebug] compat profile marker ${marker}: ${present}`);
  }
  for (const line of patchedCommand.split('\n')) {
    if (
      line.includes('com.apple.container') ||
      line.includes('sysctl.proc_translated') ||
      line.includes('com.apple.diagnosticd')
    ) {
      console.error(`[SandboxDebug] compat profile line ${line}`);
    }
  }
}

let exitCode = 1;
try {
  await SandboxManager.initialize(settings);
  const wrapped = await SandboxManager.wrapWithSandbox(`sh ${shellQuote(jobScript)}`);
  const patched = patchProfileCommand(wrapped);
  debugPatchedProfile(patched);

  const code = await new Promise((resolve, reject) => {
    const child = spawn(patched, {
      shell: true,
      stdio: 'inherit',
      env: process.env,
    });
    child.on('error', reject);
    child.on('exit', code => resolve(code ?? 1));
  });
  exitCode = code;
} finally {
  try {
    await SandboxManager.reset();
  } catch {
    // Best-effort cleanup for local runtime state.
  }
}

process.exit(exitCode);
