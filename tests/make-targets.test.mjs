// The check-push target runs the checks that a push needs, and the pre-push hook runs check-push on the pushed range (S72).
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { homedir } from "node:os";
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
  delete env.CARGO_TARGET_DIR;
  const result = spawnSync("make", ["-n", "--no-print-directory", ...variables, target], { cwd: root, encoding: "utf8", timeout: 30000, env });
  assert.equal(result.status, 0, `make -n ${target} failed: ${result.stderr}`);
  return result.stdout;
}

// A published commit is kept, so the push checks its message and the checklist history it records; CI runs the tests on
// the pushed commits.
test("make check-push runs the commit message check and the checklist history check of the pushed range (S154)", { timeout: 30000 }, () => {
  const commands = recipe("check-push");
  assert.deepEqual(commands.trim().split("\n"), [
    'pnpm exec sok-check commits "origin/main..HEAD"',
    'pnpm exec sok-check docs --range "origin/main..HEAD"',
  ]);
});

test("make build and release pass an explicit diagnostics feature to Cargo (S78)", { timeout: 30000 }, () => {
  const build = recipe("build", ["FEATURES=diagnostics"]);
  const release = recipe("release", ["FEATURES=diagnostics", "OUT=/tmp/vt-s78-dry-run"]);
  assert.match(build, /cargo build\s+--features diagnostics\s+-p soksak-sidecar-vt-alacritty/);
  assert.match(release, /cargo build\s+--features diagnostics\s+-p soksak-sidecar-vt-alacritty/);
  assert.match(release, /npm pack --silent --pack-destination \/tmp\/vt-s78-dry-run \.\/vt-alacritty/);
});

// Cargo writes the executables where CARGO_TARGET_DIR names, so the copy reads the same folder (S159).
test("make build copies the executables from the target directory of Cargo (S159)", { timeout: 30000 }, () => {
  const named = recipe("build", ["CARGO_TARGET_DIR=/tmp/sidecar-vt-S159-dry-run"]);
  assert.match(named, /^cp \/tmp\/sidecar-vt-S159-dry-run\/debug\/sok-vt-alacritty vt-alacritty\/build\/sok-vt-alacritty$/m);
  assert.match(named, /^cp \/tmp\/sidecar-vt-S159-dry-run\/debug\/sok-vt-replay vt-alacritty\/build\/sok-vt-replay$/m);
  const unnamed = recipe("build");
  assert.match(unnamed, /^cp target\/debug\/sok-vt-alacritty vt-alacritty\/build\/sok-vt-alacritty$/m);
  assert.match(unnamed, /^cp target\/debug\/sok-vt-replay vt-alacritty\/build\/sok-vt-replay$/m);
});

test("the pre-push hook runs make check-push on the pushed range (S72)", { timeout: 30000 }, () => {
  const hook = readFileSync(path.join(root, ".githooks", "pre-push"), "utf8");
  assert.match(hook, /make -C "\$root" check-push RANGE="\$base\.\.\$local_sha"/);
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

// cc compiles the Objective-C frame of the core for MACOSX_DEPLOYMENT_TARGET and rustc links for it. Without it cc compiles
// for the version of the SDK and rustc links for its own default, so every command that runs Cargo, and the tests that
// start Cargo, give it MACOS_MINIMUM, the one value of the Makefile (S163).
test("each command of build, test and repeat that runs Cargo gives it MACOS_MINIMUM (S163)", { timeout: 30000 }, () => {
  const definitions = readFileSync(path.join(root, "Makefile"), "utf8").split("\n").filter((line) => /^MACOS_MINIMUM\s*=/.test(line));
  assert.equal(definitions.length, 1, "the Makefile must define MACOS_MINIMUM once");
  const minimum = definitions[0].replace(/^MACOS_MINIMUM\s*=\s*/, "").trim();
  assert.match(minimum, /^\d+\.\d+$/, `MACOS_MINIMUM is ${JSON.stringify(minimum)}`);
  const target = new RegExp(`(?:^|\\s)MACOSX_DEPLOYMENT_TARGET=${minimum.replace(".", "\\.")}\\s`);
  const recipes = [recipe("build"), recipe("test"), recipe("repeat", ["PACKAGE=soksak-sidecar-vt-core", "COUNT=1"])];
  const commands = recipes.flatMap((lines) => lines.split("\n")).filter((line) => /\bcargo (?:build|clippy|test)\b|nextest-run\.mjs|node --test tests\//.test(line));
  assert.ok(commands.some((line) => line.includes("node --test tests/")), "make test runs no node test");
  for (const command of commands) {
    assert.match(command, target, `the command gives Cargo no MACOSX_DEPLOYMENT_TARGET=${minimum}: ${command}`);
  }
});

// rustc reports a linked object that was built for a newer macOS than the link under the lint linker_info, which is
// allowed by default, so the link prints nothing and passes. Every member of the workspace denies it, so such a link fails
// each build and test of the member (S163).
test("every member of the workspace denies linker_info (S163)", { timeout: 30000 }, () => {
  const workspace = readFileSync(path.join(root, "Cargo.toml"), "utf8");
  const lints = workspace.match(/^\[workspace\.lints\.rust\]\n((?:[^[\n].*\n?)*)/m);
  assert.ok(lints, "Cargo.toml declares no [workspace.lints.rust]");
  assert.match(lints[1], /^linker_info = "deny"$/m, "the workspace does not deny linker_info");
  const members = JSON.parse(workspace.match(/^members = (\[.*\])$/m)[1]);
  assert.notEqual(members.length, 0);
  for (const member of members) {
    const manifest = readFileSync(path.join(root, member, "Cargo.toml"), "utf8");
    assert.match(manifest, /^\[lints\]\nworkspace = true$/m, `${member}/Cargo.toml does not take the lints of the workspace`);
  }
});

// rustup installs cargo in ~/.cargo/bin and adds that folder to PATH in the shell profile, which the shell of make does not
// read, so the Makefile puts the folder on PATH. A recipe run with only the system folders on PATH finds that cargo (S164).
test("the shell of make finds the cargo of rustup without the PATH of the profile (S164)", { timeout: 30000 }, () => {
  const cargo = path.join(homedir(), ".cargo", "bin", "cargo");
  assert.ok(existsSync(cargo), `rustup installs no cargo at ${cargo}`);
  // The probe target comes from standard input after the Makefile, so the recipe runs with the PATH that the Makefile exports.
  const result = spawnSync("/usr/bin/make", ["-s", "--no-print-directory", "-f", "Makefile", "-f", "-", "find-cargo"], {
    cwd: root,
    input: "find-cargo: ; @command -v cargo\n",
    encoding: "utf8",
    timeout: 25000,
    env: { HOME: homedir(), PATH: "/usr/bin:/bin" },
  });
  assert.equal(result.status, 0, `the shell of make finds no cargo: ${result.stdout}${result.stderr}`);
  assert.equal(result.stdout.trim(), cargo);
});
