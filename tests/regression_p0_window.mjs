// Regression test: the P0 window-method fixes.
//
//   B-011 `Browser.getWindowForTarget.windowId` is an integer, but the code
//         read it with `as_str()`, so `getWindowBounds` / `setWindowBounds`
//         always failed with "no windowId".
//   B-012 `Browser.setWindowBounds.windowState` was forwarded unvalidated; an
//         unknown state is not merely rejected by Chrome — it aborts the whole
//         browser process, taking every page with it.
//
//   aike -m tests/regression_p0_window.mjs

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

// Headless Chrome has no real window, so the bounds calls are only asserted
// when Chrome answers them; the distinction the test cannot lose is that the
// failure must NOT be our own "no windowId" bookkeeping error.
const NO_WINDOW_ID = "no windowId";

async function testWindowBounds(browser) {
  console.log("\n=== B-011: window bounds ===");
  let bounds = null;
  let error = null;
  try {
    bounds = await browser.getWindowBounds();
  } catch (e) {
    error = e && (e.message || e);
  }
  if (error) {
    check("getWindowBounds() does not fail on windowId bookkeeping",
      error.indexOf(NO_WINDOW_ID) < 0, "error=" + error);
    console.log("  [skip] no window in this build: " + error);
  } else {
    check("getWindowBounds() returns bounds", bounds !== null && typeof bounds === "object",
      "bounds=" + JSON.stringify(bounds));
    check("bounds carry width/height",
      bounds && bounds.bounds && typeof bounds.bounds.width === "number",
      "bounds=" + JSON.stringify(bounds));
  }

  let resizeError = null;
  try {
    await browser.setWindowBounds({ width: 800, height: 600 });
  } catch (e) {
    resizeError = e && (e.message || e);
  }
  check("setWindowBounds() does not fail on windowId bookkeeping",
    resizeError === null || resizeError.indexOf(NO_WINDOW_ID) < 0, "error=" + resizeError);
}

async function testWindowStateValidation(browser) {
  console.log("\n=== B-012: windowState is validated ===");
  const page = await browser.newPage();
  await page.navigate("data:text/html,<p id='w'>window</p>");
  await page.waitForSelector("#w", 5000);

  let unknownState = null;
  try {
    await browser.setWindowBounds({ windowState: "Maximised" });
  } catch (e) {
    unknownState = e && (e.message || e);
  }
  check("an unknown windowState throws", typeof unknownState === "string",
    "error=" + unknownState);
  check("the error lists the legal states",
    typeof unknownState === "string"
      && unknownState.indexOf("normal") >= 0
      && unknownState.indexOf("fullscreen") >= 0,
    "error=" + unknownState);
  check("the error names the offending value",
    typeof unknownState === "string" && unknownState.indexOf("Maximised") >= 0,
    "error=" + unknownState);

  // The process must still be alive: sending the bad state to Chrome would have
  // killed it, taking this page with it.
  const alive = await page.evaluate("document.getElementById('w').textContent");
  check("the browser survived the rejected windowState", alive === "window",
    "body=" + JSON.stringify(alive));

  // A legal state must be accepted (it may still be refused by a headless
  // build that has no window — that is Chrome's business, not ours).
  let accepted = null;
  try {
    await browser.setWindowBounds({ windowState: "normal" });
  } catch (e) {
    accepted = e && (e.message || e);
  }
  check("a legal windowState is not rejected by validation",
    accepted === null || accepted.indexOf("windowState") < 0, "error=" + accepted);

  check("the page still works after setWindowBounds", browser.isRunning === true);
  await page.close();
}

async function main() {
  const browser = await Browser.launch({ headless: true });
  check("Browser launched", browser.isRunning === true);

  const tests = [testWindowBounds, testWindowStateValidation];
  for (const fn of tests) {
    try {
      await fn(browser);
    } catch (e) {
      console.log("  [FAIL] " + fn.name + " threw: " + (e && (e.message || e)));
      fail++;
    }
  }

  try { await browser.close(); } catch (_) {}

  console.log("\n" + "=".repeat(50));
  console.log("pass: " + pass + ", fail: " + fail);
  if (fail > 0) throw new Error("regression_p0_window: " + fail + " check(s) failed");
}

await main();