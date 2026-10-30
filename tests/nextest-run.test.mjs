// The Rust test runner prints a running line before the result of each test (S76-1). nextest prints no start line in its
// human output, so the runner prints the started events of nextest's libtest JSON output.
import test from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatEvent } from "../scripts/nextest-run.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const runner = path.join(root, "scripts", "nextest-run.mjs");
const name = "soksak-sidecar-vt-core::serve_events$request_operation_names_the_kind_of_request";

test("a started event prints a running line and a passed event prints its elapsed time (S76-1)", { timeout: 30000 }, () => {
  assert.equal(formatEvent(JSON.stringify({ type: "test", event: "started", name })), "RUNNING soksak-sidecar-vt-core::serve_events request_operation_names_the_kind_of_request");
  assert.equal(
    formatEvent(JSON.stringify({ type: "test", event: "ok", name, exec_time: 0.01486175 })),
    "PASS [   0.015s] soksak-sidecar-vt-core::serve_events request_operation_names_the_kind_of_request",
  );
});

test("the runner prints the running line of a test before its pass line (S76-1)", { timeout: 240000 }, () => {
  const result = spawnSync(
    process.execPath,
    [runner, "--config-file", ".config/nextest.toml", "-p", "soksak-sidecar-vt-core", "-E", "test(=request_operation_names_the_kind_of_request)"],
    { cwd: root, encoding: "utf8", timeout: 230000 },
  );
  assert.equal(result.status, 0, `runner exited with ${result.status}: ${result.stderr}`);
  const lines = result.stdout.split("\n");
  const running = lines.findIndex((line) => line === "RUNNING soksak-sidecar-vt-core::serve_events request_operation_names_the_kind_of_request");
  const passed = lines.findIndex((line) => /^PASS \[ *[0-9.]+s\] soksak-sidecar-vt-core::serve_events request_operation_names_the_kind_of_request$/.test(line));
  assert.notEqual(running, -1, "no running line for the test");
  assert.notEqual(passed, -1, "no pass line for the test");
  assert.ok(running < passed, "the running line comes after the pass line");
});
