// The census of core (docs/spec/logging.md#census) applies to this repository: its sources pass with census.json, and a source
// of occurrences that a test adds without an event or a reason fails. The command is soksak-events of the @soksak/plugin-api
// that package.json names; SOKSAK_EVENTS names another command (a path to events-check.js of a checkout of core).
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const root = new URL("../", import.meta.url).pathname;
const command = process.env.SOKSAK_EVENTS ?? join(root, "node_modules/.bin/soksak-events");

/** Runs the census in `directory` with the declaration census.json there and returns the exit status and the output. */
function census(directory) {
  const run = command.endsWith(".js") ? spawnSync("node", [command, "--census", "census.json", "."], { cwd: directory, encoding: "utf8" })
    : spawnSync(command, ["--census", "census.json", "."], { cwd: directory, encoding: "utf8" });
  assert.equal(run.error, undefined, `${command}: ${run.error}`);
  return { status: run.status, output: `${run.stdout}${run.stderr}` };
}

test("the sources of this repository pass the census", () => {
  const { status, output } = census(root);
  assert.equal(status, 0, output);
});

test("a source without an event or a reason fails the census, and a reason makes it pass", () => {
  const directory = mkdtempSync(join(tmpdir(), "vt-census-"));
  try {
    execFileSync("git", ["init", "-q"], { cwd: directory });
    cpSync(join(root, "census.json"), join(directory, "census.json"));
    mkdirSync(join(directory, "src"));
    const source = join(directory, "src/probe.rs");
    writeFileSync(source, "fn count(value: Option<u32>) -> u32 {\n    value.unwrap_or(0)\n}\n");
    execFileSync("git", ["add", "src/probe.rs", "census.json"], { cwd: directory });
    const failed = census(directory);
    assert.equal(failed.status, 1, failed.output);
    assert.match(failed.output, /src\/probe\.rs:2: rs\.unwrap-or has neither an event nor a stated reason/);
    writeFileSync(source, "fn count(value: Option<u32>) -> u32 {\n    // default: an absent count is zero.\n    value.unwrap_or(0)\n}\n");
    const passed = census(directory);
    assert.equal(passed.status, 0, passed.output);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("a reason comment of another form is an error that names the file and the line", () => {
  const directory = mkdtempSync(join(tmpdir(), "vt-census-"));
  try {
    execFileSync("git", ["init", "-q"], { cwd: directory });
    cpSync(join(root, "census.json"), join(directory, "census.json"));
    mkdirSync(join(directory, "src"));
    writeFileSync(join(directory, "src/probe.rs"), "fn count(value: Option<u32>) -> u32 {\n    // 기본값: 없는 개수는 0이다.\n    value.unwrap_or(0)\n}\n");
    execFileSync("git", ["add", "src/probe.rs", "census.json"], { cwd: directory });
    const failed = census(directory);
    assert.equal(failed.status, 1, failed.output);
    assert.match(failed.output, /src\/probe\.rs:2: reason marker/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
