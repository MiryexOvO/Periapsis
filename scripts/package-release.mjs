// Package the current working trees, never local credentials or experimental output.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { existsSync, readFileSync, writeFileSync, mkdirSync, readdirSync, copyFileSync, mkdtempSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const plugin = path.resolve(root, '../obs-periapsis');
const withPlugin = process.argv.slice(2).includes('--plugin');
if (process.argv.slice(2).some(a => a !== '--plugin')) throw Error('Usage: node scripts/package-release.mjs [--plugin]');
const version = readFileSync(path.join(root, 'VERSION'), 'utf8').trim();
if (!/^\d+\.\d+\.\d+$/.test(version)) throw Error('Invalid VERSION');
function run(command, args, cwd = root) {
  const p = spawnSync(command, args, { cwd, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  if (p.error || p.status !== 0) throw Error(`${command} failed: ${p.error ?? p.stderr}`);
  return p.stdout.trim();
}
function hash(file) { return createHash('sha256').update(readFileSync(file)).digest('hex'); }
function copy(from, to) { mkdirSync(path.dirname(to), { recursive: true }); copyFileSync(from, to); }
function tree(dir) {
  return readdirSync(dir, { withFileTypes: true }).sort((a,b) => a.name.localeCompare(b.name)).flatMap(e => {
    const p = path.join(dir, e.name);
    if (e.isSymbolicLink()) throw Error(`Symlinks not allowed in release inputs: ${p}`);
    return e.isDirectory() ? tree(p) : [p];
  });
}
function relative(base, p) { return path.relative(base, p).replaceAll('\\', '/'); }
run(process.execPath, ['scripts/license-boundary-check.mjs']);
const metadata = JSON.parse(run('cargo', ['metadata', '--locked', '--offline', '--format-version', '1']));
for (const p of metadata.packages.filter(p => metadata.workspace_members.includes(p.id))) {
  if (p.version !== version) throw Error(`Version mismatch: ${p.name}`);
}
if (JSON.parse(readFileSync(path.join(root, 'package.json'))).version !== version) throw Error('package.json mismatch');
const platform = `${process.platform === 'win32' ? 'Windows' : process.platform === 'linux' ? 'Linux' : process.platform}-${process.arch}`;
const binaries = ['peri', 'peri-webtransport'].map(n => n + (process.platform === 'win32' ? '.exe' : ''));
for (const name of binaries) {
  const output = run(path.join(root, 'target/release', name), ['--version']);
  if (!output.startsWith(`${name.replace(/\.exe$/, '')} ${version} / protocol 1.0 / ALPN peri-dev-02`)) throw Error(`Stale binary: ${output}`);
}
const parent = path.join(root, 'dist/releases', version);
mkdirSync(parent, { recursive: true });
const out = mkdtempSync(path.join(parent, 'build-'));
const source = path.join(out, `PERI-${version}-source`);
const server = path.join(out, `PERI-${version}-${platform}`);
const coreSource = path.join(source, 'periapsis');
const pluginSource = path.join(source, 'obs-periapsis');
const sourceHashes = {};
function snapshot(repo, dest, allow) {
  const files = run('git', ['ls-files', '--cached', '--others', '--exclude-standard', '-z'], repo).split('\0').filter(Boolean);
  for (const file of [...new Set(files)].sort()) {
    if (!allow(file) || /(^|\/)(\.env[^/]*|server\.json)$|\.(pem|key|der|pfx|p12|zip|exe|dll|log)$/i.test(file)) continue;
    const from = path.join(repo, file);
    if (!existsSync(from)) continue;
    copy(from, path.join(dest, file));
    sourceHashes[relative(source, path.join(dest, file))] = hash(from);
  }
}
snapshot(root, coreSource, f => /^(crates|docs|deploy|scripts|tests|LICENSES|\.github)\//.test(f) || /^(Cargo\.(toml|lock)|package\.json|pnpm-lock\.yaml|pnpm-workspace\.yaml|tsconfig\.json|README\.md|CHANGELOG\.md|VERSION|LICENSING\.md|LICENSE[^/]*|THIRD-PARTY-NOTICES\.md|\.gitignore)$/.test(f));
if (withPlugin) snapshot(plugin, pluginSource, f => /^(native|tests)\//.test(f) || /^(README\.md|LICENSE|LICENSE-HISTORY\.md|VERSION)$/.test(f));

// Include exact dependency sources as well as notices; this also permits offline Rust builds.
const vendor = path.join(coreSource, 'vendor');
const vendorConfig = run('cargo', ['vendor', '--locked', '--offline', vendor]);
mkdirSync(path.join(coreSource, '.cargo'), { recursive: true });
writeFileSync(path.join(coreSource, '.cargo/config.toml'), vendorConfig.replaceAll(vendor.replaceAll('\\', '/'), 'vendor').replaceAll(vendor.replaceAll('\\', '\\\\'), 'vendor') + '\n');
// Cargo may print absolute directory using native separators; insist on relocatable config.
const config = readFileSync(path.join(coreSource, '.cargo/config.toml'), 'utf8');
if (!/directory = "vendor"/.test(config)) throw Error('Vendor configuration must be relocatable');
const notices = path.join(server, 'licenses');
const inventory = [];
for (const p of metadata.packages.filter(p => p.source)) {
  const base = path.dirname(p.manifest_path);
  const files = tree(base).filter(f => /^(license|licence|copying|notice|copyright)([._-]|$)/i.test(path.basename(f)));
  for (const f of files) copy(f, path.join(notices, `${p.name}-${p.version}`, relative(base, f)));
  inventory.push({ name: p.name, version: p.version, license: p.license, source: p.source, notices: files.map(f => relative(base, f)) });
}
for (const f of ['LICENSE', 'LICENSE-HISTORY.md', 'LICENSING.md', 'THIRD-PARTY-NOTICES.md']) copy(path.join(root, f), path.join(server, f));
for (const f of tree(path.join(root, 'LICENSES'))) copy(f, path.join(server, 'LICENSES', relative(path.join(root, 'LICENSES'), f)));
copy(path.join(root, 'scripts/vendor/hls.js/LICENSE'), path.join(notices, 'hls.js/LICENSE'));
writeFileSync(path.join(server, 'dependencies.json'), JSON.stringify(inventory, null, 2) + '\n');
for (const name of binaries) copy(path.join(root, 'target/release', name), path.join(server, name));
for (const f of ['README.md', 'CHANGELOG.md', 'VERSION']) copy(path.join(root, f), path.join(server, f));
for (const f of tree(path.join(root, 'docs'))) copy(f, path.join(server, 'docs', relative(path.join(root, 'docs'), f)));
for (const f of tree(path.join(root, 'deploy'))) copy(f, path.join(server, 'deploy', relative(path.join(root, 'deploy'), f)));
const git = repo => ({ commit: run('git', ['rev-parse', 'HEAD'], repo), dirty: !!run('git', ['status', '--porcelain'], repo) });
const componentLicenses = Object.fromEntries(metadata.packages.filter(p => metadata.workspace_members.includes(p.id)).map(p => [p.name, p.license]));
const manifest = { version, protocol: '1.0', alpn: 'peri-dev-02', componentLicenses, platform, created: new Date().toISOString(), rustc: run('rustc', ['--version']), core: git(root), ...(withPlugin ? {plugin: git(plugin)} : {}), sourceHashes };
writeFileSync(path.join(source, 'BUILD.json'), JSON.stringify(manifest, null, 2) + '\n');
writeFileSync(path.join(server, 'BUILD.json'), JSON.stringify(manifest, null, 2) + '\n');
const archives = [];
for (const dir of [source, server]) {
  const name = path.basename(dir) + '.tar.gz';
  run('tar', ['-czf', path.join(out, name), '-C', out, path.basename(dir)]);
  archives.push(name);
}
if (withPlugin) {
  const build = JSON.parse(readFileSync(path.join(plugin, 'release-build.json'), 'utf8').replace(/^\uFEFF/, ''));
  if (build.version !== version) throw Error('Stale OBS build version');
  for (const [file, digest] of Object.entries(build.sources)) if (hash(path.join(plugin, file)) !== digest) throw Error(`OBS source changed after build: ${file}`);
  const zip = path.join(plugin, 'PERI-OBS-Windows-x64.zip');
  if (hash(zip) !== build.sha256) throw Error('OBS ZIP changed after build');
  if (hash(path.join(root, 'target/release/peri_obs.dll')) !== build.coreSha256) throw Error('OBS core DLL changed after build');
  const name = `PERI-OBS-${version}-Windows-x64.zip`;
  const stage = path.join(out, `PERI-OBS-${version}-Windows-x64`);
  mkdirSync(stage);
  run('tar', ['-xf', zip, '-C', stage]);
  for (const f of tree(notices)) copy(f, path.join(stage, 'licenses', relative(notices, f)));
  for (const f of ['dependencies.json', 'LICENSING.md', 'THIRD-PARTY-NOTICES.md', 'BUILD.json']) copy(path.join(server, f), path.join(stage, f));
  const quote = s => "'" + s.replaceAll("'", "''") + "'";
  run('powershell.exe', ['-NoProfile', '-Command', `$ErrorActionPreference='Stop'; Compress-Archive -Path ${quote(stage + '/*')} -DestinationPath ${quote(path.join(out, name))}`]);
  archives.push(name);
}
writeFileSync(path.join(out, 'SHA256SUMS'), archives.map(f => `${hash(path.join(out, f))}  ${f}`).join('\n') + '\n');
writeFileSync(path.join(out, 'BUILD.json'), JSON.stringify(manifest, null, 2) + '\n');
console.log(out);
console.log('Local review artifacts. Commit final sources and rebuild before public release.');
