#!/usr/bin/env node
// Run the Rust tests for the crates a change touches and every workspace
// crate that depends on them, instead of the whole workspace.
//
//   node scripts/test-rust-changed.mjs [--base <ref>] [--print] [-- <cargo test args>]
//
// The change is the working tree (committed, staged, unstaged and untracked)
// against the merge base with `--base` (default origin/main). A file under
// crates/<dir>/ selects that crate. A file outside crates/ selects every crate
// whose sources name its path or one of its directories (include_str!, a
// fixture dir), so fixtures, browser-bundle/, ui/ and skill/scripts/ edits
// reach the crates that read them. Cargo.toml, Cargo.lock,
// rust-toolchain.toml and .cargo/ select the whole workspace. Then every
// workspace crate that depends on a selected one, dev-dependencies included,
// joins the run.
//
// The full cargo output goes to target/test-rust-changed.log; stdout gets the
// selection, then one summary line (and the failing test names). The exit
// code is cargo's. `--print` only prints the selection.
import fs from 'node:fs';
import path from 'node:path';
import { execFileSync, spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const ROOT = path.resolve(fileURLToPath(new URL('..', import.meta.url)));
const WORKSPACE_FILES = [/^Cargo\.(toml|lock)$/, /^rust-toolchain(\.toml)?$/, /^\.cargo\//];

const argv = process.argv.slice(2);
const dashDash = argv.indexOf('--');
const own = dashDash === -1 ? argv : argv.slice(0, dashDash);
const passthrough = dashDash === -1 ? [] : argv.slice(dashDash + 1);
if (own.includes('--help') || own.includes('-h')) {
  console.log('usage: node scripts/test-rust-changed.mjs [--base <ref>] [--print] [-- <cargo test args>]');
  process.exit(0);
}
const baseAt = own.indexOf('--base');
const base = baseAt === -1 ? 'origin/main' : own[baseAt + 1];
if (!base) fail('--base needs a ref');
const printOnly = own.includes('--print');

const git = (args) => execFileSync('git', args, { cwd: ROOT, encoding: 'utf-8', maxBuffer: 64 * 1024 * 1024 });
const lines = (text) => text.split(/\r?\n/).filter(Boolean);

let mergeBase;
try {
  mergeBase = git(['merge-base', base, 'HEAD']).trim();
} catch {
  fail(`no merge base with ${base}; fetch it first or pass --base <ref>`);
}
const changed = [...new Set([
  // --no-renames lists both sides of a move, so a file moved out of a crate
  // still selects the crate it left.
  ...lines(git(['diff', '--name-only', '--no-renames', mergeBase])),
  ...lines(git(['ls-files', '--others', '--exclude-standard'])),
])];

const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--format-version', '1', '--no-deps'], {
  cwd: ROOT, encoding: 'utf-8', maxBuffer: 64 * 1024 * 1024,
}));
const packages = metadata.packages.map((p) => ({
  name: p.name,
  dir: path.relative(ROOT, path.dirname(p.manifest_path)).split(path.sep).join('/'),
  deps: p.dependencies.filter((d) => d.path).map((d) => d.name),
}));
const byDir = new Map(packages.map((p) => [p.dir, p.name]));

const whole = changed.filter((f) => WORKSPACE_FILES.some((re) => re.test(f)));
const selected = new Set();
const reasons = new Map();
const pick = (name, why) => {
  if (!selected.has(name)) reasons.set(name, why);
  selected.add(name);
};

const external = [];
for (const file of changed) {
  const m = /^(crates\/[^/]+)\//.exec(file);
  if (m) {
    const name = byDir.get(m[1]);
    if (name) pick(name, file);
  } else if (!whole.includes(file)) {
    external.push(file);
  }
}
for (const file of external) {
  // Crates name what they read either relative to their manifest
  // (`include_str!("../../../skill/scripts/live-browser.js")`, a
  // `CARGO_MANIFEST_DIR/../../browser-bundle` dir) or from the repo root
  // (`repo_root().join("tests/fixtures/antipatterns/x.html")`). Search for the
  // file itself, a manifest-relative read of a directory above it
  // (`../<dir>"`), and a repo-relative path under a directory at least two
  // levels deep (`tests/fixtures/antipatterns/`). A crate that includes one
  // file of scripts/ or merely mentions `package.json` is not selected by
  // every edit there.
  const parts = file.split('/');
  const terms = new Set([`../${file}`]);
  if (parts.length > 2) terms.add(file);
  for (let i = parts.length - 1; i >= 1; i--) {
    const dir = parts.slice(0, i).join('/');
    terms.add(`../${dir}"`);
    if (i >= 2) terms.add(`${dir}/`);
  }
  let hits = '';
  try {
    hits = git(['grep', '-l', '-F', ...[...terms].flatMap((t) => ['-e', t]), '--', 'crates']);
  } catch {
    continue; // git grep exits 1 when nothing matches
  }
  for (const hit of lines(hits)) {
    const name = byDir.get(hit.split('/').slice(0, 2).join('/'));
    if (name) pick(name, file);
  }
}

// Reverse dependencies: anything in the workspace that depends on a selected
// crate is selected too.
for (let grew = true; grew;) {
  grew = false;
  for (const p of packages) {
    if (selected.has(p.name)) continue;
    const dep = p.deps.find((d) => selected.has(d));
    if (dep) {
      pick(p.name, `depends on ${dep}`);
      grew = true;
    }
  }
}

if (whole.length) {
  console.log(`Whole workspace: ${whole.join(', ')} changed against ${base}.`);
} else if (!selected.size) {
  console.log(`No Rust crate is affected by the ${changed.length} changed file(s) against ${base}.`);
  process.exit(0);
} else {
  console.log(`${selected.size} of ${packages.length} crates against ${base}:`);
  for (const name of [...selected].sort()) console.log(`  ${name}  (${reasons.get(name)})`);
}

const cargoArgs = ['test', ...(whole.length ? ['--workspace'] : [...selected].sort().flatMap((n) => ['-p', n])), '--no-fail-fast', ...passthrough];
console.log(`cargo ${cargoArgs.join(' ')}`);
if (printOnly) process.exit(0);

const logPath = path.join(metadata.target_directory, 'test-rust-changed.log');
fs.mkdirSync(path.dirname(logPath), { recursive: true });
const log = fs.createWriteStream(logPath);
console.log(`Log: ${logPath}`);
const started = Date.now();
const child = spawn('cargo', cargoArgs, { cwd: ROOT, stdio: ['ignore', 'pipe', 'pipe'] });
let output = '';
for (const stream of [child.stdout, child.stderr]) {
  stream.on('data', (chunk) => {
    log.write(chunk);
    output += chunk;
  });
}
child.on('close', (code, signal) => {
  log.end();
  let passed = 0;
  let failed = 0;
  for (const m of output.matchAll(/^test result: \w+\. (\d+) passed; (\d+) failed/gm)) {
    passed += Number(m[1]);
    failed += Number(m[2]);
  }
  const failures = [...output.matchAll(/^test (\S+) \.\.\. FAILED$/gm)].map((m) => m[1]);
  const secs = Math.round((Date.now() - started) / 1000);
  const status = code === 0 ? 'ok' : `FAILED (exit ${code ?? signal})`;
  console.log(`${status}: ${passed} passed, ${failed} failed in ${secs}s`);
  for (const name of failures) console.log(`  FAILED ${name}`);
  if (code !== 0 && !failures.length) console.log(`  see the log for the error: ${logPath}`);
  process.exit(code ?? 1);
});

function fail(message) {
  console.error(`test-rust-changed: ${message}`);
  process.exit(2);
}
