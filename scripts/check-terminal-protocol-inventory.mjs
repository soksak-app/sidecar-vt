import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL("../", import.meta.url));
const engine = readFileSync(`${ROOT}vt-alacritty/src/engine.rs`, "utf8");
const tests = readFileSync(`${ROOT}vt-alacritty/tests/engine_test.rs`, "utf8");
const specification = readFileSync(`${ROOT}docs/spec/terminal-protocols.md`, "utf8");
// A test that the specification report names as evidence can be anywhere in the Rust sources of the sidecar.
const rustFiles = (directory) => readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
  const path = join(directory, entry.name);
  if (entry.isDirectory()) return entry.name === "target" ? [] : rustFiles(path);
  return entry.name.endsWith(".rs") ? [path] : [];
});
const sidecarRust = ["vt-core", "vt-alacritty"]
  .flatMap((name) => rustFiles(`${ROOT}${name}`)).map((path) => readFileSync(path, "utf8")).join("\n");

/** "13-19,21" 이나 "`13`–`19`, `21`" 같은 선택자 목록을 선택자 하나씩으로 펼친다. */
function selectors(list) {
  return list.replaceAll("`", "").split(",").map((part) => part.trim()).filter(Boolean).flatMap((part) => {
    const range = /^(\d+)\s*[-–]\s*(\d+)$/.exec(part);
    if (!range) return [part];
    const out = [];
    for (let value = Number(range[1]); value <= Number(range[2]); value++) out.push(String(value));
    return out;
  });
}

const OUTCOME_WORDS = { Implemented: "implemented", Unsupported: "unsupported", Vendor: "vendor implemented" };

/**
 * 명세의 OSC 보고서 표를 엔진 목록과 선택자 하나씩 비교한다. 모든 선택자는 두 곳에 같은 결과로 있어야 하고,
 * 표가 근거로 적은 테스트는 사이드카 소스에 있어야 한다.
 */
function auditOscReport(oscRows, specSource, evidenceSource) {
  const errors = [];
  const section = specSource.split("## OSC selector inventory")[1]?.split("\n## ")[0] ?? "";
  const report = new Map();
  for (const line of section.split("\n")) {
    const cells = /^\| (`[^|]+) \| [^|]+ \| `([^`]+)`[^|]* \| ([^|]+) \|$/.exec(line);
    if (!cells) continue;
    const tests = [...cells[3].matchAll(/`([A-Za-z0-9_]+)`/g)].map(([, name]) => name);
    for (const selector of selectors(cells[1])) {
      if (report.has(selector)) errors.push(`OSC selector ${selector} has more than one row in the specification report`);
      report.set(selector, { outcome: cells[2], tests });
    }
  }
  const engine = new Map();
  for (const row of oscRows) for (const selector of selectors(row.selector)) engine.set(selector, OUTCOME_WORDS[row.outcome]);
  for (const [selector, outcome] of engine) {
    const row = report.get(selector);
    if (!row) {
      errors.push(`OSC selector ${selector} has no row in the specification report`);
      continue;
    }
    if (row.outcome !== outcome) {
      errors.push(`OSC selector ${selector} is ${outcome} in the engine but ${row.outcome} in the specification report`);
    }
    if (row.tests.length === 0) errors.push(`OSC selector ${selector} names no test in the specification report`);
    for (const test of row.tests) {
      if (!evidenceSource.includes(`fn ${test}(`)) errors.push(`OSC selector ${selector} names a test that does not exist: ${test}`);
    }
  }
  for (const selector of report.keys()) {
    if (!engine.has(selector)) errors.push(`OSC selector ${selector} is in the specification report but not in the engine inventory`);
  }
  return errors;
}

const REQUIRED_ROWS = [
  "A/B/C/D/G/H/f/s/u",
  "E/F",
  "?12h/l",
  "?25h/l",
  "0,7 SP q",
  "1-6 SP q",
  "CSI framing",
  "m",
  "?1049h/l",
  "S/T;r",
  "J/K",
  "@/P",
  "L/M",
  "I/Z",
  "6n/c",
  "5n/6n",
  "c/>c",
  "b",
  "14t",
  "other t",
  "rectangle/protected/palette",
  "?47/?1047/?1048h/l",
  "?1,?1000,?1002,?1003,?1004,?1005,?1006,?1007,?2004 h/l",
  "ESC =/>",
];

const REQUIRED_OSC_ROWS = [
  "0,2", "1,3", "4", "5,6,105,106", "10-12", "13-16,18,21,46", "17,19,117,119", "22", "50", "51", "52",
  "60-62", "104", "110-112", "I,l,L", "7,8,9,133", "1337",
];

export function auditTerminalProtocolInventory({
  engineSource = engine, testSource = tests, specSource = specification, evidenceSource = sidecarRust,
} = {}) {
  const errors = [];
  if (!specSource.includes("XTerm control sequences, patch 411, 2026-08-23")) {
    errors.push("pinned XTerm patch 411 reference is missing");
  }
  if (!specSource.includes("CSI: every standard final byte, parameter form, private mode, and device-response sequence")) {
    errors.push("specification does not state the complete CSI inventory contract");
  }

  const rows = [...engineSource.matchAll(/CsiSelectorEvidence \{\s*selector: "([^"]+)",\s*outcome: CsiOutcome::(Implemented|Unsupported),\s*test: "([^"]+)",?\s*\}/g)]
    .map(([, selector, outcome, test]) => ({ selector, outcome, test }));
  const oscRows = [...engineSource.matchAll(/OscSelectorEvidence \{\s*selector: "([^"]+)",\s*outcome: OscOutcome::(Implemented|Unsupported|Vendor),\s*test: "([^"]+)",?\s*\}/g)]
    .map(([, selector, outcome, test]) => ({ selector, outcome, test }));
  const seen = new Set();
  for (const row of rows) {
    if (seen.has(row.selector)) errors.push(`duplicate CSI selector row: ${row.selector}`);
    seen.add(row.selector);
    if (!testSource.includes(`fn ${row.test}(`)) errors.push(`${row.selector}: named test is missing: ${row.test}`);
  }
  for (const selector of REQUIRED_ROWS) {
    if (!seen.has(selector)) errors.push(`required CSI inventory row is missing: ${selector}`);
  }
  const seenOsc = new Set();
  for (const row of oscRows) {
    if (seenOsc.has(row.selector)) errors.push(`duplicate OSC selector row: ${row.selector}`);
    seenOsc.add(row.selector);
    if (!testSource.includes(`fn ${row.test}(`)) errors.push(`${row.selector}: named OSC test is missing: ${row.test}`);
  }
  for (const selector of REQUIRED_OSC_ROWS) {
    if (!seenOsc.has(selector)) errors.push(`required OSC inventory row is missing: ${selector}`);
  }
  errors.push(...auditOscReport(oscRows, specSource, evidenceSource));
  return { errors, rowCount: rows.length, oscRowCount: oscRows.length };
}

const result = auditTerminalProtocolInventory();
if (result.errors.length > 0) {
  for (const error of result.errors) console.error(`FAIL terminal protocol inventory: ${error}`);
  process.exitCode = 1;
} else {
  console.log(`PASS terminal protocol inventory: ${result.rowCount} unique CSI rows and ${result.oscRowCount} unique OSC rows with named tests`);
}
