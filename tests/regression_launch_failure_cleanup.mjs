// Regression test: a FAILED Browser.launch() must not orphan the process it started.
//
// Bug: when the post-spawn CDP connect failed, the code did
//     let _ = child.start_kill();   // request only — never reaped
// and the error propagated, so the module lost its handle to the child. The
// process was never waited on: it lingered holding its `aike-chrome-*` profile
// dir (observed: 3 such processes, all with dead debug ports, left behind by a
// batch containing launch failures).
//
// Reaching that path requires a stand-in "browser" that:
//   * writes DevToolsActivePort -> `wait_for_devtools_port` succeeds, so the
//     child IS handed back to the CDP connect step. With a plain long-lived
//     binary the endpoint wait times out first and the buggy path is never
//     reached, so the test would pass even against the broken build; and
//   * does NOT speak CDP        -> `CdpClient::connect` fails.
//
// The stand-in is a small native program compiled into %TEMP% at run time, so
// no non-.mjs file is ever added to tests/. It must be a single process, like
// chrome.exe: a .cmd wrapper would insert a cmd.exe layer whose grandchild
// survives killing the direct child, which is an artifact of the wrapper rather
// than a property of the module under test.

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

function sh(script) {
  const r = Aike.spawnSync("powershell", ["-NoProfile", "-Command", script]);
  return decode(r.stdout).trim();
}

// A stand-in that publishes DevToolsActivePort with an ephemeral port, accepts
// TCP connections there, and answers with plain HTTP so the WebSocket handshake
// fails. Single process, exactly like a real browser binary.
const STANDIN_SRC = `use std::io::Write;
use std::net::TcpListener;

fn main() {
    let dir = match std::env::args().find_map(|a| a.strip_prefix("--user-data-dir=").map(String::from)) {
        Some(d) => d,
        None => std::process::exit(2),
    };
    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(_) => std::process::exit(3),
    };
    let port = listener.local_addr().unwrap().port();
    let _ = std::fs::create_dir_all(&dir);
    if std::fs::write(
        std::path::Path::new(&dir).join("DevToolsActivePort"),
        format!("{port}\\n/devtools/browser/fake\\n"),
    )
    .is_err()
    {
        std::process::exit(5);
    }
    for stream in listener.incoming() {
        if let Ok(mut sock) = stream {
            let _ = sock.write_all(b"HTTP/1.1 500 Not a CDP endpoint\\r\\nContent-Length: 0\\r\\n\\r\\n");
            let _ = sock.flush();
        }
    }
}
`;

console.log("\n=== failed launch must not orphan the spawned process ===");

const rustc = Aike.which("rustc");
if (!rustc) {
  console.log("rustc not found on PATH -- SKIP");
  process.exit(0);
}

const tmp = process.env.TEMP;
const tag = "aike-regress-" + process.pid;
const src = tmp + "\\" + tag + ".rs";
const standin = tmp + "\\" + tag + ".exe";

// Generated outside tests/ so that directory keeps only *.mjs.
await fs.writeText(src, STANDIN_SRC);
const built = sh(
  "& '" + rustc.replace(/'/g, "''") + "' -O -o '" + standin.replace(/'/g, "''") +
    "' '" + src.replace(/'/g, "''") + "' 2>&1 | Out-String",
);
if (!(await fs.exists(standin))) {
  console.log("could not build the stand-in -- SKIP");
  console.log(built);
  process.exit(0);
}
console.log("  stand-in: " + standin);

// Count live stand-in processes (single process, no wrapper).
function orphanCount() {
  const n = parseInt(
    sh(
      "@(Get-CimInstance Win32_Process -Filter \"Name='" + tag + ".exe'\" | " +
        "Where-Object { $_.CommandLine -match 'aike-chrome-' }).Count",
    ),
    10,
  );
  return Number.isFinite(n) ? n : -1;
}

// Names of `aike-chrome-*` profile dirs created at/after `sinceMs` (epoch ms).
async function profileDirsCreatedAfter(sinceMs) {
  const out = sh(
    "$cut=[datetimeoffset]::FromUnixTimeMilliseconds(" + sinceMs + ").LocalDateTime; " +
      "Get-ChildItem $env:TEMP -Directory -Filter 'aike-chrome-*' " +
      "-ErrorAction SilentlyContinue | Where-Object { $_.CreationTime -ge $cut } | " +
      "Select-Object -ExpandProperty Name",
  );
  return out.split(/\r?\n/).map((s) => s.trim()).filter((s) => s.length > 0);
}

// Only dirs created from now on are attributable to this run.
const startedAt = Date.now();

const before = orphanCount();
console.log("  orphaned stand-ins before: " + before);

let threw = false;
let errMsg = "";
try {
  await Browser.launch({ headless: true, chromePath: standin, port: 0 });
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

// Wait for the failed launch's cleanup to settle. `Browser.launch` rejects as
// soon as the connect fails, while the module reaps the child and removes the
// profile dir; under load that finishes slightly later, so poll instead of
// sampling once (a single 3s snapshot produced false failures in a full-suite
// run on a busy machine).
async function waitForNoNewDirs(sinceMs, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let last = await profileDirsCreatedAfter(sinceMs);
  while (last.length > 0 && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 250));
    last = await profileDirsCreatedAfter(sinceMs);
  }
  return last;
}

const settled = await waitForNoNewDirs(startedAt, 20000);
console.log("  new profile dirs from this run: " + settled.length);
check(
  "failed launch removed the profile dir it created",
  settled.length === 0,
  "left behind: " + settled.join(", "),
);

const after = orphanCount();
console.log("  orphaned stand-ins after: " + after);
check(
  "no orphaned stand-in process left by the failed launch",
  after >= 0 && after <= before,
  "before=" + before + " after=" + after,
);

// Clean up the generated stand-in (rustc also emits a .pdb beside it).
for (const p of [src, standin, standin.replace(/\.exe$/, ".pdb")]) {
  try {
    await fs.remove(p);
  } catch (_) {}
}

console.log("\n" + "=".repeat(50));
console.log("pass: " + pass + ", fail: " + fail);
if (fail > 0) process.exit(1);
