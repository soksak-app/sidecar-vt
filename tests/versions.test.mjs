// Checks that the version of the package.json of the sidecar equals the version of each Rust crate. package.json sets the
// version of the releases.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");

/** Cargo.toml 의 [package] version. */
function crateVersion(text) {
  let inPackage = false;
  for (const line of text.split("\n")) {
    const section = line.match(/^\s*\[([^\]]+)\]\s*$/);
    if (section) {
      inPackage = section[1] === "package";
      continue;
    }
    const version = inPackage && line.match(/^\s*version\s*=\s*"([^"]*)"\s*$/);
    if (version) return version[1];
  }
  return undefined;
}

test("every crate declares the version of the package.json of the sidecar", () => {
  const { version } = JSON.parse(read("vt-alacritty/package.json"));
  for (const crate of ["vt-alacritty/Cargo.toml", "vt-core/Cargo.toml"]) {
    assert.equal(crateVersion(read(crate)), version, crate);
  }
});
