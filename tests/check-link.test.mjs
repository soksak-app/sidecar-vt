// scripts/check-link.mjs fails a command whose linker reports an object built for a newer macOS than the link. rustc prints
// that line under the lint linker_info, beside other linker_info lines such as the duplicate `-lc` that the standard library
// and the libc crate both pass to the linker; this repository cannot remove that one, so the check passes it (S163).
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { builtForNewer } from "../scripts/check-link.mjs";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const newer = "warning: ld: warning: object file (/tmp/frame.o) was built for newer 'macOS' version (27.0) than being linked (14.4)";
const duplicate = "warning: ld: warning: ignoring duplicate libraries: '-lc'";

// Runs the check on a node program that writes to its outputs and ends with the given status.
function check(program) {
  return spawnSync("node", ["scripts/check-link.mjs", process.execPath, "-e", program], { cwd: root, encoding: "utf8", timeout: 10000 });
}

test("the judgement names an object built for a newer macOS and passes duplicate libraries", { timeout: 5000 }, () => {
  assert.equal(builtForNewer(newer), true);
  assert.equal(builtForNewer("ld: warning: object file (/tmp/go-link-1/000002.o) was built for newer 'macOS' version (27.0) than being linked (14.4)"), true);
  assert.equal(builtForNewer(duplicate), false);
  assert.equal(builtForNewer("   Compiling soksak-sidecar-vt-core v0.0.0"), false);
});

test("a linker line of a newer object fails a command that succeeds, and the line stays in the output", { timeout: 15000 }, () => {
  const result = check(`console.log("out"); console.error(${JSON.stringify(newer)});`);
  assert.equal(result.status, 1, `${result.stdout}\n${result.stderr}`);
  assert.equal(result.stdout, "out\n");
  assert.ok(result.stderr.startsWith(`${newer}\n`), result.stderr);
  assert.match(result.stderr, /check-link: the linker linked 1 object built for a newer macOS than the executable declares/);
});

test("a duplicate library line passes, and a command keeps its output and its status", { timeout: 15000 }, () => {
  const passed = check(`console.log("ok"); console.error(${JSON.stringify(duplicate)});`);
  assert.equal(passed.status, 0, passed.stderr);
  assert.equal(passed.stdout, "ok\n");
  assert.equal(passed.stderr, `${duplicate}\n`);
  const failed = check(`console.error("FAIL"); process.exit(3);`);
  assert.equal(failed.status, 3, failed.stderr);
  assert.equal(failed.stderr, "FAIL\n");
});

test("a command that fails with a newer object fails with its own status", { timeout: 15000 }, () => {
  const result = check(`console.error(${JSON.stringify(newer)}); process.exit(2);`);
  assert.equal(result.status, 2, result.stderr);
  assert.match(result.stderr, /check-link: the linker linked 1 object built for a newer macOS/);
});
