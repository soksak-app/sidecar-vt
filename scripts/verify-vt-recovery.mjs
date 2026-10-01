import { mkdir, readFile, rm } from "node:fs/promises";
import { mkdtemp } from "node:fs/promises";
import net from "node:net";
import { loadavg, tmpdir } from "node:os";
import { spawn } from "node:child_process";
import { join } from "node:path";
import { performance } from "node:perf_hooks";

const STEP_TIMEOUT_MS = 5000;

const fail = (message) => {
  throw new Error(message);
};

const withTimeout = async (promise, label) => {
  let timer;
  try {
    return await Promise.race([
      promise,
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} timed out after ${STEP_TIMEOUT_MS}ms`)), STEP_TIMEOUT_MS);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
};

// 서비스는 기본 글꼴을 읽은 뒤 엔드포인트를 게시하고 준비 줄을 쓴다. 호스트처럼 준비 줄을 기다린다. 첫 CoreText
// 호출은 몇 초 걸릴 수 있으므로 한도는 멈춤만 막는다(docs/spec/terminal-runtime.md).
const READY_GUARD_MS = 60000;

const readyLine = (service) => new Promise((resolve, reject) => {
  let buffered = "";
  const finish = (error, value) => {
    clearTimeout(timer);
    service.stdout.off("data", onData);
    service.off("exit", onExit);
    service.off("error", onError);
    if (error) reject(error);
    else resolve(value);
  };
  const onData = (chunk) => {
    buffered += chunk;
    const newline = buffered.indexOf("\n");
    if (newline >= 0) finish(null, buffered.slice(0, newline));
  };
  const onExit = (code, signal) => finish(new Error(`service ${service.pid} exited before its ready line ` +
    `(exit ${code ?? "none"}, signal ${signal ?? "none"}, stdout ${JSON.stringify(buffered)})`));
  const onError = (error) => finish(new Error(`service ${service.pid ?? "unstarted"} could not start: ${error.message}`));
  // 실패를 해석하도록 서비스가 살아 있는지와 부하를 적는다. 서비스 stderr 는 호출자가 붙인다.
  const timer = setTimeout(() => finish(new Error(`service ${service.pid} wrote no ready line within ${READY_GUARD_MS}ms ` +
    `(exit ${service.exitCode ?? "none"}, signal ${service.signalCode ?? "none"}, stdout ${JSON.stringify(buffered)}, ` +
    `load average ${loadavg().map((value) => value.toFixed(1)).join(" ")})`)), READY_GUARD_MS);
  service.stdout.setEncoding("utf8");
  service.stdout.on("data", onData);
  service.once("exit", onExit);
  service.once("error", onError);
});

// root 가 있으면 호스트처럼 표시 요청마다 래스터를 받았다고 consumed 로 답한다. 사이드카는 답을 받기 전에 바뀐
// 화면을 그리거나 보내지 않는다(docs/spec/terminal-runtime.md).
const lineClient = async (endpoint, clientName, { root = null } = {}) => {
  const socket = net.createConnection(endpoint.socket);
  await withTimeout(new Promise((resolve, reject) => {
    socket.once("connect", resolve);
    socket.once("error", reject);
  }), "socket connect");
  socket.setEncoding("utf8");

  let buffered = "";
  const queued = [];
  const waiters = [];
  let socketError;
  socket.on("data", (chunk) => {
    buffered += chunk;
    while (true) {
      const newline = buffered.indexOf("\n");
      if (newline < 0) break;
      const line = buffered.slice(0, newline);
      buffered = buffered.slice(newline + 1);
      const value = JSON.parse(line);
      const image = value.body?.image;
      if (root && value.surface && image?.token) {
        socket.write(`${JSON.stringify({ surface: value.surface, root, body: { image: { consumed: {
          name: image.name, generation: image.generation, raster: image.raster, sequence: image.sequence,
        } } } })}\n`);
      }
      const waiter = waiters.shift();
      if (waiter) waiter.resolve(value);
      else queued.push(value);
    }
  });
  socket.on("error", (error) => {
    socketError = error;
    while (waiters.length) waiters.shift().reject(error);
  });
  socket.on("close", () => {
    const error = socketError ?? new Error("sidecar socket closed");
    while (waiters.length) waiters.shift().reject(error);
  });

  const next = () => {
    if (queued.length) return Promise.resolve(queued.shift());
    return new Promise((resolve, reject) => waiters.push({ resolve, reject }));
  };
  const write = (value) => socket.write(`${JSON.stringify(value)}\n`);
  write({ operation: "hello", protocol: 1, token: endpoint.token, client: clientName });
  const hello = await withTimeout(next(), "hello response");
  if (hello.operation !== "hello" || hello.ok !== true || hello.protocol !== 1) {
    fail(`invalid hello response: ${JSON.stringify(hello)}`);
  }
  return { socket, next, write };
};

const screenText = (value) => (value.body?.lines ?? [])
  .map((row) => row.map((cell) => cell.ch).join(""))
  .join("\n");

const waitFor = async (client, predicate, label) => {
  const started = performance.now();
  while (performance.now() - started < STEP_TIMEOUT_MS) {
    const value = await withTimeout(client.next(), label);
    if (predicate(value)) return value;
  }
  fail(`${label} timed out after ${STEP_TIMEOUT_MS}ms`);
};

const sendSurface = (client, surface, root, body) => {
  client.write({ surface, root, body });
};

const assertServiceAlive = (pid) => {
  try {
    process.kill(pid, 0);
  } catch (error) {
    fail(`service ${pid} is not alive after application process exit: ${error.message}`);
  }
};

const closeSocket = async (socket, label) => {
  socket.destroy();
  await withTimeout(new Promise((resolve) => socket.once("close", resolve)), label);
};

const worker = async () => {
  if (process.argv.length !== 8) {
    fail("usage: node scripts/verify-vt-recovery.mjs --worker <endpoint> <service-dir> <initial|restore> <session|-> <orphan-session|->");
  }
  const endpointPath = process.argv[3];
  const serviceDirectory = process.argv[4];
  const mode = process.argv[5];
  const expectedSession = process.argv[6];
  const expectedOrphan = process.argv[7];
  if (mode !== "initial" && mode !== "restore") fail(`unknown worker mode: ${mode}`);
  if (mode === "restore" && (expectedSession === "-" || expectedOrphan === "-")) fail("restore worker requires both session IDs");

  const endpoint = JSON.parse(await readFile(endpointPath, "utf8"));
  const surface = "recovery-surface";
  // orphan 표면은 닫힘 알림 없이 애플리케이션과 함께 사라진 표면이다. 재시작한 애플리케이션의 레이아웃에 없다.
  const orphan = "orphan-surface";
  const project = "/recovery/project";
  const client = await lineClient(endpoint, serviceDirectory, { root: project });
  const openSession = async (name, label) => {
    sendSurface(client, name, project, { operation: "open", image: "terminal", shell: "/bin/sh" });
    sendSurface(client, name, project, { image: { configure: {
      name: "terminal", generation: 1, raster: 1, width: 640, height: 384, scale: 1,
    } } });
    const state = await waitFor(client, (value) => value.surface === name && value.body?.event === "state", label);
    if (!state.body.sessionId) fail(`${label} did not contain a session ID`);
    return state.body.sessionId;
  };
  try {
    if (mode === "initial") {
      const sessionId = await openSession(surface, "worker initial state");
      const orphanSession = await openSession(orphan, "worker orphan state");
      console.log(`WORKER_ORPHAN_SESSION_ID=${orphanSession}`);
      sendSurface(client, surface, project, {
        operation: "input",
        bytes: Buffer.from("echo RECOVERY\n").toString("base64"),
      });
      await waitFor(client, (value) => value.surface === surface && value.body?.event === "screen" && screenText(value).includes("RECOVERY"), "worker initial output");
      console.log(`WORKER_SESSION_ID=${sessionId}`);
      return;
    }

    // 재시작한 애플리케이션은 표면을 보내기 전에 레이아웃의 표면만 남기도록 retain 을 보낸다. 닫힌 수 1 은
    // 고아 세션이 재시작 뒤에도 서비스에 남아 있었다는 측정이다.
    client.write({ operation: "retain", request: "orphan-retain", surfaces: [{ surface, root: project }] });
    const retained = await waitFor(client, (value) => value.operation === "retained" && value.request === "orphan-retain", "worker retain reply");
    console.log(`WORKER_RETAIN_REPLY=${JSON.stringify(retained)}`);
    if (retained.ok !== true || retained.closed !== 1) fail(`retain did not close exactly the orphan session: ${JSON.stringify(retained)}`);
    const reopened = await openSession(orphan, "worker orphan reopen state");
    if (reopened === expectedOrphan) fail(`the orphan session ${expectedOrphan} survived retain`);
    console.log(`WORKER_ORPHAN_CLOSED_SESSION_ID=${expectedOrphan}`);
    sendSurface(client, orphan, project, { operation: "close" });

    sendSurface(client, surface, project, { operation: "open", image: "terminal", shell: "/bin/sh" });
    sendSurface(client, surface, project, { operation: "screen.read" });
    const reconnected = await waitFor(client, (value) => value.surface === surface && value.body?.event === "session", "worker session reattach");
    if (reconnected.body.sessionId !== expectedSession) {
      fail(`worker session changed from ${expectedSession} to ${reconnected.body.sessionId}`);
    }
    await waitFor(client, (value) => value.surface === surface && value.body?.event === "screen" && screenText(value).includes("RECOVERY"), "worker retained output");
    console.log(`WORKER_RESTORED_SESSION_ID=${expectedSession}`);
    sendSurface(client, surface, project, { operation: "close" });
  } finally {
    await closeSocket(client.socket, `worker ${mode} socket close`);
  }
};

const runWorker = async (endpointPath, serviceDirectory, mode, sessionId = "-", orphanSession = "-") => {
  const child = spawn(process.execPath, [process.argv[1], "--worker", endpointPath, serviceDirectory, mode, sessionId, orphanSession], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  let stdout = "";
  let stderr = "";
  child.stdout.setEncoding("utf8");
  child.stderr.setEncoding("utf8");
  child.stdout.on("data", (chunk) => { stdout += chunk; });
  child.stderr.on("data", (chunk) => { stderr += chunk; });
  const exited = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("exit", (code, signal) => resolve({ code, signal }));
  });
  let result;
  try {
    result = await withTimeout(exited, `worker ${mode} process`);
  } catch (error) {
    // 멈춘 단계를 찾도록 작업자가 남긴 출력을 붙이고 작업자를 끝낸다.
    child.kill("SIGKILL");
    await exited;
    fail(`${error.message}; worker stdout: ${stdout.trim() || "none"}; worker stderr: ${stderr.trim() || "none"}`);
  }
  if (result.code !== 0) {
    fail(`worker ${mode} exited with code=${result.code} signal=${result.signal}: ${stderr.trim()}`);
  }
  return stdout;
};

const main = async () => {
  const binary = process.argv[2];
  if (!binary || process.argv.length !== 3) {
    fail("usage: node scripts/verify-vt-recovery.mjs <sidecar-binary>");
  }

  const started = performance.now();
  const root = await mkdtemp(join(tmpdir(), "soksak-vt-recovery-"));
  const serviceDirectory = join(root, "services", "vt-alacritty");
  await mkdir(serviceDirectory, { recursive: true, mode: 0o700 });
  const service = spawn(binary, ["--service-dir", serviceDirectory], {
    stdio: ["ignore", "pipe", "pipe"],
  });
  let serviceStderr = "";
  service.stderr.setEncoding("utf8");
  service.stderr.on("data", (chunk) => { serviceStderr += chunk; });

  const endpointPath = join(serviceDirectory, "endpoint.json");
  try {
    const ready = await readyLine(service);
    console.log(`PASS service_endpoint_ms=${Math.round(performance.now() - started)}`);
    const endpoint = JSON.parse(ready);
    const published = await readFile(endpointPath, "utf8");
    if (JSON.stringify(JSON.parse(published)) !== JSON.stringify(endpoint)) {
      fail(`endpoint.json ${published.trim()} differs from the ready line ${ready}`);
    }
    if (endpoint.protocol !== 1 || !endpoint.socket || !endpoint.token || endpoint.pid !== service.pid) {
      fail(`invalid endpoint: ${JSON.stringify(endpoint)}`);
    }
    const initial = await runWorker(endpointPath, serviceDirectory, "initial");
    const sessionMatch = initial.match(/^WORKER_SESSION_ID=([^\n]+)$/m);
    if (!sessionMatch) fail(`initial worker did not report a session ID: ${initial.trim()}`);
    const sessionId = sessionMatch[1];
    const orphanMatch = initial.match(/^WORKER_ORPHAN_SESSION_ID=([^\n]+)$/m);
    if (!orphanMatch) fail(`initial worker did not report the orphan session ID: ${initial.trim()}`);
    assertServiceAlive(service.pid);
    console.log(`PASS service_alive_after_application_process_exit pid=${service.pid}`);
    const restored = await runWorker(endpointPath, serviceDirectory, "restore", sessionId, orphanMatch[1]);
    if (!restored.match(new RegExp(`^WORKER_RESTORED_SESSION_ID=${sessionId.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`, "m"))) {
      fail(`restore worker did not report the expected session: ${restored.trim()}`);
    }
    console.log(`PASS application_process_restarted session=${sessionId}`);
    if (!restored.includes(`WORKER_ORPHAN_CLOSED_SESSION_ID=${orphanMatch[1]}`)) {
      fail(`restore worker did not report the retain of the orphan session: ${restored.trim()}`);
    }
    console.log(`PASS retain_closed_orphan_session=${orphanMatch[1]}`);
    console.log("PASS retained_screen_contains_RECOVERY");
  } catch (error) {
    const detail = serviceStderr.trim();
    throw new Error(detail ? `${error.message}; service stderr: ${detail}` : error.message);
  } finally {
    if (!service.killed) service.kill("SIGTERM");
    await withTimeout(new Promise((resolve) => {
      if (service.exitCode !== null || service.signalCode !== null) resolve();
      else service.once("close", resolve);
    }), "service cleanup");
    await rm(root, { recursive: true, force: false });
  }
  console.log(`PASS recovery_check_duration_ms=${Math.round(performance.now() - started)}`);
};

try {
  if (process.argv[2] === "--worker") await worker();
  else await main();
} catch (error) {
  console.error(`FAIL ${error.message}`);
  process.exitCode = 1;
}
