// Checks the workflows against core docs/spec/plugins.md: the release starts on a push to main, runs ci.yml and the release
// gate, downloads the sok of the declared core release, and no workflow checks out another repository; the dependencies on
// core are the packed release files of that core release.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import test from "node:test";

const read = (path) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
const release = read(".github/workflows/release.yml");

/** The core release that release.yml declares in CORE_RELEASE. */
function coreRelease() {
  const value = /^\s*CORE_RELEASE:\s*(\S+)\s*$/m.exec(release)?.[1];
  assert.match(value ?? "", /^v\d+\.\d+\.\d+$/, `CORE_RELEASE must name a core release tag, found ${JSON.stringify(value)}`);
  return value;
}

test("the release starts on a push to main and runs ci.yml and the release gate", { timeout: 1000 }, () => {
  assert.match(release, /^on:\n {2}push:\n {4}branches: \[main\]\n/m, "release.yml must start on a push to main");
  assert.doesNotMatch(release, /^\s*tags:/m, "release.yml must not start on a pushed tag");
  assert.match(release, /^\s*uses: \.\/\.github\/workflows\/ci\.yml$/m, "release.yml must run ci.yml as its checks");
  assert.match(read(".github/workflows/ci.yml"), /^on:\n(?: {2}.+\n)* {2}workflow_call:/m, "ci.yml must be callable by release.yml");
  assert.match(release, /soksak-release-gate vt-alacritty\/package\.json docs\/features\.md/, "release.yml must run the release gate");
  assert.match(release, /git ls-remote origin refs\/heads\/main/, "the release must require the run commit to be the head of main");
  assert.match(release, /git ls-remote --tags origin "refs\/tags\/v\$VERSION"/, "the release must require the tag to be absent");
});

test("the release downloads the sok of the declared core release", { timeout: 1000 }, () => {
  const tag = coreRelease();
  assert.match(release, /gh release download "\$CORE_RELEASE" --repo soksak-app\/core --pattern 'sok-\*-darwin-arm64'/,
    `release.yml must download the sok of ${tag}`);
  assert.doesNotMatch(release, /go build/, "release.yml must not build sok from source");
});

test("no workflow checks out another repository", { timeout: 1000 }, () => {
  for (const file of readdirSync(new URL("../.github/workflows/", import.meta.url)).filter((name) => name.endsWith(".yml"))) {
    assert.doesNotMatch(read(`.github/workflows/${file}`), /^\s*repository:/m, `${file} checks out another repository`);
  }
});

test("the dependencies on core are the packed release files of the declared core release", { timeout: 1000 }, () => {
  const tag = coreRelease();
  const version = tag.slice(1);
  const manifest = JSON.parse(read("package.json"));
  const base = `https://github.com/soksak-app/core/releases/download/${tag}`;
  assert.equal(manifest.devDependencies["@soksak/plugin-api"], `${base}/soksak-plugin-api-${version}.tgz`);
  assert.equal(manifest.optionalDependencies["@soksak/native-process-identity-darwin"],
    `${base}/soksak-native-process-identity-darwin-${version}.tgz`);
});
