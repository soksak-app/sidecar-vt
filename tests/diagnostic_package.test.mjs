import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { access, copyFile, mkdir, mkdtemp, readFile, rm } from "node:fs/promises";
import net from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { spawn } from "node:child_process";
import test from "node:test";

const release = process.env.SOKSAK_DIAGNOSTIC_PACKAGE_RELEASE;
const STEP_TIMEOUT_MS = 5000;
const SERVICE_TIMEOUT_MS = 30000;

function within(promise, label, timeoutMs = STEP_TIMEOUT_MS) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${label} timed out after ${timeoutMs} ms`)), timeoutMs);
    }),
  ]).finally(() => clearTimeout(timer));
}

function screenText(body) {
  assert.ok(Array.isArray(body.cells), "screen cells are required");
  return body.cells.map((cell) => cell.grapheme ?? " ".repeat(cell.width)).join("");
}

test("packaged diagnostics binary reports matching render metadata and engine trace", {
  skip: release ? false : "candidate-release supplies SOKSAK_DIAGNOSTIC_PACKAGE_RELEASE",
}, async (t) => {
  const root = await mkdtemp(join(tmpdir(), "vt-diagnostic-package-"));
  const extracted = join(root, "package");
  const serviceDir = join(root, "service");
  const logsDir = join(root, "logs");
  const traceDir = join(root, "trace");
  const assetPath = join(root, "candidate.tar.gz");
  await Promise.all([
    mkdir(extracted, { recursive: true }), mkdir(serviceDir, { recursive: true }),
    mkdir(logsDir, { recursive: true }), mkdir(traceDir, { recursive: true }),
    copyFile(release, assetPath),
  ]);
  execFileSync("tar", ["-xzf", assetPath, "-C", extracted], { stdio: "pipe" });
  const packageInfo = JSON.parse(await readFile(join(extracted, "package.json"), "utf8"));
  assert.equal(packageInfo.name, "@soksak/sidecar-vt-alacritty", "release package identity differs");
  assert.match(packageInfo.version, /^0\.0\.\d+$/, "release package version is invalid");
  const binary = join(extracted, "build", "soksak-vt-alacritty");
  const surface = "s85-diagnostic-surface";
  const project = "/s85-diagnostic-project";
  let service;
  let socket;
  let exited;
  let stderr = "";
  let stdoutBuffer = "";
  const messages = [];
  const waiters = [];
  const images = [];
  const readMessage = (predicate, label) => {
    const find = () => {
      const index = messages.findIndex(predicate);
      return index < 0 ? null : messages.splice(index, 1)[0];
    };
    const current = find();
    if (current) return Promise.resolve(current);
    return within(new Promise((resolve, reject) => {
      const waiter = { predicate, resolve: (value) => resolve(value), reject, label };
      waiters.push(waiter);
      const timer = setTimeout(() => {
        const index = waiters.indexOf(waiter);
        if (index >= 0) waiters.splice(index, 1);
        reject(new Error(`${label} timed out after ${STEP_TIMEOUT_MS} ms`));
      }, STEP_TIMEOUT_MS);
      waiter.resolve = (value) => { clearTimeout(timer); resolve(value); };
    }), label, STEP_TIMEOUT_MS);
  };
  const deliver = (message) => {
    const image = message.body?.image;
    if (message.surface === surface && image?.token) {
      images.push({ generation: image.generation, raster: image.raster, sequence: image.sequence });
      socket.write(`${JSON.stringify({ surface, root: project, body: { image: { consumed: {
        name: image.name, generation: image.generation, raster: image.raster, sequence: image.sequence,
      } } } })}\n`);
    }
    const index = waiters.findIndex((waiter) => waiter.predicate(message));
    if (index < 0) messages.push(message);
    else waiters.splice(index, 1)[0].resolve(message);
  };

  try {
    service = spawn(binary, ["--service-dir", serviceDir, "--log-dir", logsDir], {
      env: { ...process.env, SOKSAK_VT_TRACE_DIR: traceDir },
      stdio: ["ignore", "pipe", "pipe"],
    });
    service.stdout.setEncoding("utf8");
    service.stderr.setEncoding("utf8");
    service.stderr.on("data", (chunk) => { stderr += chunk; });
    exited = new Promise((resolve) => service.once("exit", (code, signal) => resolve({ code, signal })));
    const ready = await within(new Promise((resolve, reject) => {
      const onData = (chunk) => {
        stdoutBuffer += chunk;
        const newline = stdoutBuffer.indexOf("\n");
        if (newline < 0) return;
        service.stdout.off("data", onData);
        try { resolve(JSON.parse(stdoutBuffer.slice(0, newline))); }
        catch (error) { reject(new Error(`service ready line is invalid: ${error.message}`)); }
      };
      service.stdout.on("data", onData);
      service.once("error", reject);
    }), "service endpoint", SERVICE_TIMEOUT_MS);
    const endpoint = JSON.parse(await readFile(join(serviceDir, "endpoint.json"), "utf8"));
    assert.deepEqual(ready, endpoint, "service ready line differs from endpoint.json");
    const socketDirectory = dirname(endpoint.socket);
    socket = net.createConnection(endpoint.socket);
    await within(new Promise((resolve, reject) => {
      socket.once("connect", resolve);
      socket.once("error", reject);
    }), "service socket connect");
    socket.setEncoding("utf8");
    let buffered = "";
    socket.on("data", (chunk) => {
      buffered += chunk;
      while (true) {
        const newline = buffered.indexOf("\n");
        if (newline < 0) break;
        const line = buffered.slice(0, newline);
        buffered = buffered.slice(newline + 1);
        deliver(JSON.parse(line));
      }
    });
    socket.write(`${JSON.stringify({ operation: "hello", protocol: 1, token: endpoint.token, client: "s85-diagnostic-smoke" })}\n`);
    const hello = await readMessage((message) => message.operation === "hello", "hello reply");
    assert.equal(hello.ok, true, "service refused the protocol handshake");
    socket.write(`${JSON.stringify({ surface, root: project, body: { operation: "open", image: "view", shell: "/bin/sh" } })}\n`);
    socket.write(`${JSON.stringify({ surface, root: project, body: { image: { configure: {
      name: "view", generation: 1, raster: 1, width: 640, height: 384, scale: 1,
    } } } })}\n`);
    await readMessage((message) => message.surface === surface && message.body?.event === "state", "initial session state");

    const namesFrame = (frame) => images.some((image) => image.generation === frame?.generation &&
      image.raster === frame?.raster && image.sequence === frame?.sequence);
    const initialScreen = await readMessage((message) => message.surface === surface &&
      message.body?.event === "screen" && namesFrame(message.body.frame),
    "initial drawn screen");
    const frame = initialScreen.body.frame;
    assert.ok(frame && Number.isInteger(frame.sequence), "screen frame identity is required");
    assert.ok(initialScreen.body.render && typeof initialScreen.body.render === "object",
      `diagnostics build omitted matching render metadata (frame ${JSON.stringify(frame)}, version ${packageInfo.version})`);
    assert.deepEqual(initialScreen.body.render.frame, frame, "render metadata names a different frame");
    assert.equal(packageInfo.version, "0.0.17", "candidate release package version differs");
    t.diagnostic(`PASS rendered_frame=${JSON.stringify(frame)}`);

    const payload = "printf 'S85_TRACE\\n'";
    socket.write(`${JSON.stringify({
      surface,
      root: project,
      cids: ["s85-ordered-input"],
      body: {
        operation: "input",
        actions: [{ type: "insert", text: `${payload}\n`, replacementRange: null, attributed: false }],
        preedit: {
          text: "", markedRange: null, selectedRange: null,
          replacementRange: null, attributed: false,
        },
      },
    })}\n`);
    const inputReply = await readMessage((message) => message.cids?.includes("s85-ordered-input"),
      "ordered input transaction reply");
    assert.equal(inputReply.body?.ack, true,
      `packaged binary rejected the current input transaction: ${JSON.stringify(inputReply)}`);
    const outputScreen = await readMessage((message) => message.surface === surface && message.body?.event === "screen" &&
      screenText(message.body).includes("S85_TRACE") && namesFrame(message.body.frame),
    "screen containing controlled PTY output");
    assert.deepEqual(outputScreen.body.render?.frame, outputScreen.body.frame,
      "controlled output screen has no render metadata for its frame");
    t.diagnostic(`PASS output_frame=${JSON.stringify(outputScreen.body.frame)}`);
    socket.destroy();
    socket = null;
    service.kill("SIGTERM");
    const result = await within(exited, "service shutdown", 10000);
    assert.ok(result.code === 0 || result.signal === "SIGTERM",
      `service exited unexpectedly: ${JSON.stringify(result)}; ${stderr}`);
    service = null;
    await assert.rejects(access(socketDirectory), (error) => error.code === "ENOENT",
      `service left its socket directory ${socketDirectory}`);
    const trace = await readFile(join(traceDir, `${surface}.trace`), "utf8");
    const fedBytes = trace.split(/\r?\n/)
      .filter((line) => line.startsWith("feed "))
      .map((line) => Buffer.from(line.slice("feed ".length), "base64"))
      .reduce((all, chunk) => Buffer.concat([all, chunk]), Buffer.alloc(0));
    const expected = Buffer.from(`${payload}\r\n`, "utf8");
    assert.ok(fedBytes.includes(expected),
      `engine trace lacks controlled PTY input ${expected.toString("base64")}: ${trace}`);
    t.diagnostic("PASS engine trace includes controlled PTY input");
  } finally {
    socket?.destroy();
    if (service) {
      service.kill("SIGTERM");
      const result = await within(exited, "service cleanup", 10000).catch(() => null);
      if (!result) {
        service.kill("SIGKILL");
        await within(exited, "service reaping", 10000);
      }
    }
    await rm(root, { recursive: true, force: true });
  }
});
