import assert from 'node:assert/strict';
import { readFile, stat } from 'node:fs/promises';
import { createRequire } from 'node:module';
import path from 'node:path';
import test from 'node:test';

const require = createRequire(import.meta.url);

test('the installed Darwin process identity package provides the sidecar build interface', {
  skip: process.platform !== 'darwin',
  timeout: 5000,
}, async () => {
  const packageJson = require.resolve('@soksak/native-process-identity-darwin/package.json');
  const manifest = JSON.parse(await readFile(packageJson, 'utf8'));
  assert.equal(manifest.name, '@soksak/native-process-identity-darwin');
  for (const file of [
    'include/soksak_process_identity.h',
    'lib/libsoksak-process-identity.a',
    'lib/pkgconfig/soksak-process-identity.pc',
  ]) {
    const info = await stat(path.join(path.dirname(packageJson), file));
    assert.ok(info.isFile() && info.size > 0, `installed native package has no build file ${file}`);
  }
});
