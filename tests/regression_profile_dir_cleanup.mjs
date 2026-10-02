// Regression test: Chrome profile dirs must not accumulate without bound.
//
// Bug: every auto-port launch created `%TEMP%\aike-chrome-<pid>-<seq>` and
// nothing ever removed it — 453 dirs had piled up (earliest 2026-09-28).
// `close()` now reaps the child (a dir can only be deleted once Chrome has
// actually exited) and then removes that dir.
//
// This script launches and closes N times, then asserts that the dirs created
// by *this* process were removed. It counts only `<this pid>-*` dirs so prior
// leftovers in %TEMP% never make it fail.

import { Browser } from "browser";

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

const pid = process.pid;

// `SyncSubprocess.stdout` is a Uint8Array (per aike.d.ts), not a string.
function decode(u8) {
  return new TextDecoder().decode(u8);
}

function listOwnProfileDirs() {
  const ps = Aike.spawnSync("powershell", [
    "-NoProfile",
    "-Command",
    "@(Get-ChildItem $env:TEMP -Directory -Filter 'aike-chrome-" + pid + "-*' " +
      "-ErrorAction SilentlyContinue).Count",
  ]);
  const n = parseInt(decode(ps.stdout).trim(), 10);
  return Number.isFinite(n) ? n : -1;
}

console.log("\n=== profile dirs are cleaned up after close() ===");
console.log("  pid=" + pid + ", rounds=" + rounds);

const before = listOwnProfileDirs();
console.log("  own profile dirs before: " + before);

for (let i = 1; i <= rounds; i++) {
  const browser = await Browser.launch({ headless: true });
  const page = await browser.newPage();
  await page.navigate("data:text/html,<h1>cleanup</h1>");
  await page.close();
  await browser.close();
  console.log("  cycle " + i + "/" + rounds + " closed");
}

await new Promise((r) => setTimeout(r, 2000));
const after = listOwnProfileDirs();
console.log("  own profile dirs after: " + after);

check("profile dir count is readable", before >= 0 && after >= 0, "before=" + before + " after=" + after);
check(
  "close() removed the profile dirs it created",
  after <= before,
  "before=" + before + " after=" + after + " (" + rounds + " launches)",
);

console.log("\n" + "=".repeat(50));
console.log("pass: " + pass + ", fail: " + fail);
if (fail > 0) process.exit(1);
