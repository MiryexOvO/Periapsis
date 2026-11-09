import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--offline', '--all-features', '--format-version', '1'], {maxBuffer: 64 * 1024 * 1024}));
const packages = new Map(metadata.packages.map(p => [p.id, p]));
const nodes = new Map(metadata.resolve.nodes.map(n => [n.id, n]));
const sdk = ['peri-types', 'peri-session', 'peri-scheduler', 'peri-native', 'peri-obs'];
for (const name of [...sdk, 'peri-relay', 'peri-webtransport']) {
  const p = metadata.packages.find(p => p.name === name);
  assert.ok(p, `Missing ${name}`);
  assert.equal(p.license, sdk.includes(name) ? 'BSD-3-Clause' : 'AGPL-3.0-only', name);
  if (!sdk.includes(name)) continue;
  const pending = [p.id], seen = new Set();
  while (pending.length) {
    const id = pending.pop(); if (seen.has(id)) continue; seen.add(id);
    const dep = packages.get(id);
    assert.ok(!/\bAGPL\b/.test(dep.license ?? ''), `${name} links AGPL package ${dep.name}`);
    for (const edge of nodes.get(id).deps) {
      if (edge.dep_kinds.some(k => k.kind !== 'dev')) pending.push(edge.pkg);
    }
  }
}
assert.match(readFileSync('docs/LICENSE', 'utf8'), /Apache License/);
if (existsSync('../obs-periapsis/LICENSE')) assert.match(readFileSync('../obs-periapsis/LICENSE', 'utf8'), /BSD 3-Clause/);
console.log('License boundary accepted: five BSD client crates, two AGPL server crates; no AGPL client build dependency.');
