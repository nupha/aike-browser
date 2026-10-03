// Type declarations for the "browser" module
// import { Browser } from "browser";
//
// The registered JS API lives in `src/*.rs`: `class_browser.rs` and
// `class_page.rs` hold the `#[zq_method]` / `#[zq_static_method]` bodies and
// simply forward to the per-domain modules (`dom.rs`, `network.rs`, `fetch.rs`,
// `runtime.rs`, `input.rs`, `emulation.rs`, `log.rs`, `io.rs`, `storage.rs`,
// `tracing.rs`, `overlay.rs`, `css.rs`, `target.rs`, `security.rs`). There is no
// protocol-description table in the source and no generator: the wire contract is
// the CDP one, and this file is maintained by hand — `bugs.md` records where a
// declaration here still disagrees with the implementation.
//
// `make check-types` (`aike -m tests/check_dts.mjs`) diffs this file's member list
// against the registration table in `src/*.rs`, so adding a method on the Rust
// side without declaring it here — or the reverse — fails loudly.

declare module "browser" {
  // =========================================================================
  // Launch options
  // =========================================================================

  interface LaunchOptions {
    /**
     * Attach to an already-running browser via its CDP endpoint
     * (e.g. "ws://127.0.0.1:9222"). When set, no browser is spawned and
     * `close()` only disconnects — the external process is left running.
     */
    cdpUrl?: string;
    /** Run browser in headless mode. Default: true */
    headless?: boolean;
    /** Proxy server URL (e.g. "http://127.0.0.1:7890"). */
    proxyUrl?: string;
    /** Custom Chrome/Chromium executable path. Auto-detected when omitted. */
    chromePath?: string;
    /** Remote debugging port. Default: auto-assigned free port (pass 0 for auto). */
    port?: number;
  }

  // =========================================================================
  // Spawn options / process handle
  // =========================================================================

  interface SpawnOptions {
    /**
     * Browser binary to spawn. Omit → auto-detect system
     * Chrome/Chromium/Brave/Edge (known install paths, then PATH).
     */
    bin?: string;
    /**
     * Full custom command line (e.g. `["serve", "--port", "9222"]` for
     * lightpanda). Requires `bin` and an explicit `port`; `headless` /
     * `proxyUrl` are ignored (the command line controls them).
     */
    args?: string[];
    /** CDP port. Default: auto-assigned free port (auto mode only). */
    port?: number;
    /** Run browser in headless mode. Default: true (auto mode only). */
    headless?: boolean;
    /** Proxy server URL (auto mode only). */
    proxyUrl?: string;
  }

  /**
   * Handle to a browser process started by `Browser.spawn()`. Does NOT own
   * the process lifetime: dropping / GC-collecting the handle is harmless;
   * the process dies on `kill()` or at runtime exit.
   */
  interface BrowserProcess {
    /** Resolved browser binary path. */
    readonly bin: string;
    /** Browser-level CDP WebSocket URL — pass to `Browser.connect()`. */
    readonly cdpUrl: string;
    /** Browser process id, or `null` once killed. */
    readonly pid: number | null;
    /** Kill the browser process. Resolves after it has exited. */
    kill(): Promise<void>;
  }

  // =========================================================================
  // Browser
  // =========================================================================

  /** One entry of a CDP target list (a tab, worker, service worker, ...). */
  interface TargetInfo {
    targetId: string;
    type: string;
    title: string;
    url: string;
    attached: boolean;
    browserContextId?: string;
  }

  /** A Chrome browser instance. Cannot be constructed directly. */
  interface Browser {
    // ---- Target domain ----
    /**
     * Create a new tab (page).
     * @param urlOrOpts - URL string, or `{ url?, contextId? }`.
     */
    newPage(
      urlOrOpts?: string | { url?: string; contextId?: string },
    ): Promise<Page>;

    /** Close the browser and release all resources. */
    close(): Promise<void>;

    /**
     * Whether the browser is still usable.
     *
     * Owning mode (`launch`) reports the child process. Attach mode (`connect`)
     * owns no process, so it reports whether this `Browser` still holds an open
     * CDP connection — the only liveness answerable without a round trip. It is
     * `false` after `close()`.
     */
    readonly isRunning: boolean;

    // ---- Browser domain ----
    /** Browser/platform version info. */
    getVersion(): Promise<{
      protocolVersion: string;
      product: string;
      revision: string;
      userAgent: string;
      jsVersion: string;
    }>;

    /** Current browser window bounds. */
    getWindowBounds(): Promise<any>;

    /** Set the browser window bounds `{ left?, top?, width?, height?, windowState? }`. */
    setWindowBounds(bounds: {
      left?: number;
      top?: number;
      width?: number;
      height?: number;
      /**
       * One of `"normal"`, `"minimized"`, `"maximized"`, `"fullscreen"`.
       * Anything else throws a RangeError (Chrome aborts the browser process
       * on an unknown state).
       */
      windowState?: "normal" | "minimized" | "maximized" | "fullscreen";
    }): Promise<void>;

    /** Set global download behavior. Empty path → "deny", otherwise "allow". */
    setDownloadBehavior(path: string): Promise<void>;

    // ---- Target domain ----
    /** Page-type targets only. */
    listPages(): Promise<TargetInfo[]>;

    /**
     * Every target, of every type.
     *
     * The `{ targetInfos }` wrapper is `Target.getTargets`' own response object,
     * passed through unchanged — unlike `listPages`, which unwraps and filters.
     */
    getTargets(): Promise<{ targetInfos: TargetInfo[] }>;

    /** Create a browser context. Returns the new context id (string). */
    newContext(): Promise<string>;

    /** Dispose a browser context created via `newContext()`. */
    disposeContext(contextId: string): Promise<void>;

    // ---- Security domain ----
    /** Ignore HTTPS certificate errors. */
    setIgnoreHTTPSErrors(enabled: boolean): Promise<void>;

    /**
     * Register a callback for `Security.certificateError` events. The callback
     * receives `{ eventId, errorType, requestURL, accept }`; `accept(true)`
     * turns certificate-error ignoring on (the deprecated per-event
     * `continue`/`cancel` action is gone).
     *
     * Scoped to this browser, and one callback per browser: registering again
     * replaces the previous callback and stops its forwarding task.
     */
    handleCertificateError(
      callback: (event: {
        eventId: number;
        errorType?: string;
        requestURL?: string;
        accept(enabled: boolean): void;
      }) => void,
    ): void;
  }

  // =========================================================================
  // Page
  // =========================================================================

  /** A single browser tab/page. Cannot be constructed directly. */
  interface Page {
    /** Unique target ID assigned by Chrome. */
    readonly targetId: string;

    // ---- Navigation ----
    /**
     * Navigate to a URL. Resolves when Chrome has committed the navigation.
     * @throws Rejects with `navigate failed: <errorText>` when the browser could
     *   not load the URL — `Page.navigate` answers successfully even for a dead
     *   navigation and puts the reason in `errorText`.
     */
    navigate(url: string): Promise<void>;

    /** Reload the page. */
    reload(options?: { ignoreCache?: boolean }): Promise<void>;

    /** Stop the current page load. */
    stopLoading(): Promise<void>;

    /** Navigate back in history. */
    back(): Promise<void>;

    /** Navigate forward in history. */
    forward(): Promise<void>;

    /** Get the navigation history. */
    getNavigationHistory(): Promise<any>;

    /** Bring the tab to the front (focus). */
    bringToFront(): Promise<void>;

    /**
     * Wait for a CSS selector to appear on the page.
     * @param selector  - CSS selector string.
     * @param timeoutMs - Timeout in milliseconds. Default: 30000.
     */
    waitForSelector(selector: string, timeoutMs?: number): Promise<void>;

    /**
     * Poll a JS `expression` until it evaluates truthy or `timeoutMs` elapses.
     * Use for waiting on text/conditions that `waitForSelector` cannot express.
     */
    waitForFunction(expression: string, timeoutMs?: number): Promise<void>;

    /**
     * Wait for a navigation to complete.
     *
     * Satisfied by `Page.loadEventFired` (cross-document) **or**
     * `Page.navigatedWithinDocument` (same-document), so an SPA route change or
     * a hash change counts.
     *
     * @param timeoutMs - Timeout in milliseconds. Default: 30000.
     */
    waitForNavigation(timeoutMs?: number): Promise<void>;

    // ---- Content / evaluate ----
    /**
     * Execute JavaScript code in the page context.
     * @param jsCode - JavaScript source to evaluate.
     * @returns The JSON-serialized result, or its `unserializableValue` string
     *   form for values JSON cannot represent (NaN, Infinity, BigInt).
     *   Rejects with the thrown error's message when the code throws.
     */
    evaluate(jsCode: string): Promise<any>;

    /** Get the full outerHTML of the document. */
    getOuterHTML(): Promise<string>;

    /** Set the page content (HTML) directly. */
    setContent(html: string): Promise<void>;

    /** Capture a snapshot of the page as a single string. */
    captureSnapshot(options?: { format?: string }): Promise<string>;

    /**
     * Accessibility (aria) tree of the page, as a JSON string.
     *
     * An array of nodes, each:
     * - `nodeId`: the CDP accessibility node id (a numeric **string-or-number**),
     *   or `null` when Chrome reported an id this parser could not read;
     * - `backendNodeId`: stable CDP backend node id — pass this to
     *   `clickBackendNode` / `typeBackendNode`;
     * - `parentIndex`: index of the nearest **kept** ancestor in this array, or
     *   `-1` at the root;
     * - `contentType`, `textValue`, `attributes: { role, name, description }`.
     *
     * Nodes Chrome marks `ignored` (and their subtrees) are excluded, matching
     * what an accessibility client would see.
     */
    ariaSnapshot(): Promise<string>;

    /**
     * Layout metrics. The non-`css*` viewports are deprecated upstream; these
     * are the ones Chrome populates in CSS pixels.
     */
    getLayoutMetrics(): Promise<{
      cssLayoutViewport: {
        pageX: number;
        pageY: number;
        clientWidth: number;
        clientHeight: number;
      };
      cssVisualViewport: {
        offsetX: number;
        offsetY: number;
        pageX: number;
        pageY: number;
        clientWidth: number;
        clientHeight: number;
        scale: number;
        zoom?: number;
      };
      cssContentSize: { x: number; y: number; width: number; height: number };
    }>;

    /** Inject a script that runs in every new document (before other scripts). */
    addInitScript(source: string): Promise<string>;

    /** Get the iframe/frame hierarchy tree. */
    getFrameTree(): Promise<{
      frameTree: {
        frame: { id: string; parentId?: string; url: string; name?: string };
        childFrames?: any[];
      };
    }>;

    /** Enable Runtime domain events (consoleAPICalled, exceptionThrown, etc.). */
    enableRuntime(): Promise<void>;

    // ---- Runtime: remote objects ----
    /**
     * Evaluate an expression and return a handle `{ objectId }` referencing the
     * result. A primitive result has no handle: `objectId` is then `""` and
     * `value` carries the primitive.
     */
    evaluateHandle(expression: string): Promise<{ objectId: string; value?: any }>;

    /**
     * Enumerate an object's own properties by its CDP `objectId`.
     * @returns A plain object mapping each property name to its JSON value.
     */
    getProperties(objectId: string): Promise<Record<string, any>>;

    /**
     * Call a function on a remote object.
     * @param objectId - CDP objectId returned by `evaluateHandle`.
     * @param functionDeclaration - Full function source, e.g. `"function (x) { return this.a + x; }"`.
     * @param args - Optional call arguments (JSON-serializable).
     * @returns The JSON-serialized return value.
     */
    callFunctionOn(
      objectId: string,
      functionDeclaration: string,
      args?: any[],
    ): Promise<any>;

    /**
     * Register a callback invoked for every `Runtime.consoleAPICalled` event
     * (e.g. `console.log` in the page). The callback receives the raw event
     * params object (`{ type, args, ... }`).
     */
    onConsole(callback: (msg: any) => void): void;

    // ---- Screenshot / PDF ----
    /** Capture a screenshot of the current page. */
    captureScreenshot(options?: ScreenshotOptions): Promise<Uint8Array>;

    /** Export the current page as PDF (Chromium-specific). */
    printToPDF(options?: PdfOptions): Promise<Uint8Array>;

    // ---- Viewport / emulation ----
    /** Set the browser window / device viewport size. */
    setViewport(width: number, height: number): Promise<void>;

    /** Override geolocation. */
    setGeolocation(
      latitude: number,
      longitude: number,
      accuracy?: number,
    ): Promise<void>;

    /** Override supported media features (e.g. `[{ name: "prefers-color-scheme", value: "dark" }]`). */
    setMediaFeatures(
      features: Array<{ name: string; value: string }>,
    ): Promise<void>;

    /** Override the timezone (IANA name, e.g. "Asia/Shanghai"). */
    setTimezone(timezoneId: string): Promise<void>;

    /** Override the locale (e.g. "en-US"). */
    setLocale(locale: string): Promise<void>;

    /** Enable / disable touch emulation. */
    setTouchEnabled(enabled: boolean): Promise<void>;

    /** Throttle CPU (rate multiplier, e.g. 4 = 4x slower). */
    setCPUThrottling(rate: number): Promise<void>;

    // ---- Dialogs ----
    /**
     * Accept or dismiss a JavaScript dialog (alert/confirm/prompt).
     * @param accept    - true=accept/OK, false=dismiss/cancel.
     * @param promptText - Text to enter for prompt dialogs.
     */
    handleDialog(accept: boolean, promptText?: string): Promise<void>;

    // ---- Download ----
    /** Set whether to bypass Content-Security-Policy. */
    setBypassCSP(enabled: boolean): Promise<void>;

    // ---- Files ----
    /**
     * Upload files to an <input type="file"> element.
     * @param selector  - CSS selector for the file input element.
     * @param filePaths - Absolute paths to the files to upload.
     */
    uploadFile(selector: string, filePaths: string[]): Promise<void>;

    /** Close this tab. */
    close(): Promise<void>;

    // ---- DOM ----
    /** Resolve a selector to a DOM nodeId (number), or 0 if not found. */
    querySelector(selector: string): Promise<number>;

    /** Resolve a selector to an array of DOM nodeIds (numbers). */
    querySelectorAll(selector: string): Promise<number[]>;

    /** Get an attribute value of the first element matching the selector. */
    getAttribute(selector: string, name: string): Promise<string | null>;

    /** Focus the first element matching the selector. */
    focus(selector: string): Promise<void>;

    /** Get the box model (quads) of the first element matching the selector. */
    getBoxModel(selector: string): Promise<any>;

    /** Remove the first element matching the selector. */
    remove(selector: string): Promise<void>;

    /** Get the outerHTML of the first element matching the selector. */
    getNodeValue(selector: string): Promise<string>;

    /** Click the first element matching the selector. */
    click(selector: string): Promise<void>;

    /**
     * Drag the first element matching `sourceSelector` onto the first element
     * matching `targetSelector`.
     *
     * Drives the real HTML5 drag sequence (press → move with the left-button
     * mask → explicit `Input.dispatchDragEvent` drop → release), so page
     * `dragstart` / `dragenter` / `dragover` / `drop` / `dragend` handlers all
     * fire. No payload is carried.
     */
    dragAndDrop(sourceSelector: string, targetSelector: string): Promise<void>;

    /** Type text into the first element matching the selector. */
    type(selector: string, text: string): Promise<void>;

    /**
     * Click an element by its CDP backend node id (from `ariaSnapshot`).
     * More robust to DOM changes than a CSS selector.
     */
    clickBackendNode(backendNodeId: number): Promise<void>;

    /**
     * Focus an element by its CDP backend node id (from `ariaSnapshot`), then
     * type text into it. CJK/Unicode safe.
     */
    typeBackendNode(backendNodeId: number, text: string): Promise<void>;

    // ---- Input ----
    /**
     * Click at pixel coordinates (x, y).
     * Performs mousePressed + mouseReleased via native CDP Input.dispatchMouseEvent.
     */
    mouseClick(
      x: number,
      y: number,
      options?: MouseClickOptions,
    ): Promise<void>;

    /**
     * Move mouse to pixel coordinates (x, y).
     * Uses native CDP Input.dispatchMouseEvent (mouseMoved).
     */
    mouseMove(x: number, y: number): Promise<void>;

    /**
     * Dispatch `mousePressed` at (x, y) without releasing.
     * Pair with `mouseUp` to perform drag or press/hold gestures.
     */
    mouseDown(x: number, y: number, options?: MouseClickOptions): Promise<void>;

    /**
     * Dispatch `mouseReleased` at (x, y).
     */
    mouseUp(x: number, y: number, options?: MouseClickOptions): Promise<void>;

    /**
     * Simulate pressing a key (keyDown → keyUp).
     * Uses native CDP Input.dispatchKeyEvent. Accepts a single printable
     * character or a named key (`"Enter"`, `"Tab"`, `"Escape"`, `"Backspace"`,
     * `"ArrowLeft"` … `"ArrowDown"`, `"Home"`, `"End"`, `"PageUp"`,
     * `"PageDown"`, `"Insert"`, `"Delete"`, `"Shift"`, `"Control"`, `"Alt"`,
     * `"Meta"`, `"CapsLock"`, `"F1"`…`"F12"`); anything else throws a
     * TypeError.
     */
    keyPress(key: string, options?: KeyPressOptions): Promise<void>;

    /**
     * Insert text directly into the focused element.
     * Uses native CDP Input.insertText — reliable for CJK / Unicode.
     */
    insertText(text: string): Promise<void>;

    // ---- Request interception (Fetch domain) ----
    /**
     * Enable / disable request interception. While enabled every request is
     * paused and `onRequestPaused` fires; a request the callback does not
     * explicitly continue / abort / fulfill is released automatically.
     */
    setRequestInterception(enabled: boolean): Promise<void>;

    /**
     * Continue a paused request by its interceptionId.
     * @param opts - `url`, `method`, `headers` (`Record<string,string>`) and
     *   `postData` (plain text; base64-encoded on the wire for you).
     */
    continueRequest(
      interceptionId: string,
      opts?: {
        url?: string;
        method?: string;
        headers?: Record<string, string>;
        postData?: string;
      },
    ): Promise<void>;

    /**
     * Abort a paused request by its interceptionId.
     * @param reason - A `Network.ErrorReason` value; anything else throws a
     *   RangeError.
     */
    abortRequest(
      interceptionId: string,
      reason?:
        | "Failed"
        | "Aborted"
        | "TimedOut"
        | "AccessDenied"
        | "ConnectionClosed"
        | "ConnectionReset"
        | "ConnectionRefused"
        | "ConnectionAborted"
        | "ConnectionFailed"
        | "NameNotResolved"
        | "InternetDisconnected"
        | "AddressUnreachable"
        | "BlockedByClient"
        | "BlockedByResponse",
    ): Promise<void>;

    /**
     * Fulfill a paused request by its interceptionId with a response.
     * @param opts.status - Response code. Defaults to 200 when omitted.
     */
    respondRequest(
      interceptionId: string,
      opts?: {
        status?: number;
        headers?: Record<string, string>;
        body?: string;
        contentType?: string;
      },
    ): Promise<void>;

    /** Register a callback when a request is paused (interception). */
    onRequestPaused(callback: (event: any) => void): void;

    // ---- Network ----
    /** Get all cookies for the current page. */
    getCookies(): Promise<Cookie[]>;

    /** Set a single cookie for the current page. */
    setCookie(cookie: Cookie): Promise<void>;

    /** Set multiple cookies for the current page. */
    setCookies(cookies: Cookie[]): Promise<void>;

    /** Delete a cookie by name. */
    deleteCookie(name: string, url?: string): Promise<void>;

    /** Clear all browser cookies. */
    clearBrowserCookies(): Promise<void>;

    /** Override the User-Agent string. */
    setUserAgent(userAgent: string): Promise<void>;

    /** Set extra HTTP headers sent with every request. */
    setExtraHTTPHeaders(headers: Record<string, string>): Promise<void>;

    /** Enable / disable offline mode. */
    setOfflineMode(enabled: boolean): Promise<void>;

    /** Enable / disable the HTTP cache. */
    setCacheDisabled(disabled: boolean): Promise<void>;

    /**
     * Get the HTTP response body for a given request ID.
     * Requires Network domain to be enabled (see startNetworkCapture).
     * @returns `{ body: string, base64Encoded: boolean }`
     */
    getResponseBody(
      requestId: string,
    ): Promise<{ body: string; base64Encoded: boolean }>;

    /**
     * Get the POST body of a request by its (Network-domain) request ID.
     * Requires the request to be intercepted (Fetch domain) so the body is
     * buffered; pass `event.networkId` from `onRequestPaused`.
     */
    getRequestPostData(requestId: string): Promise<string>;

    /**
     * Start capturing network requests.
     * Enables the CDP Network domain and begins recording
     * Network.requestWillBeSent / responseReceived / loadingFinished events.
     * Calling it again replaces the running capture. At most 5000 events are
     * retained per page; the oldest are dropped.
     */
    startNetworkCapture(): Promise<void>;

    /** Stop the network capture task. */
    stopNetworkCapture(): void;

    /**
     * The captured network events, oldest first.
     * Each entry has `{ method, params }` from the raw CDP event.
     * Synchronous — the buffer is in-process.
     */
    getCapturedRequests(): Array<{ method: string; params: any }>;

    /** Clear captured network requests. */
    clearCapturedRequests(): void;

    // ---- Console / Log ----
    /**
     * Register a callback for console / log messages.
     *
     * Both CDP sources feed it (`Runtime.consoleAPICalled` and
     * `Log.entryAdded`), and each message also goes into the ring read by
     * `getConsoleMessages`. Registering again replaces the callback — the page
     * never receives the same message twice.
     */
    onLog(callback: (message: ConsoleMessage) => void): void;

    /**
     * Read buffered console / log messages, oldest first.
     * Pass `clear=true` to drain the buffer; without it the entries stay.
     * Synchronous; bounded per page.
     */
    getConsoleMessages(clear?: boolean): ConsoleMessage[];

    // ---- Storage ----
    /** Clear cache/storage for an origin. `storageTypes` defaults to `"all"`. */
    clearData(origin: string, storageTypes?: string): Promise<void>;

    /**
     * Resolve a frame's storage key (string) by its CDP frameId. Resolves
     * `null` when the frame has no storage key.
     */
    getStorageKeyForFrame(frameId: string): Promise<string | null>;

    // ---- Overlay (debug highlight) ----
    /**
     * Highlight the first element matching the selector.
     * Rejects when `Overlay.enable` or `Overlay.highlightNode` fails, instead of
     * resolving without drawing anything.
     */
    highlight(selector: string, options?: HighlightOptions): Promise<void>;

    /** Remove any active overlay highlight. */
    hideHighlight(): Promise<void>;

    // ---- CSS ----
    /** Get the computed style of the first element matching the selector. */
    getComputedStyle(selector: string): Promise<Record<string, string>>;

    /** Get the matched CSS rules of the first element matching the selector. */
    getMatchedStyles(selector: string): Promise<any>;

    // ---- IO ----
    /** Read a CDP IO stream handle to completion and return its text. */
    readStream(handle: string): Promise<string>;

    // ---- Tracing ----
    /** Start a Chrome trace. `categories` is a comma-separated CDP filter. */
    startTracing(categories?: string): Promise<void>;

    /**
     * Stop tracing and return the captured trace as a `{ traceEvents: [...] }`
     * JSON string.
     * @throws Rejects when Chrome sends no `tracingComplete` (rather than
     *   resolving with an empty string) or when the stream cannot be read.
     */
    stopTracing(): Promise<string>;
  }

  // =========================================================================
  // Supporting types
  // =========================================================================

  /** Screenshot capture options. */
  interface ScreenshotOptions {
    /**
     * Image format. Default: "png".
     * `quality` only applies to the lossy formats (`jpeg`, `webp`).
     */
    format?: "png" | "jpeg" | "webp";
    /**
     * Image quality, 1-100, for the lossy formats. No default is sent, so
     * Chrome picks its own encoding quality when this is omitted.
     */
    quality?: number;
    /** Capture a sub-region instead of the whole viewport (CSS px). */
    clip?: {
      x: number;
      y: number;
      width: number;
      height: number;
      scale?: number;
    };
    /** Capture the full scrollable page (maps to CDP `captureBeyondViewport`). */
    fullPage?: boolean;
    /** Explicit CDP `captureBeyondViewport` flag. */
    captureBeyondViewport?: boolean;
    /** Capture from the surface (default true). */
    fromSurface?: boolean;
    /** Skip encoding optimizations for speed. */
    optimizeForSpeed?: boolean;
  }

  /**
   * A cookie as returned by `getCookies()`.
   *
   * Every field `setCookie` / `setCookies` accept is present, so a cookie read
   * back can be written straight out again without losing attributes. `expires`
   * is omitted for a session cookie, and `session` is then `true`.
   */
  interface Cookie {
    name: string;
    value: string;
    domain?: string;
    path?: string;
    httpOnly?: boolean;
    secure?: boolean;
    /** True for a cookie with no expiry. */
    session?: boolean;
    /** One of `"Strict"`, `"Lax"`, `"None"`; anything else throws a RangeError. */
    sameSite?: "Strict" | "Lax" | "None";
    /** Unix timestamp in seconds; absent for a session cookie. */
    expires?: number;
    /** CDP `CookiePriority`, e.g. `"Low"`, `"Medium"`, `"High"`. Read-only. */
    priority?: string;
    /** Byte size, as reported by Chrome. Read-only. */
    size?: number;
    /** Read-only provenance fields Chrome adds. */
    sourcePort?: number;
    sourceScheme?: string;
  }

  /** PDF export options. */
  interface PdfOptions {
    landscape?: boolean;
    paperWidth?: number;
    paperHeight?: number;
    /** Print background graphics. Default: true */
    printBackground?: boolean;
  }

  /** Mouse click options. */
  interface MouseClickOptions {
    /** Mouse button: "left" (default), "middle", "right". */
    button?: "left" | "middle" | "right";
    /** Click count: 1 (single), 2 (double). Default: 1. */
    clickCount?: number;
  }

  /** Key press options. */
  interface KeyPressOptions {
    /** Modifier bitmask: 1=Alt, 2=Ctrl, 4=Meta, 8=Shift. */
    modifiers?: number;
  }

  /** An `Overlay.RGBA` colour. */
  interface HighlightColor {
    /** Red, 0-255 (integer). */
    r?: number;
    /** Green, 0-255 (integer). */
    g?: number;
    /** Blue, 0-255 (integer). */
    b?: number;
    /** Alpha, 0-1. Default: 1. */
    a?: number;
  }

  /**
   * Overlay highlight options — the subset of `Overlay.HighlightConfig` this
   * method lets a caller override. Omitted fields keep the built-in defaults.
   */
  interface HighlightOptions {
    /** Draw the info tooltip. Default: true. */
    showInfo?: boolean;
    contentColor?: HighlightColor;
    paddingColor?: HighlightColor;
    borderColor?: HighlightColor;
    marginColor?: HighlightColor;
  }

  /**
   * A buffered console / log message.
   *
   * One shape for both CDP sources: `Log.entryAdded` (`entry.text`) and
   * `Runtime.consoleAPICalled` (`args[]`) are both flattened into `text`.
   */
  interface ConsoleMessage {
    /** `"console-api"` for `console.*`, or the CDP `Log.Entry.source`. */
    source?: string;
    /** CDP level (`"info"`, `"warning"`, `"error"`, ...) or the console method. */
    level?: string;
    /** The message text; console arguments joined with a space. */
    text?: string;
    /** The raw CDP event params, for anything the fields above drop. */
    params?: any;
  }

  // =========================================================================
  // Module export
  // =========================================================================

  /** The Browser class. Launch / spawn / connect via static factories. */
  export class Browser {
    /**
     * Spawn a browser AND connect to it, owning the process: `close()`
     * kills it. Sugar over `spawn()` + `connect()`.
     * @returns A `Browser` instance (NOT a Page — call `browser.newPage()` next).
     */
    static launch(options?: LaunchOptions): Promise<Browser>;

    /**
     * Spawn a browser CDP server without connecting. Auto-detects system
     * Chrome when `bin` is omitted; accepts a custom command line
     * (e.g. lightpanda) via `bin` + `args` (+ explicit `port`).
     * @returns A process handle with `bin`, `cdpUrl` and `kill()`.
     */
    static spawn(options?: SpawnOptions): Promise<BrowserProcess>;

    /**
     * Attach to an already-running CDP server (from `spawn()`, an external
     * browser, or lightpanda). Does NOT own the process — `close()` only
     * disconnects.
     */
    static connect(cdpUrl: string): Promise<Browser>;
  }
}
