// 터미널 protocol inventory(docs/spec/terminal-protocols.md)를 engine source 와 test 에 대조하는 검사의 test.
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";
import { auditTerminalProtocolInventory } from "../scripts/check-terminal-protocol-inventory.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const node = process.execPath;

function run(command, args) {
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, { cwd: root, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("close", (code, signal) => resolve({ code, signal, stdout, stderr }));
  });
}

test("terminal protocol inventory rejects missing, duplicate, or unlinked CSI rows", { timeout: 5000 }, async () => {
  const result = await run(node, [join(root, "scripts/check-terminal-protocol-inventory.mjs")]);
  assert.equal(result.code, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, /PASS terminal protocol inventory: \d+ unique CSI rows and \d+ unique OSC rows with named tests/);
});

test("terminal protocol inventory reproduces missing and duplicate CSI rows as Red", { timeout: 1000 }, async () => {
  const source = await readFile(join(root, "vt-alacritty/src/engine.rs"), "utf8");
  const tests = await readFile(join(root, "vt-alacritty/tests/engine_test.rs"), "utf8");
  const broken = auditTerminalProtocolInventory({
    engineSource: source.replace('selector: "E/F"', 'selector: "A/B/C/D/G/H/f/s/u"'),
    testSource: tests,
  });
  assert.ok(broken.errors.some((error) => error.includes("duplicate CSI selector row: A/B/C/D/G/H/f/s/u")));
  assert.ok(broken.errors.some((error) => error.includes("required CSI inventory row is missing: E/F")));
  const brokenOsc = auditTerminalProtocolInventory({
    engineSource: source.replace('selector: "0,2"', 'selector: "4"'),
    testSource: tests,
  });
  assert.ok(brokenOsc.errors.some((error) => error.includes("duplicate OSC selector row: 4")));
  assert.ok(brokenOsc.errors.some((error) => error.includes("required OSC inventory row is missing: 0,2")));
});

test("the OSC report in the specification matches the engine inventory selector by selector", { timeout: 5000 }, async () => {
  const specification = await readFile(join(root, "docs/spec/terminal-protocols.md"), "utf8");
  assert.deepEqual(auditTerminalProtocolInventory().errors, []);
  const missing = auditTerminalProtocolInventory({
    specSource: specification.replace(/^\| `46` \|.*\n/m, ""),
  });
  assert.ok(missing.errors.includes("OSC selector 46 has no row in the specification report"), missing.errors.join("\n"));
  const outcome = auditTerminalProtocolInventory({
    specSource: specification.replace("| `4` | Indexed color set/query | `implemented`", "| `4` | Indexed color set/query | `unsupported`"),
  });
  assert.ok(outcome.errors.includes("OSC selector 4 is implemented in the engine but unsupported in the specification report"),
    outcome.errors.join("\n"));
  const evidence = auditTerminalProtocolInventory({
    specSource: specification.replace("`osc104_resets_indexed_colors`", "`osc104_test_that_does_not_exist`"),
  });
  assert.ok(evidence.errors.includes("OSC selector 104 names a test that does not exist: osc104_test_that_does_not_exist"),
    evidence.errors.join("\n"));
});

