import assert from 'node:assert/strict';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const repository = fileURLToPath(new URL('..', import.meta.url));
const installTimeout = 35_000;

test('a clean frozen install provides the Darwin process identity package', {
  skip: process.platform !== 'darwin',
  timeout: 45_000,
}, async () => {
  const fixture = await mkdtemp(path.join(os.tmpdir(), 'soksak-vt-native-install-20261029-'));
  const logPath = path.join(fixture, `install-${process.pid}-${Date.now()}.jsonl`);
  const preserveFixture = process.env.SOKSAK_PRESERVE_NATIVE_INSTALL_FIXTURE === '1';
  let passed = false;

  try {
    await writeFile(path.join(fixture, 'package.json'), await readFile(path.join(repository, 'package.json')));
    const lockfile = execFileSync('git', ['show', 'HEAD:pnpm-lock.yaml'], {
      cwd: repository,
      encoding: 'utf8',
    });
    await writeFile(path.join(fixture, 'pnpm-lock.yaml'), lockfile);
    const workspace = await readFile(path.join(repository, 'pnpm-workspace.yaml'), 'utf8');
    const consumerWorkspace = workspace.replace(/^overrides:\r?\n(?:[ \t]+.*\r?\n)*/m, '');
    await writeFile(path.join(fixture, 'pnpm-workspace.yaml'), consumerWorkspace);

    const install = spawnSync('pnpm', [
      'install',
      '--frozen-lockfile',
      '--reporter=ndjson',
      '--store-dir',
      path.join(fixture, '.pnpm-store'),
    ], {
      cwd: fixture,
      encoding: 'utf8',
      maxBuffer: 8 * 1024 * 1024,
      timeout: installTimeout,
    });
    await writeFile(logPath, `${install.stdout ?? ''}${install.stderr ?? ''}`);
    assert.equal(install.error, undefined, `pnpm install failed to run; log: ${logPath}`);
    assert.equal(install.status, 0, `pnpm install exited ${install.status}; log: ${logPath}`);

    const require = createRequire(path.join(fixture, 'package.json'));
    const packageJson = require.resolve('@soksak/native-process-identity-darwin/package.json');
    const packageRoot = path.dirname(packageJson);
    const manifest = JSON.parse(await readFile(packageJson, 'utf8'));
    assert.equal(manifest.name, '@soksak/native-process-identity-darwin');
    for (const file of [
      'include/soksak_process_identity.h',
      'lib/libsoksak-process-identity.a',
      'lib/pkgconfig/soksak-process-identity.pc',
    ]) {
      const result = await stat(path.join(packageRoot, file));
      assert.ok(result.isFile() && result.size > 0, `installed package has no build file ${file}; log: ${logPath}`);
    }
    passed = true;
  } finally {
    if (passed && !preserveFixture) {
      await rm(fixture, { recursive: true, force: true });
    } else {
      console.error(`Preserved native dependency install fixture and log: ${fixture}`);
    }
  }
});
