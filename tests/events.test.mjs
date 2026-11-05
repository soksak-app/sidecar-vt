// Checks the catalog of the events of the service, vt-alacritty/events.json (core docs/spec/logging.md#catalog): its structure, and
// the emit sites of the sources against it with soksak-check census of the kit release that package.json pins, which checks the
// sites that the emitters of census.json declare, and the event names that the sources of vt-core write.
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
  assert.equal(catalog.schema, 3);
  assert.deepEqual(catalog.layers, ["sidecar"]);
  assert.ok(Object.keys(catalog.events).length > 0);
  for (const [name, event] of Object.entries(catalog.events)) {
    assert.match(name, /^sidecar\.vt\.[a-z_]+(\.[a-z_]+)*$/, name);
    assert.equal(event.layer, "sidecar", name);
    assert.ok(CLASSES.includes(event.class), `${name}: class ${event.class}`);
    for (const level of [event.level].flat()) assert.ok(LEVELS.includes(level), `${name}: level ${level}`);
    assert.equal(typeof event.description, "string", name);
    assert.ok(event.description.length > 0, name);
    assert.equal(event.fields.$schema, "https://json-schema.org/draft/2020-12/schema", name);
    assert.equal(event.fields.type, "object", name);
    assert.ok(Array.isArray(event.fields.required), name);
    for (const field of event.fields.required) assert.ok(Object.hasOwn(event.fields.properties, field), `${name}.${field}`);
    for (const [field, property] of Object.entries(event.fields.properties)) {
      for (const type of [property.type].flat()) assert.ok(TYPES.includes(type), `${name}.${field}: type ${type}`);
    }
  }
});

const command = join(root, "node_modules/.bin/soksak-check");

test("the census check finds every emit site of the emitters declared and the catalog complete", () => {
  const args = ["census", "--config", "census.json", "--catalog", "vt-alacritty/events.json", "."];
  const run = spawnSync(command, args, { cwd: root, encoding: "utf8" });
  assert.equal(run.error, undefined, `${command}: ${run.error}`);
  assert.equal(run.status, 0, `${run.stdout}${run.stderr}`);
});

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
  assert.equal(manifest.displayName, "soksak://sidecar/vt");
  assert.equal(manifest.events, "events.json");
  assert.ok(packed.includes(manifest.events), `package.json files ${JSON.stringify(packed)} lack ${manifest.events}`);
});

test("the manifest declares the replay executable of the role vt, the events of its inputs, and the release packs it", () => {
  const manifest = JSON.parse(readFileSync(join(root, "vt-alacritty/sidecar.json"), "utf8"));
  const packed = JSON.parse(readFileSync(join(root, "vt-alacritty/package.json"), "utf8")).files;
  assert.deepEqual(manifest.replay, {
    role: "vt",
    executable: "build/soksak-vt-replay",
    // Each kind of replay input names the event that stores it (core docs/spec/sidecars.md, replay).
    inputs: {
      output: "sidecar.vt.pty_read",
      write: "sidecar.vt.pty_write",
      resize: "sidecar.vt.resize",
      selection: "sidecar.vt.selection",
      viewport: "sidecar.vt.viewport",
      theme: "sidecar.vt.theme",
    },
  });
  for (const name of Object.values(manifest.replay.inputs)) assert.ok(catalog.events[name], `the replay input ${name} is not declared in the catalog`);
  assert.ok(packed.includes(manifest.replay.executable), `package.json files ${JSON.stringify(packed)} lack ${manifest.replay.executable}`);
});
