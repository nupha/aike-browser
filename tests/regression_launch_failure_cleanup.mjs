// Regression test: a FAILED Browser.launch() must not orphan the process it started.
//
// Bug: when the post-spawn CDP connect failed, the code did
//     let _ = child.start_kill();   // request only — never reaped
// and the error propagated, so the module lost its handle to the child. The
// process was never waited on: it lingered as a zombie still holding its
// `aike-chrome-*` profile dir (observed: 3 such processes, all with dead debug
// ports, left behind by a batch containing launch failures).
//
// Reaching the buggy path requires a stand-in that:
//   * writes DevToolsActivePort  -> `wait_for_devtools_port` succeeds, so the
//     child IS handed back to the connect step (with a plain long-lived binary
//     like `node` the wait times out first and the path is never reached), and
//   * does NOT speak CDP          -> `CdpClient::connect` fails.
// tests/fake_cdp_server.mjs does both. The launched stand-in is a `node`
// process, so we count node processes using an aike profile dir.

import { Browser } from "browser";
import * as fs from "fs";

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

function decode(u8) {
  return new TextDecoder().decode(u8);
}

// Count stand-in processes alive with an aike profile dir on their command line.
function orphanCount() {
  const ps = Aike.spawnSync("powershell", [
    "-NoProfile",
    "-Command",
    "@(Get-CimInstance Win32_Process -Filter \"Name='fake_cdp_server.exe'\" | " +
      "Where-Object { $_.CommandLine -match 'aike-chrome-' }).Count",
  ]);
  const n = parseInt(decode(ps.stdout).trim(), 10);
  return Number.isFinite(n) ? n : -1;
}

// Names of `aike-chrome-*` profile dirs created at/after `sinceMs` (epoch ms).
// Age is the only safe attribution: %TEMP% holds hundreds of older leftovers.
async function profileDirsCreatedAfter(sinceMs) {
  const ps = Aike.spawnSync("powershell", [
    "-NoProfile",
    "-Command",
    "$cut=[datetimeoffset]::FromUnixTimeMilliseconds(" + sinceMs + ").LocalDateTime; " +
      "Get-ChildItem $env:TEMP -Directory -Filter 'aike-chrome-*' " +
      "-ErrorAction SilentlyContinue | Where-Object { $_.CreationTime -ge $cut } | " +
      "Select-Object -ExpandProperty Name",
  ]);
  return decode(ps.stdout)
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

console.log("\n=== failed launch must not orphan the spawned process ===");

// A native .exe stand-in (built from fake_cdp_server.rs), so it is the DIRECT
// child of aike exactly like chrome.exe. A .cmd shim would insert a cmd.exe
// layer whose grandchild survives killing the direct child — an artifact of the
// harness, not of the module under test.
const shim = new URL("./fake_cdp_server.exe", import.meta.url).pathname.replace(
  /^\//,
  "",
);
if (!(await fs.exists(shim))) {
  console.log("tests/fake_cdp_server.exe missing -- build it with:");
  console.log("  rustc -O -o fake_cdp_server.exe fake_cdp_server.rs");
  console.log("SKIP");
  process.exit(0);
}
console.log("  stand-in binary: " + shim);

// Record the run start so only profile dirs created from now on are counted:
// `%TEMP%` holds hundreds of pre-fix leftovers that must not be attributed here.
const startedAt = Date.now();

const before = orphanCount();
console.log("  orphaned stand-ins before: " + before);

let threw = false;
let errMsg = "";
try {
  await Browser.launch({ headless: true, chromePath: shim, port: 0 });
} catch (e) {
  threw = true;
  errMsg = String((e && (e.message || e)) || e);
}

check("launch with a non-CDP endpoint rejects", threw, "it resolved instead");
check(
  "rejection names the CDP/WebSocket failure",
  /CDP|WebSocket|connect/i.test(errMsg),
  "got: " + errMsg,
);

// Give any orphan a moment to appear / any reaper a moment to finish.
await new Promise((r) => setTimeout(r, 3000));

const after = orphanCount();
console.log("  orphaned stand-ins after: " + after);

check(
  "no orphaned stand-in process left by the failed launch",
  after >= 0 && after <= before,
  "before=" + before + " after=" + after,
);

// The residue the bug genuinely produced: the module lost its handle to the
// child, so the child's `aike-chrome-*` profile dir was never removed.
// (`kill_on_drop` usually still kills the process, which is why the process
// check above can pass even on the buggy build — the dir is the real signal.)
const newDirs = await profileDirsCreatedAfter(startedAt);
console.log("  new profile dirs from this run: " + newDirs.length);
check(
  "failed launch removed the profile dir it created",
  newDirs.length === 0,
  "left behind: " + newDirs.join(", "),
);

console.log("\n" + "=".repeat(50));
console.log("pass: " + pass + ", fail: " + fail);
if (fail > 0) process.exit(1);
