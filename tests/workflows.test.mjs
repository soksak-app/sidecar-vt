// Checks the workflows against core docs/spec/plugins.md: the release starts on a push to main, runs ci.yml and the release
// gate, and writes the npm pack file of the built sidecar folder without a core release; no workflow checks out another
// repository, and the dependencies on core are the packed release files of the core release that package.json pins.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { spawnSync } from "node:child_process";
import test from "node:test";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const release = read(".github/workflows/release.yml");

test("the release starts on a push to main and runs ci.yml and the release gate", { timeout: 1000 }, () => {
  assert.match(release, /^on:\n {2}push:\n {4}branches: \[main\]\n/m, "release.yml must start on a push to main");
  assert.doesNotMatch(release, /^\s*tags:/m, "release.yml must not start on a pushed tag");
  assert.match(release, /^\s*uses: \.\/\.github\/workflows\/ci\.yml$/m, "release.yml must run ci.yml as its checks");
  assert.match(read(".github/workflows/ci.yml"), /^on:\n(?: {2}.+\n)* {2}workflow_call:/m, "ci.yml must be callable by release.yml");
  assert.match(release, /soksak-release-gate vt-alacritty\/package\.json docs\/features\.md/, "release.yml must run the release gate");
  assert.match(release, /git ls-remote origin refs\/heads\/main/, "the release must require the run commit to be the head of main");
  assert.match(release, /git ls-remote --tags origin "refs\/tags\/v\$VERSION"/, "the release must require the tag to be absent");
});

test("the release writes the npm pack file of the sidecar and downloads no core release", { timeout: 1000 }, () => {
  assert.match(release, /^\s*run: make release .*OUT=dist$/m, "release.yml must run make release with OUT=dist");
  for (const [pattern, what] of [[/gh release download/, "a release download"], [/CORE_RELEASE/, "a core release variable"],
    [/SOK=/, "a sok path"]]) {
    assert.doesNotMatch(release, pattern, `release.yml must not hold ${what}`);
  }
});

test("make release writes the npm pack file of vt-alacritty named with the platform into the folder that OUT names", { timeout: 30000 }, () => {
  const result = spawnSync("make", ["-n", "release", "OUT=/releases"], { cwd: new URL("..", import.meta.url), encoding: "utf8", timeout: 25000 });
  assert.equal(result.status, 0, result.stderr);
  const pack = result.stdout.trim().split("\n").at(-1);
  assert.match(pack, /npm pack --silent --pack-destination \/releases/, `make release runs ${JSON.stringify(pack)}`);
  assert.match(pack, /-(?:darwin|linux|windows)-(?:arm64|x64)\.tgz"$/, `make release names the release with the platform: ${pack}`);
  assert.doesNotMatch(read("Makefile"), /\bSOK\b|sidecar release/, "the Makefile must not name a sok");
});

test("no workflow checks out another repository", { timeout: 1000 }, () => {
  for (const file of readdirSync(new URL("../.github/workflows/", import.meta.url)).filter((name) => name.endsWith(".yml"))) {
    assert.doesNotMatch(read(`.github/workflows/${file}`), /^\s*repository:/m, `${file} checks out another repository`);
  }
});

test("the dependencies on core are the packed release files of the core release that package.json pins", { timeout: 1000 }, () => {
  const manifest = JSON.parse(read("package.json"));
  const base = "https://github.com/soksak-app/core/releases/download/v0.0.25";
  assert.equal(manifest.devDependencies["@soksak/plugin-api"], `${base}/soksak-plugin-api-0.0.25.tgz`);
  assert.equal(manifest.optionalDependencies["@soksak/native-process-identity-darwin"],
    `${base}/soksak-native-process-identity-darwin-0.0.25.tgz`);
});


test("pnpm-workspace.yaml names no git source of core", { timeout: 1000 }, () => {
  assert.doesNotMatch(read("pnpm-workspace.yaml"), /github\.com\/soksak-app\/core\.git/,
    "the dependencies on core are packed release files, so no setting names a git source of core");
});
