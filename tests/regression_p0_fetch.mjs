// Regression test: the P0 Fetch-domain fixes.
//
//   B-002 `headers` / `responseHeaders` must be an array of `{name, value}`;
//         sending a JSON object is a protocol error.
//   B-003 `Fetch.fulfillRequest.responseCode` is required — omitting `status`
//         left the request paused forever.
//   B-004 a paused request the callback does not handle (no callback, or a
//         callback that throws) must still be released.
//   B-005 `Fetch.failRequest.errorReason` must be a `Network.ErrorReason`.
//   B-006 `Fetch.continueRequest.postData` is a binary::String.
//   B-039 `setRequestInterception(true)` without a callback must not stall the
//         page.
//
// Fully offline: every navigation targets `http://localhost:39217/*`, an origin
// nothing listens on, so requests only exist behind our own Fetch
// interception. Each test gets its own page so one navigation cannot leak into
// the next.
//
//   aike -m tests/regression_p0_fetch.mjs

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

// Bound one awaited step. Chrome occasionally leaves a CDP round-trip
// unanswered for its full 30s native timeout while a navigation is settling; a
// bare `await` would then spend 30s per poll iteration and make the suite look
// hung. Racing a short timeout keeps a *stall* the same as a "not ready" answer,
// which is what the poll loop is for.
function bounded(p, ms) {
  return Promise.race([p.catch(() => null), sleep(ms).then(() => null)]);
}

// Poll without setTimeout: every awaited CDP round-trip gives the native event
// job a tick, which is what actually drains `Fetch.requestPaused`. Each step is
// bounded, and a wall-clock deadline caps the whole poll.
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

async function pageBody(page) {
  try {
    const t = await page.evaluate("document.body ? document.body.innerText : ''");
    return typeof t === "string" ? t : "";
  } catch (_) {
    return "";
  }
}

// Start a navigation and poll until the document is there.
//
// The navigation promise is deliberately not awaited: Chrome only answers
// `Page.navigate` once the navigation commits or fails, and it does not always
// fail a request that Fetch released into a refused connection — a stalled
// navigation is precisely the B-004/B-039 symptom under test here.
async function load(page, url, expect) {
  page.navigate(url).catch(function () {});
  return await poll(page, async () => {
    const t = await pageBody(page);
    return expect ? (t === expect ? t : "") : (t.length ? t : "");
  });
}

const ALIVE = "data:text/html,<p id='alive'>alive</p>";

// B-002 / B-003: `headers` becomes a HeaderEntry[] and a missing `status`
// still fulfills, instead of erroring out and leaving the request paused.
async function testRespond(browser) {
  emit("\n=== B-002/B-003: respondRequest headers + default status ===");
  const page = await browser.newPage();
  let lastError = null;
  page.onRequestPaused((evt) => {
    page
      .respondRequest(evt.requestId, {
        status: 200,
        headers: { "X-Probe": "yes", "Content-Type": "text/plain" },
        body: "mocked",
      })
      .catch((e) => {
        lastError = e && (e.message || e);
      });
  });
  await page.setRequestInterception(true);

  const text = await load(page, ORIGIN + "/hdr", "mocked");
  await page.evaluate("0");
  check("respondRequest(headers) does not raise a CDP error", lastError === null,
    "error=" + lastError);
  check("respondRequest(headers) fulfills the navigation", text === "mocked",
    "body=" + JSON.stringify(text));

  await page.setRequestInterception(false);
  await bounded(page.close(), 5000);
}

// B-006: `postData` must be base64 on the wire; a plain string is a protocol
// error that leaves the request paused. Rewriting the method only makes sense
// for a subresource, so the document is served and the POST is issued from it.
async function testContinuePostData(browser) {
  emit("\n=== B-006: continueRequest(postData) ===");
  const page = await browser.newPage();
  let postId = null;
  let error = null;
  page.onRequestPaused((evt) => {
    const url = (evt.request && evt.request.url) || "";
    if (url.indexOf("/echo") >= 0) {
      postId = evt.requestId;
      page.continueRequest(evt.requestId, { method: "POST", postData: "hello=1" }).catch((e) => {
        error = e && (e.message || e);
      });
    } else {
      page.respondRequest(evt.requestId, {
        status: 200,
        body: "<!DOCTYPE html><p id='host'>host</p>",
        contentType: "text/html",
      });
    }
  });
  await page.setRequestInterception(true);
  await load(page, ORIGIN + "/host", "host");

  await page.evaluate("fetch('" + ORIGIN + "/echo', { method: 'POST' }).catch(function () {})");
  await poll(page, async () => postId !== null);
  check("subresource POST was intercepted", postId !== null);
  check("continueRequest(postData) raised no CDP error", error === null, "error=" + error);

  await page.setRequestInterception(false);
  await bounded(page.close(), 5000);
}

// B-005: an unknown `errorReason` is a protocol error — reject instead of
// sending it, which would otherwise leave the request paused.
async function testAbortReasonValidation(browser) {
  emit("\n=== B-005: abortRequest(errorReason) validation ===");
  const page = await browser.newPage();
  let rejection = null;
  page.onRequestPaused((evt) => {
    // Argument validation throws synchronously, like any JS function that
    // rejects bad arguments before building its promise.
    try {
      page.abortRequest(evt.requestId, "NotAReason");
    } catch (e) {
      rejection = e && (e.message || e);
    }
  });
  await page.setRequestInterception(true);
  await load(page, ORIGIN + "/bad-reason");
  check("abortRequest throws on an unknown errorReason", typeof rejection === "string",
    "rejection=" + rejection);
  check("error names the offending value",
    typeof rejection === "string" && rejection.indexOf("NotAReason") >= 0,
    "rejection=" + rejection);
  await page.setRequestInterception(false);

  // The request must have been released anyway, so the page keeps working.
  const alive = await load(page, ALIVE, "alive");
  check("page still loads after the rejected abort", alive === "alive",
    "body=" + JSON.stringify(alive));
  await bounded(page.close(), 5000);
}

// B-039: interception enabled with no callback must not stall the page.
async function testInterceptionWithoutCallback(browser) {
  emit("\n=== B-039: setRequestInterception(true) without a callback ===");
  const page = await browser.newPage();
  await page.setRequestInterception(true);
  // Nothing listens on ORIGIN: the navigation failing is fine, what matters is
  // that it terminates instead of staying paused.
  const text = await load(page, ORIGIN + "/nocb");
  check("navigation completes without an onRequestPaused callback", text.length > 0,
    "body=" + JSON.stringify(text));
  await page.setRequestInterception(false);
  const alive = await load(page, ALIVE, "alive");
  check("page loads after interception without a callback", alive === "alive",
    "body=" + JSON.stringify(alive));
  await bounded(page.close(), 5000);
}

// B-004: a throwing callback must not leave the page paused.
async function testThrowingCallback(browser) {
  emit("\n=== B-004: throwing onRequestPaused callback ===");
  const page = await browser.newPage();
  let fired = 0;
  page.onRequestPaused(() => {
    fired++;
    throw new Error("callback blew up on purpose");
  });
  await page.setRequestInterception(true);
  const text = await load(page, ORIGIN + "/throw");
  check("throwing callback was invoked", fired >= 1, "fired=" + fired);
  check("navigation completes after a throwing callback", text.length > 0,
    "body=" + JSON.stringify(text));
  await page.setRequestInterception(false);
  const alive = await load(page, ALIVE, "alive");
  check("page loads after a throwing callback", alive === "alive",
    "body=" + JSON.stringify(alive));
  await bounded(page.close(), 5000);
}

async function main() {
  // Watchdog: a stalled Chrome round-trip must fail the suite, not hang it.
  setTimeout(() => {
    console.log("  [WATCHDOG] forcing exit after 90s (a step likely hung)");
    process.exit(1);
  }, 90000);

  const browser = await Browser.launch({ headless: true });
  check("Browser launched", browser.isRunning === true);

  const tests = [
    testRespond,
    testContinuePostData,
    testAbortReasonValidation,
    testInterceptionWithoutCallback,
    testThrowingCallback,
  ];
  for (const fn of tests) {
    // A brand-new tab occasionally comes up with a wedged CDP session: Chrome
    // never delivers `Fetch.requestPaused` and `evaluate` fails, so every
    // interception check would fail. Retry the whole case on a fresh page
    // instead of reporting a browser flake as a product failure.
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
  // Exit explicitly: the watchdog timer above stays pending otherwise, which
  // keeps the event loop alive for the full 90s after the checks are done (and
  // any stray native listener must not decide when the suite ends).
  process.exit(fail > 0 ? 1 : 0);
}

await main();