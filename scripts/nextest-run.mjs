#!/usr/bin/env node
// Runs cargo-nextest and prints a running line before the result of each test (S76-1). The human output of nextest 0.9.146
// prints no start line, so the runner reads the libtest JSON output of nextest (experimental, enabled by
// NEXTEST_EXPERIMENTAL_LIBTEST_JSON=1), which emits a started event for each test. Lines that are not test events are
// printed unchanged, so no nextest output is dropped. The human summary of nextest stays on stderr. The output of Cargo
// passes through the link check of scripts/check-link.mjs, which fails a run whose linker linked an object built for a
// newer macOS (S163).
//
//   node scripts/nextest-run.mjs --config-file .config/nextest.toml --workspace
import { pathToFileURL } from "node:url";
import { runChecked } from "./check-link.mjs";

// Formats one line of the libtest JSON output. A line that is not a JSON object, a suite event and a test event other than
// started or ok are returned unchanged; a failed test keeps its JSON line, which holds the output of the test.
export function formatEvent(line) {
  if (!line.startsWith("{")) {
    return line;
  }
  const event = JSON.parse(line);
  if (event.type !== "test") {
    return line;
  }
  const name = event.name.replace("$", " ");
  switch (event.event) {
    case "started":
      return `RUNNING ${name}`;
    case "ok":
      return `PASS [${event.exec_time.toFixed(3).padStart(8)}s] ${name}`;
    case "failed":
      return `FAIL [${event.exec_time.toFixed(3).padStart(8)}s] ${name}\n${line}`;
    default:
      return line;
  }
}

async function run(args) {
  process.exitCode = await runChecked("cargo", ["nextest", "run", "--message-format", "libtest-json-plus", ...args], {
    env: { ...process.env, NEXTEST_EXPERIMENTAL_LIBTEST_JSON: "1" },
    format: formatEvent,
  });
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  run(process.argv.slice(2)).catch((error) => {
    process.stderr.write(`nextest-run: ${error.message}\n`);
    process.exitCode = 1;
  });
}
