# aike-browser

Headless (or headed) Chrome automation for the [aike](https://github.com/nupha/aike) runtime, exposed as a zapi native extension. It speaks the Chrome DevTools Protocol (CDP) directly — no Puppeteer, no Node, no WebDriver — and gives JS/TS scripts two classes:

- `Browser` — launch, spawn or connect to a browser, manage tabs and contexts.
- `Page` — navigation, DOM, input, screenshots/PDF, network, cookies, storage, emulation, tracing, overlays and request interception.

## Features

- **Three ways in**: `Browser.launch()` spawns and owns a browser, `Browser.spawn()` returns a process handle to connect to later, `Browser.connect(cdpUrl)` attaches to an existing CDP endpoint.
- **Chrome-family auto-detection**: Chrome, Chromium, Brave and Edge are found on the usual install paths and `PATH`; a custom `bin` (for example [lightpanda](https://lightpanda.io)) is supported too.
- **Real device-level input**: `page.click()` / `type()` use CDP `Input.*` events, and `insertText()` handles CJK/Unicode.
- **Request interception**: pause, continue, abort or fulfill requests, with an automatic resume safety net.
- **Observability**: console/log capture, network capture, Chrome tracing, accessibility snapshots and CDP-IO stream reads.
- **No leaked processes**: spawned browsers and their temp profile directories are cleaned up on `kill()`, runtime exit, and failed launches.

## Requirements

- Chrome, Chromium, Brave or Edge, or any CDP server (for example [lightpanda](https://lightpanda.io) etc.) reachable by URL.

## Quick start

```ts
import { Browser } from "browser";
import * as fs from "fs";

const browser = await Browser.launch({ headless: true });
try {
  const page = await browser.newPage("about:blank");
  await page.setViewport(1280, 800);
  await page.navigate("https://example.com");
  console.log(await page.evaluate("document.title"));

  const png = await page.captureScreenshot({ format: "png" });
  await fs.writeBytes("example.png", png);
} finally {
  await browser.close();
}
```

Typed projects can point their compiler at [`types/browser.d.ts`](types/browser.d.ts). The declaration file is maintained by hand.

### Launch, spawn, connect

```ts
// Own the process: close() kills it.
const browser = await Browser.launch({
  headless: false,
  proxyUrl: "http://127.0.0.1:7890",
});

// Start a server only; connect later and keep ownership explicit.
const proc = await Browser.spawn({ bin: "/usr/bin/chromium", port: 9222 });
const browser2 = await Browser.connect(proc.cdpUrl);
await proc.kill();

// Attach to anything already speaking CDP; close() only disconnects.
const browser3 = await Browser.connect("ws://127.0.0.1:9222");
```

A `spawn()`ed process does not die when its JS handle is collected: it dies on `kill()` or at runtime exit. In contrast, a `launch()`ed browser is owned by its `Browser` instance, so `close()` kills it.

### API at a glance

| Area             | Examples                                                                                                                                                 |
| ---------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Browser / Target | `newPage`, `listPages`, `getTargets`, `newContext`, `setWindowBounds`, `setDownloadBehavior`                                                             |
| Navigation       | `navigate`, `reload`, `back`, `forward`, `waitForSelector`, `waitForFunction`, `waitForNavigation`                                                       |
| Content          | `evaluate`, `evaluateHandle`, `getOuterHTML`, `setContent`, `ariaSnapshot`, `captureSnapshot`, `addInitScript`                                           |
| DOM              | `querySelector`, `querySelectorAll`, `getAttribute`, `click`, `type`, `dragAndDrop`, `clickBackendNode`                                                  |
| Input            | `mouseClick`, `mouseMove`, `mouseDown`/`mouseUp`, `keyPress`, `insertText`                                                                               |
| Output           | `captureScreenshot`, `printToPDF`                                                                                                                        |
| Network          | cookies, `setExtraHTTPHeaders`, `setUserAgent`, `setOfflineMode`, `startNetworkCapture`, `getResponseBody`, `setRequestInterception` + `onRequestPaused` |
| Runtime          | `onConsole`, `onLog`, `getConsoleMessages`, remote-object helpers                                                                                        |
| Emulation        | viewport, geolocation, media features, timezone, locale, touch, CPU throttling                                                                           |
| Other            | storage cleanup, overlay highlight, computed styles, tracing, IO streams, certificate errors                                                             |

This table is only an overview. [`types/browser.d.ts`](types/browser.d.ts) is the authoritative definition of the API: every method, parameter, option field and return type is declared there.

## License

Apache License 2.0. See [`LICENSE.txt`](LICENSE.txt). Third-party components and their copyright notices are listed in [`NOTICE.txt`](NOTICE.txt).

Copyright (c) 2026 John Ray <996351336@qq.com>
