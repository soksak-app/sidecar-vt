import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const buildScript = await readFile(join(repository, "vt-alacritty/build.rs"));
const cases = [
  {
    name: "a missing sidecar declaration reports its path through the build result",
    declaration: null,
    expected: "sidecar.json: read declaration:",
  },
  {
    name: "invalid sidecar JSON reports its path through the build result",
    declaration: "{",
    expected: "sidecar.json: invalid JSON:",
  },
  {
    name: "a missing display name reports the declaration field through the build result",
    declaration: JSON.stringify({ protocol: 1 }),
    expected: "sidecar.json: displayName must be a non-empty string",
  },
];

for (const contractCase of cases) {
  test(contractCase.name, { timeout: 30000 }, async (t) => {
    const temporary = await mkdtemp(join(tmpdir(), "vt-build-script-contract-"));
    t.after(() => rm(temporary, { recursive: true, force: true }));

    const crate = join(temporary, "crate");
    await mkdir(crate);
    await writeFile(
      join(crate, "Cargo.toml"),
      '[package]\nname = "vt-build-script-contract"\nversion = "0.0.0"\nedition = "2024"\n' +
        '[build-dependencies]\nserde_json = "1"\npkg-config = "0.3"\n',
    );
    await mkdir(join(crate, "src"));
    await writeFile(join(crate, "src/main.rs"), "fn main() {}\n");
    await writeFile(join(crate, "build.rs"), buildScript);
    if (contractCase.declaration !== null) {
      await writeFile(join(crate, "sidecar.json"), contractCase.declaration);
    }

    const result = spawnSync(
      "cargo",
      ["check", "--offline", "--manifest-path", join(crate, "Cargo.toml")],
      {
        cwd: crate,
        encoding: "utf8",
        timeout: 25000,
        env: { ...process.env, CARGO_TARGET_DIR: join(temporary, "target") },
      },
    );
    assert.equal(result.error, undefined, `${result.error ?? ""}\n${result.stderr}`);
    assert.equal(result.status, 101, `${result.stdout}\n${result.stderr}`);
    assert.match(result.stderr, new RegExp(contractCase.expected));
  });
}
