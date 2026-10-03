// Regression test: Chrome profile dirs must not accumulate without bound.
//
// Bug: every auto-port launch created `<tempdir>/aike-chrome-<pid>-<seq>` and
// nothing ever removed it — 453 dirs had piled up (earliest 2026-09-28). Later,
// on macOS, a further batch leaked *empty* dirs: the removal reported success,
// but Chrome's helper processes outlived the browser process long enough to
// recreate the dir a moment later, and nothing ever revisited it.
// `close()` now reaps the child (a dir can only be deleted once Chrome has
// actually exited) and removes the dir again until it stays gone.
//
// This script launches and closes N times, then asserts that no dir is left. It
// snapshots the dir set before the run and diffs after, so leftovers from
// earlier runs never make it fail — and it locates them with `os.tmpdir()` +
// `fs.readDir`, exactly how the module picks the location, so the test follows
// the module onto Windows, macOS and Linux alike.

import { Browser } from "browser";
import * as fs from "fs";
import * as os from "os";

const rounds = Number(process.argv[2] || 5);

let pass = 0;
let fail = 0;
function check(label, condition, detail) {
  if (condition) {
    console.log("  [ok] " + label);
    pass++;
  } else {
    console.log("  [FAIL] " + label + (detail ? " -- " + detail : ""));
    fail++;
  }
}

// Names of the auto-created `aike-chrome-*` profile dirs currently in the
// platform temp dir (`%TEMP%` / `$TMPDIR` / `/tmp`).
async function profileDirs() {
  const names = [];
  for (const e of await fs.readDir(os.tmpdir())) {
    if (e.isDirectory && e.name.startsWith("aike-chrome-")) names.push(e.name);
  }
  return names;
}

console.log("\n=== profile dirs are cleaned up after close() ===");
console.log(
  "  pid=" + process.pid + ", rounds=" + rounds + ", tmpdir=" + os.tmpdir(),
);

const before = new Set(await profileDirs());
console.log("  profile dirs before: " + before.size);

for (let i = 1; i <= rounds; i++) {
  const browser = await Browser.launch({ headless: true });
  const page = await browser.newPage();
  await page.navigate("data:text/html,<h1>cleanup</h1>");
  await page.close();
  await browser.close();
  console.log("  cycle " + i + "/" + rounds + " closed");
}

// `close()` resolves once the child is reaped and its dir removed, so a leftover
// here is a real leak; poll anyway, so a slow final removal round on a loaded
// machine is not read as one.
async function waitForNoNewDirs(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let leaked = (await profileDirs()).filter((n) => !before.has(n));
  while (leaked.length > 0 && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 250));
    leaked = (await profileDirs()).filter((n) => !before.has(n));
  }
  return leaked;
}

const leaked = await waitForNoNewDirs(20000);
console.log("  profile dirs left by this run: " + leaked.length);
check(
  "close() removed the profile dirs it created",
  leaked.length === 0,
  "left behind: " + leaked.join(", ") + " (" + rounds + " launches)",
);

console.log("\n" + "=".repeat(50));
console.log("pass: " + pass + ", fail: " + fail);
if (fail > 0) process.exit(1);
