// Regression test: the P0 Runtime result-handling fixes.
//
//   B-019 `evaluate` reported only `exceptionDetails.text`, which is always the
//         generic `"Uncaught"`, so the real error message was lost.
//   B-020 non-serializable results (BigInt, NaN, Infinity) were silently
//         downgraded to `description`, e.g. `123n` came back as `"123n"`.
//   B-021 `evaluateHandle` ignored `exceptionDetails`, so a throwing expression
//         resolved to `{ objectId: "" }`, and a primitive result gave the caller
//         no way to tell it had no handle at all.
//   B-022 `callFunctionOn` never set `awaitPromise`, so a function returning a
//         promise resolved to `null`.
//
//   aike -m tests/regression_p0_runtime.mjs

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

// B-019: the rejection carries the thrown value, not "Uncaught".
async function testEvaluateThrows(page) {
  console.log("\n=== B-019: evaluate reports the real error ===");
  let message = null;
  try {
    await page.evaluate("throw new Error('boom-please')");
  } catch (e) {
    message = e && (e.message || e);
  }
  check("evaluate rejects", typeof message === "string", "message=" + message);
  check("message contains the thrown text",
    typeof message === "string" && message.indexOf("boom-please") >= 0, "message=" + message);
  check("message is not the generic CDP text",
    typeof message === "string" && message.indexOf("Uncaught") < 0, "message=" + message);

  let typeMessage = null;
  try {
    await page.evaluate("null.x");
  } catch (e) {
    typeMessage = e && (e.message || e);
  }
  check("TypeError text survives",
    typeof typeMessage === "string" && typeMessage.indexOf("TypeError") >= 0,
    "message=" + typeMessage);
}

// B-020: `unserializableValue` is the spec field for primitives JSON cannot
// represent; `description` is only a debug string.
async function testUnserializable(page) {
  console.log("\n=== B-020: evaluate reads unserializableValue ===");
  const nan = await page.evaluate("0/0");
  check("NaN comes back as 'NaN'", nan === "NaN", "got=" + JSON.stringify(nan));

  const inf = await page.evaluate("1/0");
  check("Infinity comes back as 'Infinity'", inf === "Infinity", "got=" + JSON.stringify(inf));

  const big = await page.evaluate("123n");
  check("BigInt comes back in its wire form", big === "123n", "got=" + JSON.stringify(big));

  // Ordinary values must be unaffected.
  check("plain object still round-trips",
    JSON.stringify(await page.evaluate("({ a: 1, b: [2, 3] })")) === '{"a":1,"b":[2,3]}');
  check("plain number still round-trips", (await page.evaluate("7")) === 7);
}

// B-021: handles report exceptions, and primitives carry their value.
async function testEvaluateHandle(page) {
  console.log("\n=== B-021: evaluateHandle reports exceptions and primitives ===");
  let message = null;
  try {
    await page.evaluateHandle("throw new Error('handle-boom')");
  } catch (e) {
    message = e && (e.message || e);
  }
  check("evaluateHandle rejects on a throwing expression", typeof message === "string",
    "message=" + message);
  check("handle rejection carries the thrown text",
    typeof message === "string" && message.indexOf("handle-boom") >= 0, "message=" + message);

  const obj = await page.evaluateHandle("({ n: 1 })");
  check("object expression yields a real handle",
    typeof obj.objectId === "string" && obj.objectId.length > 0, "objectId=" + obj.objectId);

  const prim = await page.evaluateHandle("42");
  check("primitive expression reports an empty handle", prim.objectId === "",
    "objectId=" + JSON.stringify(prim.objectId));
  check("primitive expression carries its value", prim.value === 42,
    "value=" + JSON.stringify(prim.value));
}

// B-022: callFunctionOn awaits a returned promise.
async function testCallFunctionOn(page) {
  console.log("\n=== B-022: callFunctionOn awaits promises ===");
  const handle = await page.evaluateHandle("({ base: 20 })");
  check("evaluateHandle gave a handle", handle.objectId.length > 0);

  const sum = await page.callFunctionOn(
    handle.objectId,
    "function (x) { return this.base + x; }",
    [22],
  );
  check("callFunctionOn returns a plain value", sum === 42, "got=" + JSON.stringify(sum));

  const awaited = await page.callFunctionOn(
    handle.objectId,
    "function () { return Promise.resolve('awaited-value'); }",
  );
  check("callFunctionOn awaits a returned promise",
    awaited === "awaited-value", "got=" + JSON.stringify(awaited));

  let rejection = null;
  try {
    await page.callFunctionOn(
      handle.objectId,
      "function () { return Promise.reject(new Error('rejected-value')); }",
    );
  } catch (e) {
    rejection = e && (e.message || e);
  }
  check("a rejected promise surfaces as a rejection, not null",
    typeof rejection === "string");
  check("rejection carries the reason",
    typeof rejection === "string" && rejection.indexOf("rejected-value") >= 0,
    "rejection=" + rejection);

  let message = null;
  try {
    await page.callFunctionOn(handle.objectId, "function () { throw new Error('fn-boom'); }");
  } catch (e) {
    message = e && (e.message || e);
  }
  check("callFunctionOn reports the thrown text",
    typeof message === "string" && message.indexOf("fn-boom") >= 0, "message=" + message);
}

async function main() {
  const browser = await Browser.launch({ headless: true });
  check("Browser launched", browser.isRunning === true);
  const page = await browser.newPage();
  await page.navigate("data:text/html,<p id='rt'>runtime</p>");
  await page.waitForSelector("#rt", 5000);

  const tests = [testEvaluateThrows, testUnserializable, testEvaluateHandle, testCallFunctionOn];
  for (const fn of tests) {
    try {
      await fn(page);
    } catch (e) {
      console.log("  [FAIL] " + fn.name + " threw: " + (e && (e.message || e)));
      fail++;
    }
  }

  try { await page.close(); } catch (_) {}
  try { await browser.close(); } catch (_) {}

  console.log("\n" + "=".repeat(50));
  console.log("pass: " + pass + ", fail: " + fail);
  if (fail > 0) throw new Error("regression_p0_runtime: " + fail + " check(s) failed");
}

await main();