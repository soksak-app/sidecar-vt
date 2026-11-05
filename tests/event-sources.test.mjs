// The event source check of core (docs/spec/logging.md#event-sources) applies to this repository: its sources pass with event-sources.json, and a source
// of occurrences that a test adds without an event or a reason fails. The command is soksak-check events of the kit release
// that package.json pins.
import assert from "node:assert/strict";
import { execFileSync, spawnSync } from "node:child_process";
import { cpSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

const root = new URL("../", import.meta.url).pathname;
const command = join(root, "node_modules/.bin/soksak-check");

/** Runs the event source check in `directory` with the declaration event-sources.json there and returns the exit status and the output. */
function eventSources(directory) {
  const run = spawnSync(command, ["events", "--config", "event-sources.json", "."], { cwd: directory, encoding: "utf8" });
  assert.equal(run.error, undefined, `${command}: ${run.error}`);
  return { status: run.status, output: `${run.stdout}${run.stderr}` };
}

test("the sources of this repository pass the event source check", () => {
  const { status, output } = eventSources(root);
  assert.equal(status, 0, output);
});

test("a source without an event or a reason fails the event source check, and a reason makes it pass", () => {
  const directory = mkdtempSync(join(tmpdir(), "vt-event-sources-"));
  try {
    execFileSync("git", ["init", "-q"], { cwd: directory });
    cpSync(join(root, "event-sources.json"), join(directory, "event-sources.json"));
    mkdirSync(join(directory, "src"));
    const source = join(directory, "src/probe.rs");
    writeFileSync(source, "fn count(value: Option<u32>) -> u32 {\n    value.unwrap_or(0)\n}\n");
    execFileSync("git", ["add", "src/probe.rs", "event-sources.json"], { cwd: directory });
    const failed = eventSources(directory);
    assert.equal(failed.status, 1, failed.output);
    assert.match(failed.output, /src\/probe\.rs:2: rs\.unwrap-or has neither an event nor a stated reason/);
    writeFileSync(source, "fn count(value: Option<u32>) -> u32 {\n    // default: an absent count is zero.\n    value.unwrap_or(0)\n}\n");
    const passed = eventSources(directory);
    assert.equal(passed.status, 0, passed.output);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("a reason comment of another form is an error that names the file and the line", () => {
  const directory = mkdtempSync(join(tmpdir(), "vt-event-sources-"));
  try {
    execFileSync("git", ["init", "-q"], { cwd: directory });
    cpSync(join(root, "event-sources.json"), join(directory, "event-sources.json"));
    mkdirSync(join(directory, "src"));
    writeFileSync(join(directory, "src/probe.rs"), "fn count(value: Option<u32>) -> u32 {\n    // 기본값: 없는 개수는 0이다.\n    value.unwrap_or(0)\n}\n");
    execFileSync("git", ["add", "src/probe.rs", "event-sources.json"], { cwd: directory });
    const failed = eventSources(directory);
    assert.equal(failed.status, 1, failed.output);
    assert.match(failed.output, /src\/probe\.rs:2: reason marker/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});
