// Checks the catalog of the events of the service, vt-alacritty/events.json (core docs/spec/logging.md#catalog): its structure, and
// the emit sites of the sources against it. When SOKSAK_EVENTS names the command of core (a path to events-check.js of a checkout
// of core), the command checks the sites that the emitters of census.json declare; otherwise the names are compared, because the
// @soksak/plugin-api that package.json pins has no soksak-events.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join } from "node:path";
import test from "node:test";

const root = new URL("../", import.meta.url).pathname;
const catalog = JSON.parse(readFileSync(join(root, "vt-alacritty/events.json"), "utf8"));

const LEVELS = ["error", "warn", "info", "debug", "trace"];
const CLASSES = ["lifecycle", "input", "call", "io", "frame"];
const TYPES = ["string", "number", "boolean", "array", "object", "null"];

/** The text of every Rust source under vt-core/src, by path. */
function sources(directory = join(root, "vt-core/src")) {
  const texts = new Map();
  for (const name of readdirSync(directory)) {
    const path = join(directory, name);
    if (statSync(path).isDirectory()) {
      for (const [file, text] of sources(path)) texts.set(file, text);
    } else if (name.endsWith(".rs")) {
      texts.set(path, readFileSync(path, "utf8"));
    }
  }
  return texts;
}

test("the catalog declares the layer sidecar and events of the form sidecar.vt.<name>", () => {
  assert.equal(catalog.schema, 2);
  assert.deepEqual(catalog.layers, ["sidecar"]);
  assert.ok(Object.keys(catalog.events).length > 0);
  for (const [name, event] of Object.entries(catalog.events)) {
    assert.match(name, /^sidecar\.vt\.[a-z_]+(\.[a-z_]+)*$/, name);
    assert.equal(event.layer, "sidecar", name);
    assert.ok(CLASSES.includes(event.class), `${name}: class ${event.class}`);
    for (const level of [event.level].flat()) assert.ok(LEVELS.includes(level), `${name}: level ${level}`);
    assert.equal(typeof event.description, "string", name);
    assert.ok(event.description.length > 0, name);
    for (const [field, type] of Object.entries(event.fields)) {
      // A type such as string|null accepts each of its parts (core docs/spec/logging.md#catalog).
      for (const part of type.split("|")) assert.ok(TYPES.includes(part), `${name}.${field}: type ${type}`);
    }
  }
});

const command = process.env.SOKSAK_EVENTS;

if (command) {
  test("the command of core finds every emit site of the emitters declared and the catalog complete", () => {
    const args = ["--census", "census.json", "--catalog", "vt-alacritty/events.json", "."];
    const run = command.endsWith(".js") ? spawnSync("node", [command, ...args], { cwd: root, encoding: "utf8" })
      : spawnSync(command, args, { cwd: root, encoding: "utf8" });
    assert.equal(run.error, undefined, `${command}: ${run.error}`);
    assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
  });
} else {
test("every declared event is named by a source of vt-core", () => {
  const texts = [...sources().values()].join("\n");
  const missing = Object.keys(catalog.events).filter((name) => !texts.includes(`"${name}"`));
  assert.deepEqual(missing, [], "declared events that no source names");
});

test("every event that a source of vt-core names is declared", () => {
  const found = new Set();
  for (const text of sources().values()) {
    for (const [, name] of text.matchAll(/"(sidecar\.vt\.[a-z_.]*[a-z_])"/g)) found.add(name);
  }
  const undeclared = [...found].filter((name) => catalog.events[name] === undefined);
  assert.deepEqual(undeclared, [], "events that no catalog entry declares");
  assert.ok(found.size > 0, "the sources name no event");
});

test("the manifest of the sidecar names the catalog and the release packs it", () => {
  const manifest = JSON.parse(readFileSync(join(root, "vt-alacritty/sidecar.json"), "utf8"));
  const packed = JSON.parse(readFileSync(join(root, "vt-alacritty/package.json"), "utf8")).files;
  assert.equal(manifest.events, "events.json");
  assert.ok(packed.includes(manifest.events), `package.json files ${JSON.stringify(packed)} lack ${manifest.events}`);
});

test("the manifest declares the replay executable of the role vt and the release packs it", () => {
  const manifest = JSON.parse(readFileSync(join(root, "vt-alacritty/sidecar.json"), "utf8"));
  const packed = JSON.parse(readFileSync(join(root, "vt-alacritty/package.json"), "utf8")).files;
  assert.deepEqual(manifest.replay, { role: "vt", executable: "build/soksak-vt-replay" });
  assert.ok(packed.includes(manifest.replay.executable), `package.json files ${JSON.stringify(packed)} lack ${manifest.replay.executable}`);
});
}
