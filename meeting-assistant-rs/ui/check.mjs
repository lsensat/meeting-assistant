/**
 * Parse every UI module as an ES module, and fail on a syntax error.
 *
 * # Why this exists
 *
 * A stray edit closed a JSDoc comment early in `main.js`, leaving the tail of
 * that comment — an `@param` line and the comment's own closing delimiter —
 * stranded as bare code.
 *
 * (This file will not quote it literally. The first attempt did, the closing
 * delimiter ended this very comment, and the checker failed its own check. That
 * is a fair demonstration of how easy the mistake is.)
 *
 * `main.js` therefore never parsed, no JavaScript ran, and the
 * window showed nothing but its static HTML — including the placeholder text
 * "Checking environment...", which is indistinguishable from a startup check
 * that began and hung. It reached `main` and two release builds.
 *
 * `node --check` was run after every edit and passed every time: it validates a
 * file as a **script**, and this failure only appears on **module** parse. That
 * is the specific hole this closes.
 *
 * # What it does and does not do
 *
 * It compiles each module. It does not execute one — that would need a DOM and
 * a Tauri IPC global, and a check that needs a fake browser is a check nobody
 * keeps running. Syntax is the failure class that takes the whole window down
 * silently, so syntax is what this catches.
 *
 * Run with: node ui/check.mjs
 */

import { readdir } from "node:fs/promises";
import { dirname, join } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";

const jsDir = join(dirname(fileURLToPath(import.meta.url)), "js");

const files = (await readdir(jsDir)).filter((name) => name.endsWith(".js")).sort();

if (files.length === 0) {
  console.error(`no modules found in ${jsDir} — has the UI moved?`);
  process.exit(1);
}

let failed = 0;

for (const file of files) {
  const path = join(jsDir, file);
  try {
    // `import` compiles the module and everything it imports. A missing import
    // is caught here too: it fails to resolve rather than to parse.
    await import(pathToFileURL(path).href);
    console.log(`  ok       ${file}`);
  } catch (error) {
    // Only build-time failures are this script's business. Anything thrown
    // while the module *runs* is a missing browser global — expected under
    // Node, and not what this is looking for.
    const buildTime =
      error instanceof SyntaxError ||
      /Cannot find module|Failed to resolve|ERR_MODULE_NOT_FOUND/.test(String(error?.message));

    if (buildTime) {
      failed += 1;
      console.error(`  FAILED   ${file}`);
      console.error(`           ${error.constructor.name}: ${error.message.split("\n")[0]}`);
    } else {
      console.log(`  ok       ${file}  (parsed; runtime needs a browser)`);
    }
  }
}

if (failed > 0) {
  console.error(`\n${failed} module(s) failed to compile.`);
  process.exit(1);
}

console.log(`\n${files.length} modules compile.`);
