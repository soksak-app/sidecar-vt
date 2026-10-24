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
function recipe(target, variables = []) {
  const env = { ...process.env };
  delete env.MAKEFLAGS;
  delete env.RANGE;
  const result = spawnSync("make", ["-n", ...variables, target], { cwd: root, encoding: "utf8", timeout: 30000, env });
  assert.equal(result.status, 0, `make -n ${target} failed: ${result.stderr}`);
  return result.stdout;
}

test("make push-check runs the Rust tests and the commit message check of the pushed range (S72)", { timeout: 30000 }, () => {
  const commands = recipe("push-check");
  assert.match(commands, /cargo test --workspace/);
  assert.match(commands, /pnpm exec soksak-commits "origin\/main\.\.HEAD"/);
});

test("make build and release pass an explicit diagnostics feature to Cargo (S78)", { timeout: 30000 }, () => {
  const build = recipe("build", ["FEATURES=diagnostics"]);
  const release = recipe("release", ["FEATURES=diagnostics", "OUT=/tmp/vt-s78-dry-run"]);
  assert.match(build, /cargo build\s+--features diagnostics\s+-p soksak-sidecar-vt-alacritty/);
  assert.match(release, /cargo build\s+--features diagnostics\s+-p soksak-sidecar-vt-alacritty/);
  assert.match(release, /sok sidecar release vt-alacritty \/tmp\/vt-s78-dry-run/);
});

test("the pre-push hook runs make push-check on the pushed range (S72)", { timeout: 30000 }, () => {
  const hook = readFileSync(path.join(root, ".githooks", "pre-push"), "utf8");
  assert.match(hook, /make -C "\$root" push-check RANGE="\$base\.\.\$local_sha"/);
});
