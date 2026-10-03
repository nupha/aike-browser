// Regression test: the P3 strict-argument fixes (B-073 ~ B-090, except B-087).
//
// Every method must reject a parameter that does not match its declared type,
// and must not silently coerce, clamp, truncate or default a value:
//
//   B-073 `coerce_string(undefined)` -> "undefined"
//   B-074 `arg_bool` applied `ToBoolean` (`setOfflineMode("yes")` went offline)
//   B-075 `setViewport` clamped/truncated/saturated the dimensions
//   B-076 `setGeolocation` defaulted the coordinates to `{0,0,1}`
//   B-077 `setCPUThrottling` accepted `0` / `-1` / omitted
//   B-078 `setMediaFeatures` turned a non-array into "clear every feature"
//   B-079 `port` went through `ToNumber` + `as u16` saturation
//   B-080 `setWindowBounds` truncated floats and mixed geometry with a state
//   B-081 `captureScreenshot.quality` had no range check and applied to PNG
//   B-082 backend node ids: `NaN` -> 0, negative -> `u64::MAX`, float truncated
//   B-083 `mouseClick.button` unvalidated, `clickCount` truncated
//   B-084 `disposeContext()` / `deleteCookie()` silently sent empty strings
//   B-085 `newPage(42)` / `newPage(null)` silently opened about:blank
//   B-086 `setExtraHTTPHeaders` silently cleared headers / sent null
//   B-088 `dragAndDrop()` dragged the root onto itself
//   B-089 `spawn({ args: [] })` entered custom-command-line mode
//   B-090 `cdpUrl` with a query was probed at the wrong address
//
//   aike -m tests/regression_p3_args.mjs

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

// A synchronous native argument error throws from the call itself.
function callThrows(label, fn, needle) {
  let threw = false;
  let msg = "";
  try {
    const r = fn();
    if (r && typeof r.then === "function") r.catch(() => {});
  } catch (e) {
    threw = true;
    msg = String((e && e.message) || e);
  }
  check(label, threw && (!needle || msg.indexOf(needle) >= 0),
    "threw=" + threw + " msg=" + JSON.stringify(msg));
}

// Validation that lives inside the async work rejects the returned promise.
async function callRejects(label, fn, needle) {
  let rejected = false;
  let msg = "";
  try {
    await fn();
  } catch (e) {
    rejected = true;
    msg = String((e && e.message) || e);
  }
  check(label, rejected && (!needle || msg.indexOf(needle) >= 0),
    "rejected=" + rejected + " msg=" + JSON.stringify(msg));
}

async function callResolves(label, fn, detail) {
  try {
    const v = await fn();
    check(label, true, detail);
    return v;
  } catch (e) {
    check(label, false, "rejected: " + String((e && e.message) || e));
    return undefined;
  }
}

// ---------------------------------------------------------------------------

function testStringArgs(page) {
  console.log("\n=== B-073: required strings are not coerced ===");
  callThrows("setUserAgent() throws", () => page.setUserAgent(), "userAgent");
  callThrows("setUserAgent(undefined) throws", () => page.setUserAgent(undefined), "userAgent");
  callThrows("setUserAgent(42) throws", () => page.setUserAgent(42), "userAgent");
  callThrows("navigate() throws", () => page.navigate(), "url");
  callThrows("evaluate() throws", () => page.evaluate(), "jsCode");
  callThrows("querySelector() throws", () => page.querySelector(), "selector");
  return callResolves("setUserAgent('aike-ua/p3') resolves",
    () => page.setUserAgent("aike-ua/p3"));
}

async function testBoolArgs(page) {
  console.log("\n=== B-074: booleans are not ToBoolean'd ===");
  callThrows("setOfflineMode('yes') throws", () => page.setOfflineMode("yes"), "enabled");
  callThrows("setOfflineMode(1) throws", () => page.setOfflineMode(1), "enabled");
  callThrows("setOfflineMode(null) throws", () => page.setOfflineMode(null), "enabled");
  await callResolves("setOfflineMode(false) resolves", () => page.setOfflineMode(false));
}

function testViewport(page) {
  console.log("\n=== B-075: setViewport dimensions ===");
  callThrows("setViewport(-1, 10) throws", () => page.setViewport(-1, 10), "width");
  callThrows("setViewport('abc', 10) throws", () => page.setViewport("abc", 10), "width");
  callThrows("setViewport(1.5, 10) throws", () => page.setViewport(1.5, 10), "width");
  callThrows("setViewport(1e12, 10) throws", () => page.setViewport(1e12, 10), "width");
  callThrows("setViewport(10, 0) throws", () => page.setViewport(10, 0), "height");
  return callResolves("setViewport(800, 600) resolves", () => page.setViewport(800, 600));
}

function testGeolocation(page) {
  console.log("\n=== B-076: setGeolocation ===");
  callThrows("setGeolocation() throws", () => page.setGeolocation(), "latitude");
  callThrows("setGeolocation('a', 'b') throws", () => page.setGeolocation("a", "b"), "latitude");
  callThrows("setGeolocation(91, 0) throws", () => page.setGeolocation(91, 0), "latitude");
  callThrows("setGeolocation(0, 181) throws", () => page.setGeolocation(0, 181), "longitude");
  callThrows("setGeolocation(0, 0, 0) throws", () => page.setGeolocation(0, 0, 0), "accuracy");
  return callResolves("setGeolocation(1, 2, 10) resolves", () => page.setGeolocation(1, 2, 10));
}

function testCPUThrottling(page) {
  console.log("\n=== B-077: setCPUThrottling ===");
  callThrows("setCPUThrottling() throws", () => page.setCPUThrottling(), "rate");
  callThrows("setCPUThrottling(0) throws", () => page.setCPUThrottling(0), "rate");
  callThrows("setCPUThrottling(-1) throws", () => page.setCPUThrottling(-1), "rate");
  callThrows("setCPUThrottling('2') throws", () => page.setCPUThrottling("2"), "rate");
  return callResolves("setCPUThrottling(2) resolves", () => page.setCPUThrottling(2));
}

async function testMediaFeatures(page) {
  console.log("\n=== B-078: setMediaFeatures ===");
  callThrows("setMediaFeatures('x') throws", () => page.setMediaFeatures("x"), "features");
  callThrows("setMediaFeatures() throws", () => page.setMediaFeatures(), "features");
  callThrows("setMediaFeatures([{name:'x'}]) throws",
    () => page.setMediaFeatures([{ name: "x" }]), "value");
  callThrows("setMediaFeatures([{name:1,value:'v'}]) throws",
    () => page.setMediaFeatures([{ name: 1, value: "v" }]), "name");
  await callResolves("setMediaFeatures([]) resolves", () => page.setMediaFeatures([]));
  await callResolves("setMediaFeatures([...]) resolves",
    () => page.setMediaFeatures([{ name: "prefers-color-scheme", value: "dark" }]));
}

function testPortValidation() {
  console.log("\n=== B-079: port ===");
  callThrows("launch({port:-1}) throws", () => Browser.launch({ port: -1 }), "port");
  callThrows("launch({port:70000}) throws", () => Browser.launch({ port: 70000 }), "port");
  callThrows("spawn({port:1.5}) throws", () => Browser.spawn({ port: 1.5 }), "port");
  callThrows("spawn({port:'9222'}) throws", () => Browser.spawn({ port: "9222" }), "port");
}

function testWindowBounds(browser) {
  console.log("\n=== B-080: setWindowBounds ===");
  callThrows("setWindowBounds({width:2.7}) throws",
    () => browser.setWindowBounds({ width: 2.7 }), "width");
  callThrows("setWindowBounds({width:'300'}) throws",
    () => browser.setWindowBounds({ width: "300" }), "width");
  callThrows("setWindowBounds(null) throws",
    () => browser.setWindowBounds(null), "bounds");
  callThrows("setWindowBounds() throws",
    () => browser.setWindowBounds(), "bounds");
  callThrows("setWindowBounds geometry + non-normal state throws",
    () => browser.setWindowBounds({ windowState: "maximized", width: 100 }), "windowState");
}

function testScreenshot(page) {
  console.log("\n=== B-081: captureScreenshot quality / format ===");
  callThrows("quality:300 throws", () => page.captureScreenshot({ quality: 300 }), "quality");
  callThrows("quality:0 throws", () => page.captureScreenshot({ quality: 0 }), "quality");
  callThrows("quality:1.9 throws", () => page.captureScreenshot({ quality: 1.9 }), "quality");
  callThrows("quality with png throws",
    () => page.captureScreenshot({ format: "png", quality: 50 }), "quality");
  callThrows("format:'gif' throws", () => page.captureScreenshot({ format: "gif" }), "format");
  return callResolves("format:jpeg + quality:60 resolves",
    () => page.captureScreenshot({ format: "jpeg", quality: 60 }));
}

function testBackendNodes(page) {
  console.log("\n=== B-082: backend node ids ===");
  callThrows("clickBackendNode() throws", () => page.clickBackendNode(), "backendNodeId");
  callThrows("clickBackendNode(-1) throws", () => page.clickBackendNode(-1), "backendNodeId");
  callThrows("clickBackendNode(1.5) throws", () => page.clickBackendNode(1.5), "backendNodeId");
  callThrows("clickBackendNode(NaN) throws", () => page.clickBackendNode(NaN), "backendNodeId");
  callThrows("typeBackendNode(0,'x') throws", () => page.typeBackendNode(0, "x"), "backendNodeId");
}

async function testClickOptions(page) {
  console.log("\n=== B-083: mouseClick options ===");
  callThrows("button:'x' throws",
    () => page.mouseClick(1, 2, { button: "x" }), "button");
  callThrows("clickCount:0 throws",
    () => page.mouseClick(1, 2, { clickCount: 0 }), "clickCount");
  callThrows("clickCount:1.5 throws",
    () => page.mouseClick(1, 2, { clickCount: 1.5 }), "clickCount");
  callThrows("clickCount:null throws",
    () => page.mouseClick(1, 2, { clickCount: null }), "clickCount");
  callThrows("x missing throws", () => page.mouseClick(), "x");
  await callResolves("mouseClick with valid options resolves",
    () => page.mouseClick(1, 2, { button: "left", clickCount: 1 }));
}

function testRequiredParams(browser, page) {
  console.log("\n=== B-084: required params are not defaulted ===");
  callThrows("disposeContext() throws", () => browser.disposeContext(), "contextId");
  callThrows("disposeContext(null) throws", () => browser.disposeContext(null), "contextId");
  callThrows("deleteCookie() throws", () => page.deleteCookie(), "name");
  callThrows("getResponseBody() throws", () => page.getResponseBody(), "requestId");
  callThrows("getRequestPostData() throws", () => page.getRequestPostData(), "requestId");
}

async function testNewPage(browser) {
  console.log("\n=== B-085: newPage argument ===");
  callThrows("newPage(42) throws", () => browser.newPage(42), "urlOrOpts");
  callThrows("newPage(null) throws", () => browser.newPage(null), "urlOrOpts");
  callThrows("newPage({contextId:''}) throws",
    () => browser.newPage({ contextId: "" }), "contextId");
  const p = await callResolves("newPage('about:blank') resolves",
    () => browser.newPage("about:blank"));
  check("newPage('about:blank') returns a Page", p && typeof p.targetId === "string");
  if (p) await p.close();
}

async function testExtraHeaders(page) {
  console.log("\n=== B-086: setExtraHTTPHeaders ===");
  callThrows("setExtraHTTPHeaders('a: b') throws",
    () => page.setExtraHTTPHeaders("a: b"), "headers");
  callThrows("setExtraHTTPHeaders({a:1}) throws",
    () => page.setExtraHTTPHeaders({ a: 1 }), "headers");
  callThrows("setExtraHTTPHeaders() throws",
    () => page.setExtraHTTPHeaders(), "headers");
  await callResolves("setExtraHTTPHeaders({}) resolves", () => page.setExtraHTTPHeaders({}));
  await callResolves("setExtraHTTPHeaders({X-Test:'1'}) resolves",
    () => page.setExtraHTTPHeaders({ "X-Test": "1" }));
}

function testDragAndDrop(page) {
  console.log("\n=== B-088: dragAndDrop ===");
  callThrows("dragAndDrop() throws", () => page.dragAndDrop(), "sourceSelector");
  callThrows("dragAndDrop('#a') throws", () => page.dragAndDrop("#a"), "targetSelector");
}

async function testSpawnEmptyArgs() {
  console.log("\n=== B-089: spawn({ args: [] }) ===");
  let proc = null;
  let err = null;
  try {
    proc = await Browser.spawn({ args: [] });
  } catch (e) {
    err = String((e && e.message) || e);
  }
  check("spawn({args:[]}) is treated as auto mode", proc !== null && proc.pid !== null,
    "err=" + err);
  if (proc) {
    try { await proc.kill(); } catch (_) {}
  }
}

async function testConnectUrl() {
  console.log("\n=== B-090: cdpUrl parsing ===");
  await callRejects("connect('ws://127.0.0.1:1?x') rejects",
    () => Browser.connect("ws://127.0.0.1:1?x"), "valid WebSocket URL");
  await callRejects("connect('http://127.0.0.1:1') rejects",
    () => Browser.connect("http://127.0.0.1:1"), "ws://");
  await callRejects("connect('ws://') rejects", () => Browser.connect("ws://"), "no host");
}

// ---------------------------------------------------------------------------

async function main() {
  const browser = await Browser.launch({ headless: true });
  check("Browser launched", browser.isRunning === true);

  const page = await browser.newPage();
  await page.navigate("data:text/html,<p id='p3'>p3</p>");
  await page.waitForSelector("#p3", 5000);

  const tests = [
    () => testStringArgs(page),
    () => testBoolArgs(page),
    () => testViewport(page),
    () => testGeolocation(page),
    () => testCPUThrottling(page),
    () => testMediaFeatures(page),
    () => testPortValidation(),
    () => testWindowBounds(browser),
    () => testScreenshot(page),
    () => testBackendNodes(page),
    () => testClickOptions(page),
    () => testRequiredParams(browser, page),
    () => testNewPage(browser),
    () => testExtraHeaders(page),
    () => testDragAndDrop(page),
    () => testSpawnEmptyArgs(),
    () => testConnectUrl(),
  ];

  for (const fn of tests) {
    try {
      await fn();
    } catch (e) {
      console.log("  [FAIL] " + fn.name + " threw: " + (e && (e.message || e)));
      fail++;
    }
  }

  try { await browser.close(); } catch (_) {}

  console.log("\n" + "=".repeat(50));
  console.log("pass: " + pass + ", fail: " + fail);
  if (fail > 0) throw new Error("regression_p3_args: " + fail + " check(s) failed");
}

await main();
