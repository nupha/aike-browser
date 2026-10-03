// Regression test: B-001 — registering a native event listener must not keep
// the process alive.
//
// `onLog`, `onConsole` and `onRequestPaused` each install an event-loop polling
// job. A job that re-registers as "keep polling" makes every event-loop round
// report "parked", so the driver's wait loop never reaches idle and the process
// never exits — the script below would hang forever after its last line.
//
// The assertion is therefore structural: this file ends here, and nothing in it
// forces an exit. A build that reintroduces the hold fails by timing out.
// Every listener is also shown to actually deliver, so a build that simply
// fails to install them cannot pass by accident.
//
//   aike -m tests/regression_p0_event_exit.mjs

import { Browser } from "browser";

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

// A stalled CDP round-trip means "not ready", not "broken": Chrome occasionally
// leaves `Runtime.evaluate` unanswered for its full timeout while a navigation
// is settling. Keep polling instead of failing on it.
async function poll(page, fn, tries = 80) {
  let v;
  for (let i = 0; i < tries; i++) {
    try {
      v = await fn();
    } catch (_) {
      v = null;
    }
    if (v) return v;
    // Another round-trip is another event-loop tick for the native event job.
    try {
      await page.evaluate("0");
    } catch (_) {}
  }
  return v;
}

const ORIGIN = "http://localhost:39217";

async function main() {
  const browser = await Browser.launch({ headless: true });
  const page = await browser.newPage();
  await page.navigate("data:text/html,<p id='e'>events</p>");
  await page.waitForSelector("#e", 5000);

  // onConsole / onLog.
  let consoleMsg = null;
  page.onConsole((msg) => {
    consoleMsg = msg;
  });
  page.onLog((msg) => {
    if (consoleMsg === null) consoleMsg = msg;
  });
  await page.enableRuntime();
  await page.evaluate("console.log('listener-alive')");
  const delivered = await poll(page, async () => consoleMsg !== null);
  check("onConsole/onLog delivered an event", delivered === true);
  const buffered = await page.getConsoleMessages();
  check("getConsoleMessages() buffered the entry",
    Array.isArray(buffered) && buffered.length >= 1, "len=" + (buffered ? buffered.length : "n/a"));

  // onRequestPaused.
  let paused = 0;
  page.onRequestPaused((evt) => {
    paused++;
    page.respondRequest(evt.requestId, {
      status: 200,
      body: "<!DOCTYPE html><p id='i'>intercepted</p>",
      contentType: "text/html",
    });
  });
  await page.setRequestInterception(true);
  page.navigate(ORIGIN + "/interception").catch(function () {});
  await page.waitForSelector("#i", 10000);
  check("onRequestPaused delivered an event", paused >= 1, "paused=" + paused);
  await page.setRequestInterception(false);

  // B-087: a non-function callback must be rejected at registration time.
  // Registering one used to succeed and then silently drop every event, so the
  // listener looked installed and never fired. Argument validation throws
  // synchronously (standard JS behaviour), hence try/catch, not await.
  const bad = [
    ["onConsole", () => page.onConsole(42)],
    ["onLog", () => page.onLog(42)],
    ["onRequestPaused", () => page.onRequestPaused(42)],
    ["handleCertificateError", () => browser.handleCertificateError("nope")],
  ];
  for (const [name, call] of bad) {
    let threw = null;
    try {
      call();
    } catch (e) {
      threw = e;
    }
    check(name + "(non-function) throws", threw instanceof TypeError,
      threw === null ? "no throw" : String(threw));
  }

  try { await page.close(); } catch (_) {}
  try { await browser.close(); } catch (_) {}

  console.log("\n" + "=".repeat(50));
  console.log("pass: " + pass + ", fail: " + fail);
  if (fail > 0) throw new Error("regression_p0_event_exit: " + fail + " check(s) failed");
  console.log("script finished — the process must now exit on its own");
}

await main();