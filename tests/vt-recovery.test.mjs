import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { access, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";

const root = new URL("../", import.meta.url).pathname;
const binary = join(root, "vt-alacritty/build/soksak-vt-alacritty");
const verifier = join(root, "scripts/verify-vt-recovery.mjs");

function runVerifier(service = binary) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [verifier, service], {
      cwd: root,
      stdio: ["ignore", "pipe", "pipe"],
    });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.once("error", reject);
    child.once("close", (code, signal) => resolve({ code, signal, stdout, stderr }));
  });
}

test("VT recovery preserves the live service and session across client restart", { timeout: 90000 }, async () => {
  await access(binary);
  const result = await runVerifier();
  assert.equal(result.code, 0, `${result.stdout}\n${result.stderr}`);
  assert.match(result.stdout, /PASS service_alive_after_application_process_exit/);
  assert.match(result.stdout, /PASS application_process_restarted session=/);
  assert.match(result.stdout, /PASS retain_closed_orphan_session=/);
  assert.match(result.stdout, /PASS retained_screen_contains_RECOVERY/);
  assert.match(result.stdout, /PASS recovery_check_duration_ms=\d+/);
});

test("VT recovery reports a service spawn error without waiting for the ready guard", { timeout: 10000 }, async (t) => {
  const fixture = await mkdtemp(join(tmpdir(), "vt-recovery-missing-"));
  t.after(() => rm(fixture, { recursive: true, force: true }));
  const missing = join(fixture, "missing-service");
  const result = await runVerifier(missing);
  assert.notEqual(result.code, 0, "a missing service executable passed the check");
  assert.match(result.stderr, /service unstarted could not start: .*ENOENT/);
  assert.doesNotMatch(result.stderr, /wrote no ready line within 60000ms/);
});

// 명세상 서비스는 기본 글꼴을 읽은 뒤 엔드포인트를 게시하고, 호스트는 시간 제한 없이 준비 줄을 기다린다.
// 첫 CoreText 호출은 몇 초 걸릴 수 있으므로 검사도 준비 줄을 기다려야 한다.
test("VT recovery waits for a ready line that the service writes after several seconds", { timeout: 90000 }, async (t) => {
  const fixture = await mkdtemp(join(tmpdir(), "vt-recovery-slow-"));
  t.after(() => rm(fixture, { recursive: true, force: true }));
  const service = join(fixture, "slow-service");
  // 6초 뒤에 엔드포인트를 게시하는 가짜 서비스다. 소켓은 없으므로 검사는 연결 단계에서 실패해야 한다.
  await writeFile(service, `#!${process.execPath}
const { writeFileSync } = require("node:fs");
const { join } = require("node:path");
const directory = process.argv[3];
setTimeout(() => {
  const endpoint = { protocol: 1, pid: process.pid, socket: join(directory, "missing.sock"), token: "slow" };
  writeFileSync(join(directory, "endpoint.json"), JSON.stringify(endpoint));
  process.stdout.write(JSON.stringify(endpoint) + "\\n");
}, 6000);
process.on("SIGTERM", () => process.exit(0));
setInterval(() => {}, 1000);
`, { mode: 0o755 });
  const result = await runVerifier(service);
  assert.notEqual(result.code, 0, "a service without a socket passed the check");
  assert.doesNotMatch(result.stderr, /endpoint file .* timed out/, "the check stopped waiting before the service was ready");
  assert.match(result.stdout, /PASS service_endpoint_ms=(\d+)/);
  assert.ok(Number(result.stdout.match(/PASS service_endpoint_ms=(\d+)/)[1]) >= 6000, result.stdout);
  assert.match(result.stderr, /worker initial exited .*FAIL connect /, result.stderr);
});
