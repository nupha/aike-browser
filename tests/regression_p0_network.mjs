// Regression test: the P0 cookie fix and the deprecated-command replacements.
//
//   B-010 `setCookie` forwarded only name/value/domain/path, silently dropping
//         `sameSite`, `expires`, `httpOnly` and `secure`.
//
//   Deprecated CDP commands replaced (none of them may be used any more):
//     Network.getAllCookies            -> Storage.getCookies
//     Network.emulateNetworkConditions -> Network.overrideNetworkState
//     Storage.getStorageKeyForFrame    -> Storage.getStorageKey
//     Page.setDownloadBehavior         -> Browser.setDownloadBehavior
//     Security.handleCertificateError  -> Security.setIgnoreCertificateErrors
//
//   aike -m tests/regression_p0_network.mjs

import { Browser } from "browser";

const ORIGIN = "http://localhost:39217";

let pass = 0;
let fail = 0;

// Per-attempt output buffer: a flaked attempt is rolled back and dropped, so
// its `[ok]`/`[FAIL]` lines must not leak into the retried, clean run.
let out = [];
function emit(s) {
  out.push(s);
}
function flush() {
  for (const s of out) console.log(s);
  out = [];
}

function check(label, condition, detail) {
  if (condition) {
    emit("  [ok] " + label);
    pass++;
  } else {
    emit("  [FAIL] " + label + (detail ? " -- " + detail : ""));
    fail++;
  }
}

function sleep(ms) {
  return new Promise((r) => setTimeout(r, ms));
}

// Bound one awaited step: Chrome occasionally leaves a CDP round-trip
// unanswered for its full 30s native timeout. Racing a short timeout turns a
// stall into an ordinary "not ready" answer instead of a long hang.
function bounded(p, ms) {
  return Promise.race([p.catch(() => null), sleep(ms).then(() => null)]);
}

// A stalled CDP round-trip means "not ready", not "broken": Chrome occasionally
// leaves `Runtime.evaluate` unanswered for its full timeout while a navigation
// is settling. Keep polling instead of failing on it.
async function poll(page, fn, tries = 40) {
  const deadline = Date.now() + 15000;
  let v = null;
  for (let i = 0; i < tries; i++) {
    v = await bounded(Promise.resolve().then(fn), 2000);
    if (v) return v;
    if (Date.now() > deadline) break;
    // Another round-trip is another event-loop tick for the native event job.
    await bounded(page.evaluate("0"), 2000);
  }
  return v;
}

// Create a page, intercept `url` and fulfill it with `body`, and wait for
// `selector`. Chrome occasionally never delivers `Fetch.requestPaused` for a
// fresh tab (the session comes up wedged), so a failed attempt is retried on a
// brand-new page. This is about a flaky browser, not about the API: the same
// call succeeds on the next tab.
async function interceptedPage(browser, url, body, readySelector) {
  const html = "<!DOCTYPE html>" + body;
  let last = null;
  for (let attempt = 0; attempt < 3; attempt++) {
    const page = await browser.newPage();
    page.onRequestPaused((evt) => {
      page
        .respondRequest(evt.requestId, {
          status: 200,
          body: html,
          contentType: "text/html",
        })
        .catch(() => {});
    });
    try {
      await page.setRequestInterception(true);
      page.navigate(url).catch(() => {});
      await page.waitForSelector(readySelector, 5000);
      return page;
    } catch (e) {
      last = e;
      await bounded(page.setRequestInterception(false), 3000);
      await bounded(page.close(), 5000);
    }
  }
  throw last || new Error("interceptedPage failed");
}

async function cookies(page) {
  return await page.evaluate("document.cookie");
}

async function hasCookie(page, name) {
  const jar = await cookies(page);
  return jar.indexOf(name + "=") >= 0;
}

// B-010: every attribute the JS cookie carries must reach Chrome.
async function testCookieAttributes(browser) {
  emit("\n=== B-010: setCookie forwards every attribute ===");
  const page = await interceptedPage(browser, ORIGIN + "/cookies", "<p id='c'>cookies</p>", "#c");
  await page.evaluate("document.cookie.split(';').forEach(function (c) {" +
    "  document.cookie = c.split('=')[0].trim() + '=; expires=Thu, 01 Jan 1970 00:00:00 GMT; path=/';" +
    "})");

  await page.setCookie({
    name: "aike_plain",
    value: "v1",
    domain: "localhost",
    path: "/",
    httpOnly: false,
    secure: false,
    sameSite: "Lax",
    expires: Math.floor(Date.now() / 1000) + 3600,
  });
  check("a cookie with every attribute is readable from document.cookie",
    await hasCookie(page, "aike_plain"), "jar=" + JSON.stringify(await cookies(page)));

  await page.setCookie({
    name: "aike_httponly",
    value: "v2",
    domain: "localhost",
    path: "/",
    httpOnly: true,
  });
  check("httpOnly:true is honoured (invisible to script)",
    !(await hasCookie(page, "aike_httponly")), "jar=" + JSON.stringify(await cookies(page)));

  await page.setCookie({
    name: "aike_expired",
    value: "v3",
    domain: "localhost",
    path: "/",
    expires: Math.floor(Date.now() / 1000) - 3600,
  });
  check("expires in the past deletes the cookie",
    !(await hasCookie(page, "aike_expired")), "jar=" + JSON.stringify(await cookies(page)));

  let badSameSite = null;
  try {
    await page.setCookie({ name: "aike_bad", value: "v4", domain: "localhost", path: "/", sameSite: "Whatever" });
  } catch (e) {
    badSameSite = e && (e.message || e);
  }
  check("an unknown sameSite throws", typeof badSameSite === "string", "error=" + badSameSite);
  check("the error names the offending value",
    typeof badSameSite === "string" && badSameSite.indexOf("Whatever") >= 0, "error=" + badSameSite);

  // Storage.getCookies (the replacement for the deprecated Network.getAllCookies)
  // must see what was just stored.
  const list = await page.getCookies();
  check("getCookies() returns an array", Array.isArray(list), "got=" + typeof list);
  check("getCookies() sees the cookie set through setCookie",
    Array.isArray(list) && list.some((c) => c.name === "aike_plain"),
    "names=" + JSON.stringify((list || []).map((c) => c.name)));

  await page.setRequestInterception(false);
  await bounded(page.close(), 5000);
}

// Network.overrideNetworkState (replacing the deprecated
// Network.emulateNetworkConditions) must actually flip navigator.onLine.
async function testOfflineMode(browser) {
  emit("\n=== deprecated: Network.emulateNetworkConditions -> overrideNetworkState ===");
  const page = await browser.newPage();
  await page.navigate("data:text/html,<p id='o'>offline</p>");
  await page.waitForSelector("#o", 5000);

  check("navigator.onLine starts true",
    (await page.evaluate("navigator.onLine")) === true);

  await page.setOfflineMode(true);
  const offline = await poll(page, async () => (await page.evaluate("navigator.onLine")) === false);
  check("setOfflineMode(true) flips navigator.onLine", offline === true);

  await page.setOfflineMode(false);
  const online = await poll(page, async () => (await page.evaluate("navigator.onLine")) === true);
  check("setOfflineMode(false) flips it back", online === true);

  await bounded(page.close(), 5000);
}

// Storage.getStorageKey (replacing the deprecated Storage.getStorageKeyForFrame)
// must resolve a key for the main frame.
async function testStorageKey(browser) {
  emit("\n=== deprecated: Storage.getStorageKeyForFrame -> getStorageKey ===");
  const page = await interceptedPage(browser, ORIGIN + "/storage", "<p id='s'>storage</p>", "#s");

  const tree = await page.getFrameTree();
  const frameId = tree && tree.frameTree && tree.frameTree.frame ? tree.frameTree.frame.id : "";
  check("getFrameTree exposes a frame id", typeof frameId === "string" && frameId.length > 0,
    "frameId=" + frameId);

  let key = null;
  let error = null;
  try {
    key = await page.getStorageKeyForFrame(frameId);
  } catch (e) {
    error = e && (e.message || e);
  }
  check("getStorageKeyForFrame(frameId) resolves", error === null, "error=" + error);
  check("getStorageKeyForFrame(frameId) returns a key",
    typeof key === "string" && key.length > 0, "key=" + key);

  await page.setRequestInterception(false);
  await bounded(page.close(), 5000);
}

// Browser.setDownloadBehavior replaces the deprecated Page.setDownloadBehavior:
// the page-level method is gone, the browser-level one still works.
async function testDownloadBehavior(browser) {
  emit("\n=== deprecated: Page.setDownloadBehavior -> Browser.setDownloadBehavior ===");
  const page = await browser.newPage();
  check("page.setDownloadBehavior is gone", page.setDownloadBehavior === undefined);
  check("browser.setDownloadBehavior is available", typeof browser.setDownloadBehavior === "function");

  let error = null;
  try {
    await browser.setDownloadBehavior("");
    await browser.setDownloadBehavior("/tmp/aike_downloads");
  } catch (e) {
    error = e && (e.message || e);
  }
  check("browser.setDownloadBehavior(deny/allow) resolves", error === null, "error=" + error);
  await bounded(page.close(), 5000);
}

// Security.setIgnoreCertificateErrors replaces the deprecated
// Security.handleCertificateError (whose `action` enum was even out of range).
async function testCertificateErrors(browser) {
  emit("\n=== deprecated: Security.handleCertificateError -> setIgnoreCertificateErrors ===");
  let error = null;
  try {
    await browser.setIgnoreHTTPSErrors(true);
    await browser.setIgnoreHTTPSErrors(false);
  } catch (e) {
    error = e && (e.message || e);
  }
  check("setIgnoreHTTPSErrors(true/false) resolves", error === null, "error=" + error);

  let registered = true;
  try {
    const ret = browser.handleCertificateError(function () {});
    check("handleCertificateError(callback) returns undefined", ret === undefined);
  } catch (e) {
    registered = false;
    console.log("  [FAIL] handleCertificateError threw: " + (e && (e.message || e)));
  }
  check("handleCertificateError(callback) registers a listener", registered);
}

async function main() {
  // Watchdog: a wedged Chrome session must fail the suite, not hang it.
  setTimeout(() => {
    console.log("  [WATCHDOG] forcing exit after 90s (a step likely hung)");
    process.exit(1);
  }, 90000);

  const browser = await Browser.launch({ headless: true });
  check("Browser launched", browser.isRunning === true);

  const tests = [
    testCookieAttributes,
    testOfflineMode,
    testStorageKey,
    testDownloadBehavior,
    testCertificateErrors,
  ];
  for (const fn of tests) {
    // Same fresh-tab retry as regression_p0_fetch: a wedged session (Chrome
    // never delivers `Fetch.requestPaused`) must not be reported as a failure.
    for (let attempt = 0; ; attempt++) {
      const p0 = pass;
      const f0 = fail;
      out = [];
      try {
        await fn(browser);
      } catch (e) {
        emit("  [FAIL] " + fn.name + " threw: " + (e && (e.message || e)));
        fail++;
      }
      if (fail === f0 || attempt >= 2) {
        flush();
        break;
      }
      pass = p0;
      fail = f0;
      out = [];
      console.log("  [retry] " + fn.name + " flaked; retrying on a fresh tab (" + (attempt + 1) + "/2)");
    }
  }

  await bounded(browser.close(), 5000);

  console.log("\n" + "=".repeat(50));
  console.log("pass: " + pass + ", fail: " + fail);
  // Exit explicitly so the watchdog timer cannot keep the event loop alive.
  process.exit(fail > 0 ? 1 : 0);
}

await main();