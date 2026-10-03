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
// The stand-in is a small native program compiled into the temp dir at run
// time, so no non-.mjs file is ever added to tests/. It must be a single
// process, like chrome.exe: a shell/batch wrapper would insert an extra
// interpreter layer whose grandchild survives killing the direct child, which
// is an artifact of the wrapper rather than a property of the module under test.
//
// Liveness is probed through a heartbeat file the stand-in rewrites every tick,
// and the profile dirs are located with `os.tmpdir()` + `fs.readDir` — the same
// lookup the module itself uses — so this runs unchanged on Windows, macOS and
// Linux (no shell, no `Win32_Process`, no platform path separators).

import { Browser } from "browser";
import * as fs from "fs";
import * as os from "os";
import * as path from "path";

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

const tmp = os.tmpdir();
const isWindows = os.platform() === "win32";

// A Rust string literal for `s`; temp paths carry backslashes on Windows.
function rustStr(s) {
  return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
}

// A stand-in that publishes DevToolsActivePort with an ephemeral port, accepts
// TCP connections there, answers with plain HTTP so the WebSocket handshake
// fails, and beats a heartbeat file for as long as it lives. Single process,
// exactly like a real browser binary.
const standinSrc = (beat) => `use std::io::Write;
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
    std::thread::spawn(|| {
        let mut n: u64 = 0;
        loop {
            n += 1;
            let _ = std::fs::write(${JSON.stringify(rustStr(beat))}, n.to_string());
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    });
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

const tag = "aike-regress-" + process.pid;
const beat = path.join(tmp, tag + ".beat");
const src = path.join(tmp, tag + ".rs");
const standin = path.join(tmp, tag + (isWindows ? ".exe" : ""));

// Generated outside tests/ so that directory keeps only *.mjs.
await fs.writeText(src, standinSrc(beat));
const built = Aike.spawnSync(rustc, ["-O", "-o", standin, src]);
if (!(await fs.exists(standin))) {
  console.log("could not build the stand-in -- SKIP");
  console.log(new TextDecoder().decode(built.stderr));
  process.exit(0);
}
console.log("  stand-in: " + standin);

// Read the heartbeat; a rising counter means the stand-in is still running.
async function beatCount() {
  try {
    return Number(await fs.readText(beat));
  } catch (_) {
    return 0; // no beat yet, or the file is gone
  }
}

// True once the stand-in is demonstrably not running any more: either its
// heartbeat stopped advancing, or it never beat at all.
async function standInExited() {
  const a = await beatCount();
  await new Promise((r) => setTimeout(r, 1500));
  const b = await beatCount();
  return b === 0 || b === a;
}

async function profileDirs() {
  const names = [];
  for (const e of await fs.readDir(tmp)) {
    if (e.isDirectory && e.name.startsWith("aike-chrome-")) names.push(e.name);
  }
  return names;
}

// Only dirs this run creates are attributable to it, so diff against a
// snapshot instead of counting (leftovers from earlier runs must not fail us).
const before = new Set(await profileDirs());

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
async function waitForNoNewDirs(timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let leaked = (await profileDirs()).filter((n) => !before.has(n));
  while (leaked.length > 0 && Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 250));
    leaked = (await profileDirs()).filter((n) => !before.has(n));
  }
  return leaked;
}

const settled = await waitForNoNewDirs(20000);
console.log("  new profile dirs from this run: " + settled.length);
check(
  "failed launch removed the profile dir it created",
  settled.length === 0,
  "left behind: " + settled.join(", "),
);

const exited = await standInExited();
check(
  "no orphaned stand-in process left by the failed launch",
  exited,
  "heartbeat still advancing: " + (await beatCount()),
);

// Clean up the generated stand-in (rustc also emits a .pdb beside it on Windows).
for (const p of [src, standin, path.join(tmp, tag + ".pdb"), beat]) {
  try {
    await fs.remove(p);
  } catch (_) {}
}

console.log("\n" + "=".repeat(50));
console.log("pass: " + pass + ", fail: " + fail);
if (fail > 0) process.exit(1);
