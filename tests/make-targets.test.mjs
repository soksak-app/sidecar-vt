// The push-check target runs the checks that a push needs, and the pre-push hook runs push-check on the pushed range (S72).
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

// Prints the recipe of a target without running it; the recipe lines name every command the target runs.
// MAKEFLAGS and RANGE come from an outer make (the pre-push hook passes RANGE), so they are removed to print the default range.
// A make run inside another make prints its directory, so the recipe is printed without it.
function recipe(target, variables = []) {
  const env = { ...process.env };
  delete env.MAKEFLAGS;
  delete env.RANGE;
  const result = spawnSync("make", ["-n", "--no-print-directory", ...variables, target], { cwd: root, encoding: "utf8", timeout: 30000, env });
  assert.equal(result.status, 0, `make -n ${target} failed: ${result.stderr}`);
  return result.stdout;
}

// A published commit message is kept, so the push checks it; CI runs the tests on the pushed commits.
test("make push-check runs only the commit message check of the pushed range (F274-vt-sidecar)", { timeout: 30000 }, () => {
  const commands = recipe("push-check");
  assert.deepEqual(commands.trim().split("\n"), ['pnpm exec soksak-check commits "origin/main..HEAD"']);
});

test("make build and release pass an explicit diagnostics feature to Cargo (S78)", { timeout: 30000 }, () => {
  const build = recipe("build", ["FEATURES=diagnostics"]);
  const release = recipe("release", ["FEATURES=diagnostics", "OUT=/tmp/vt-s78-dry-run"]);
  assert.match(build, /cargo build\s+--features diagnostics\s+-p soksak-sidecar-vt-alacritty/);
  assert.match(release, /cargo build\s+--features diagnostics\s+-p soksak-sidecar-vt-alacritty/);
  assert.match(release, /npm pack --silent --pack-destination \/tmp\/vt-s78-dry-run \.\/vt-alacritty/);
});

test("the pre-push hook runs make push-check on the pushed range (S72)", { timeout: 30000 }, () => {
  const hook = readFileSync(path.join(root, ".githooks", "pre-push"), "utf8");
  assert.match(hook, /make -C "\$root" push-check RANGE="\$base\.\.\$local_sha"/);
});

// Each Rust test reports its own result, elapsed time and time limit through cargo-nextest with the declared config (S76).
test("make test runs each Rust test through cargo-nextest with the declared config (S76)", { timeout: 30000 }, () => {
  const commands = recipe("test");
  const harnessLines = commands.split("\n").filter((line) => /cargo test\b/.test(line));
  assert.deepEqual(harnessLines.filter((line) => !/--doc\b/.test(line)), [], "make test runs a Rust test through the default harness");
  assert.match(commands, /node scripts\/nextest-run\.mjs --config-file \.config\/nextest\.toml[^\n]*--workspace/);
  const config = readFileSync(path.join(root, ".config", "nextest.toml"), "utf8");
  assert.match(config, /terminate-after\s*=/, "the nextest config declares no per-test termination limit");
});

// cargo-nextest does not run doc tests, and the doc-test output of cargo has no per-test time or limit, so make test runs no
// doc test: the usage examples of the Rust sources are unit tests that run through cargo-nextest (S76-2).
test("make test runs no doc test, so every Rust test has a nextest time and limit (S76-2)", { timeout: 30000 }, () => {
  const commands = recipe("test");
  assert.deepEqual(commands.split("\n").filter((line) => /cargo test\b.*--doc\b/.test(line)), [], "make test runs a doc test");
});

// The installation stage is make install (core docs/spec/plugins.md#repositories); the build and the tests consume what it installed.
test("make install installs the locked dependencies, and no other target installs (F275-vt-sidecar)", { timeout: 30000 }, () => {
  assert.deepEqual(recipe("install").trim().split("\n"), ["pnpm install --frozen-lockfile"]);
  const makefile = readFileSync(path.join(root, "Makefile"), "utf8");
  assert.deepEqual(makefile.split("\n").filter((line) => /^[^\t#][^:=]*:(?!=).*\bnode_modules\b/.test(line)), [], "a target depends on node_modules");
});
