// Checks the catalog of the events of the service, vt-alacritty/events.json (core docs/spec/logging.md#catalog): its structure, that
// every declared event is named by a source of vt-core, and that every event a source names is declared.
import assert from "node:assert/strict";
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
      assert.ok(TYPES.includes(type), `${name}.${field}: type ${type}`);
    }
  }
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
  assert.equal(manifest.events, "events.json");
  assert.ok(packed.includes(manifest.events), `package.json files ${JSON.stringify(packed)} lack ${manifest.events}`);
});

test("the manifest declares the replay executable of the role vt and the release packs it", () => {
  const manifest = JSON.parse(readFileSync(join(root, "vt-alacritty/sidecar.json"), "utf8"));
  const packed = JSON.parse(readFileSync(join(root, "vt-alacritty/package.json"), "utf8")).files;
  assert.deepEqual(manifest.replay, { role: "vt", executable: "build/soksak-vt-replay" });
  assert.ok(packed.includes(manifest.replay.executable), `package.json files ${JSON.stringify(packed)} lack ${manifest.replay.executable}`);
});
