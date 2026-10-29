#!/usr/bin/env node
// Checks that the comments of the tracked sources are written in English. A comment fails when it holds a letter of a
// script other than Latin, Greek and Common; Greek and Common letters are the symbols of units and quantities in English
// technical text, such as µs. A letter written as a `\uXXXX` or `\u{X}` escape is the code point of an example input
// when it stands alone, as in "\u{d55c}a", and text when two or more such escapes form a run, so a run fails. The comments
// of Rust, JavaScript and shell sources are the comment nodes of their tree-sitter grammars, so a string literal is never
// read as a comment; Makefile, TOML and YAML comments are the lines that start with `#`.
//
//   node scripts/check-comment-language.mjs
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import { Language, Parser } from "web-tree-sitter";

const require = createRequire(import.meta.url);
const root = fileURLToPath(new URL("../", import.meta.url));

const GRAMMARS = {
  rust: "tree-sitter-rust/tree-sitter-rust.wasm",
  javascript: "tree-sitter-javascript/tree-sitter-javascript.wasm",
  bash: "tree-sitter-bash/tree-sitter-bash.wasm",
};

/** The grammar of a tracked file, `lines` for a format whose comments are `#` lines, or null for a file without comments to check. */
export function sourceKind(path, text) {
  if (path.endsWith(".rs")) return "rust";
  if (/\.(mjs|cjs|js)$/.test(path)) return "javascript";
  if (path.endsWith(".sh") || /^#!.*\b(sh|bash|zsh)\b/.test(text) || /^\.?z(shenv|shrc|profile|login)$/.test(basename(path))) return "bash";
  if (basename(path) === "Makefile" || /\.(toml|ya?ml)$/.test(path)) return "lines";
  return null;
}

const FOREIGN = /(?![\p{Script=Latin}\p{Script=Greek}\p{Script=Common}])\p{L}/u;
const ESCAPE = /\\u\{([0-9a-fA-F]{1,6})\}|\\u([0-9a-fA-F]{4})/g;
const decode = (_, braced, plain) => String.fromCodePoint(parseInt(braced ?? plain, 16));

/** The letters of text that fail: raw letters of another script, and escaped letters of another script in runs of two or more. */
export function foreignLetters(text) {
  const raw = [...text.replace(ESCAPE, "").matchAll(new RegExp(FOREIGN, "gu"))].map((match) => match[0]);
  const runs = [...text.matchAll(/(?:\\u\{[0-9a-fA-F]{1,6}\}|\\u[0-9a-fA-F]{4}|\s)+/g)]
    .map((match) => [...match[0].replace(ESCAPE, decode)].filter((letter) => FOREIGN.test(letter)))
    .filter((letters) => letters.length >= 2);
  return [...raw, ...runs.flat()];
}

let parsers;
async function loadParsers() {
  await Parser.init();
  const loaded = {};
  for (const [kind, file] of Object.entries(GRAMMARS)) {
    const parser = new Parser();
    parser.setLanguage(await Language.load(require.resolve(file)));
    loaded[kind] = parser;
  }
  return loaded;
}

/** The comments of a source text as `{ line, text }`, line counted from 1. */
export async function comments(kind, text) {
  if (kind === "lines") {
    return text.split("\n").flatMap((line, index) => (/^\s*#/.test(line) ? [{ line: index + 1, text: line.trim() }] : []));
  }
  parsers ??= await loadParsers();
  const tree = parsers[kind].parse(text);
  if (tree === null) throw new Error(`the ${kind} grammar returned no tree`);
  try {
    return tree.rootNode.descendantsOfType(["line_comment", "block_comment", "comment"])
      .map((node) => ({ line: node.startPosition.row + 1, text: node.text }));
  } finally {
    tree.delete();
  }
}

/** The violations of the tracked files of a repository as `path:line: <letters>: <comment>`. */
export async function auditCommentLanguage(repository = root) {
  const files = execFileSync("git", ["ls-files", "-z"], { cwd: repository, encoding: "utf8" }).split("\0").filter(Boolean);
  const errors = [];
  for (const path of files) {
    const text = readFileSync(join(repository, path), "utf8");
    const kind = sourceKind(path, text);
    if (kind === null) continue;
    for (const comment of await comments(kind, text)) {
      // A block comment spans lines, so each of its lines is reported at its own line.
      comment.text.split("\n").forEach((line, offset) => {
        const letters = foreignLetters(line);
        if (letters.length > 0) errors.push(`${path}:${comment.line + offset}: ${[...new Set(letters)].join("")}: ${line.trim()}`);
      });
    }
  }
  return errors;
}

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const errors = await auditCommentLanguage();
  if (errors.length > 0) {
    console.error(errors.join("\n"));
    console.error(`${errors.length} comments are not written in English`);
    process.exit(1);
  }
  console.log("Comment language check passed");
}
