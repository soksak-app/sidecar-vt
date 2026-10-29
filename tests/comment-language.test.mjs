// Test of the check that the comments of the tracked sources are written in English.
import assert from "node:assert/strict";
import test from "node:test";
import { auditCommentLanguage, comments, foreignLetters, sourceKind } from "../scripts/check-comment-language.mjs";

// The test builds its non-Latin text from code points, so this file holds no such letter itself.
const HAN = String.fromCodePoint(0xd55c);
const HANGUL = String.fromCodePoint(0xd55c, 0xae00);

test("a comment letter of another script is found, and escaped letters are found when they form a run", { timeout: 1000 }, () => {
  assert.deepEqual(foreignLetters(`// ${HANGUL} text`), [...HANGUL]);
  assert.deepEqual(foreignLetters("// \\ud55c\\uae00 and \\u{d55c} \\u{ae00}"), [...HANGUL, ...HANGUL]);
  // One escaped letter is the code point of an example input, not text.
  assert.deepEqual(foreignLetters('/// "\\u{d55c}a" with a newline; add_char(\'\\ud55c\')'), []);
  assert.deepEqual(foreignLetters("// café → 10 µs, U+D55C"), []);
});

test("only comments are read: a string literal or a character literal of a source is not a comment", { timeout: 10000 }, async () => {
  const rust = `/// Doc ${HAN}\nfn f() -> char { let s = "// ${HAN}"; '${HAN}' } /* block\n${HAN} */\n`;
  assert.deepEqual((await comments("rust", rust)).map(({ line, text }) => [line, foreignLetters(text).length > 0]),
    [[1, true], [2, true]]);
  const javascript = `const s = "// ${HAN}"; // ${HAN}\nconst t = \`/* ${HAN} */\`;\n`;
  assert.deepEqual((await comments("javascript", javascript)).map(({ line }) => line), [1]);
  const shell = `#!/bin/zsh\necho "# ${HAN}" # ${HAN}\n`;
  assert.deepEqual((await comments("bash", shell)).filter(({ text }) => foreignLetters(text).length > 0).map(({ line }) => line), [2]);
  assert.deepEqual(await comments("lines", `key = "# ${HAN}"\n  # ${HAN}\n`), [{ line: 2, text: `# ${HAN}` }]);
});

test("the kinds of the tracked sources are Rust, JavaScript, shell and the line comment formats", { timeout: 1000 }, () => {
  assert.equal(sourceKind("vt-core/src/pty.rs", ""), "rust");
  assert.equal(sourceKind("scripts/check.mjs", ""), "javascript");
  assert.equal(sourceKind("vt-core/src/shell-integration/zshenv", ""), "bash");
  assert.equal(sourceKind("tool", "#!/bin/sh\n"), "bash");
  assert.equal(sourceKind("Makefile", ""), "lines");
  assert.equal(sourceKind(".github/workflows/ci.yml", ""), "lines");
  assert.equal(sourceKind("docs/spec.md", ""), null);
});

test("every comment of the tracked sources of this repository is written in English", { timeout: 60000 }, async () => {
  assert.deepEqual(await auditCommentLanguage(), []);
});
