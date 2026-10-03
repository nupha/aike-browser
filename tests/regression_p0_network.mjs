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

async function cookies(page) {
  return await page.evaluate("document.cookie");
}

async function hasCookie(page, name) {
  const jar = await cookies(page);
  return jar.indexOf(name + "=") >= 0;
}

// B-010: every attribute the JS cookie carries must reach Chrome.
async function testCookieAttributes(browser) {
  console.log("\n=== B-010: setCookie forwards every attribute ===");
  const page = await browser.newPage();
  page.onRequestPaused((evt) => {
    page.respondRequest(evt.requestId, {
      status: 200,
      body: "<!DOCTYPE html><p id='c'>cookies</p>",
      contentType: "text/html",
    });
  });
  await page.setRequestInterception(true);
  page.navigate(ORIGIN + "/cookies").catch(function () {});
  await page.waitForSelector("#c", 5000);
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
  await page.close();
}

// Network.overrideNetworkState (replacing the deprecated
// Network.emulateNetworkConditions) must actually flip navigator.onLine.
async function testOfflineMode(browser) {
  console.log("\n=== deprecated: Network.emulateNetworkConditions -> overrideNetworkState ===");
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

  await page.close();
}

// Storage.getStorageKey (replacing the deprecated Storage.getStorageKeyForFrame)
// must resolve a key for the main frame.
async function testStorageKey(browser) {
  console.log("\n=== deprecated: Storage.getStorageKeyForFrame -> getStorageKey ===");
  const page = await browser.newPage();
  page.onRequestPaused((evt) => {
    page.respondRequest(evt.requestId, {
      status: 200,
      body: "<!DOCTYPE html><p id='s'>storage</p>",
      contentType: "text/html",
    });
  });
  await page.setRequestInterception(true);
  page.navigate(ORIGIN + "/storage").catch(function () {});
  await page.waitForSelector("#s", 5000);

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
  await page.close();
}

// Browser.setDownloadBehavior replaces the deprecated Page.setDownloadBehavior:
// the page-level method is gone, the browser-level one still works.
async function testDownloadBehavior(browser) {
  console.log("\n=== deprecated: Page.setDownloadBehavior -> Browser.setDownloadBehavior ===");
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
  await page.close();
}

// Security.setIgnoreCertificateErrors replaces the deprecated
// Security.handleCertificateError (whose `action` enum was even out of range).
async function testCertificateErrors(browser) {
  console.log("\n=== deprecated: Security.handleCertificateError -> setIgnoreCertificateErrors ===");
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
  if (fail > 0) throw new Error("regression_p0_network: " + fail + " check(s) failed");
}

await main();