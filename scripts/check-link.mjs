#!/usr/bin/env node
// Runs a command and fails it when the linker reports an object built for a newer macOS than the one it links for (S163).
// The linker only warns, and rustc prints that warning under the lint linker_info, which the workspace sets to warn; without
// this check the build passes and the executable holds code compiled for a newer macOS than it declares. Other linker_info
// lines pass: the standard library and the libc crate both pass `-lc` to the linker, so every executable link prints
// `ignoring duplicate libraries: '-lc'`, which this repository cannot remove and which changes no object of the executable.
// Each output line passes through; the status is that of the command, or 1 when a command that succeeded printed the line.
//
//   node scripts/check-link.mjs cargo build -p soksak-sidecar-vt-alacritty
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { pathToFileURL } from "node:url";

/** Whether a line of the linker output names an object built for a newer macOS than the link. */
export function builtForNewer(line) {
  return line.includes("was built for newer");
}

/**
 * Runs command with args, writes each line of its standard output through format and each line of its standard error
 * unchanged, and resolves to its status; a command that printed a line of an object built for a newer macOS fails.
 */
export async function runChecked(command, args, { env = process.env, format = (line) => line } = {}) {
  const child = spawn(command, args, { env, stdio: ["inherit", "pipe", "pipe"] });
  let newer = 0;
  const outputs = [[child.stdout, process.stdout, format], [child.stderr, process.stderr, (line) => line]].map(
    ([input, output, write]) => new Promise((resolve) => {
      const lines = createInterface({ input, crlfDelay: Infinity });
      lines.on("line", (line) => {
        if (builtForNewer(line)) newer += 1;
        output.write(`${write(line)}\n`);
      });
      lines.on("close", resolve);
    }),
  );
  const status = await new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", (code, signal) => {
      if (signal) {
        reject(new Error(`${command} ended by signal ${signal}`));
      } else {
        resolve(code);
      }
    });
  });
  await Promise.all(outputs);
  if (newer > 0) {
    process.stderr.write(`check-link: the linker linked ${newer} object${newer === 1 ? "" : "s"} built for a newer macOS than the executable declares\n`);
    return status === 0 ? 1 : status;
  }
  return status;
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const [command, ...args] = process.argv.slice(2);
  if (!command) {
    process.stderr.write("usage: node scripts/check-link.mjs <command> [arguments]\n");
    process.exitCode = 2;
  } else {
    runChecked(command, args).then((status) => {
      process.exitCode = status;
    }, (error) => {
      process.stderr.write(`check-link: ${error.message}\n`);
      process.exitCode = 1;
    });
  }
}
