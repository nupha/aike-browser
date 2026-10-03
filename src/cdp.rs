// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! CDP (Chrome DevTools Protocol) client over WebSocket.
//!
//! A background tokio task owns the WebSocket connection:
//! callers submit commands via an unbounded channel, each command gets
//! a `tokio::sync::oneshot` response channel, and the background task
//! dispatches responses by `id`.
//!
//! No command can stay pending forever: a dead socket, a torn-down command
//! loop and an unanswered command each reject the caller's promise.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use tokio::{
    net::TcpStream,
    sync::{broadcast, mpsc, oneshot},
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async, tungstenite::Message};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;
type WsWriter = futures_util::stream::SplitSink<WsStream, Message>;
type WsReader = futures_util::stream::SplitStream<WsStream>;

/// In-flight commands awaiting a response, keyed by command id. Shared with
/// [`CdpClient`] so a timeout can drop its own entry. The method name rides
/// along so a failure can name the command that produced it.
type Pending =
    Arc<Mutex<HashMap<u64, (String, oneshot::Sender<Result<serde_json::Value, String>>)>>>;

/// How long a command may stay unanswered before the caller's promise rejects.
/// Chrome sometimes stops replying (renderer gone, a request held by Fetch
/// interception); without a bound the JS promise would never settle.
const CMD_TIMEOUT: Duration = Duration::from_secs(30);

/// A CDP event received from the browser (message with "method" but no "id").
#[derive(Clone, Debug)]
pub(crate) struct CdpEvent {
    pub(crate) method: String,
    pub(crate) params: serde_json::Value,
    pub(crate) session_id: Option<String>,
    /// Only set on the synthetic event [`CdpClient::disconnect`] publishes, so
    /// subscribers stop even though the broadcast channel itself stays open
    /// (every `CdpClient` clone holds a sender).
    pub(crate) shutdown: bool,
}

/// A command submitted to the background WebSocket task.
struct CdpCommand {
    msg: String,
    method: String,
    resp_tx: oneshot::Sender<Result<serde_json::Value, String>>,
}

/// Thread-safe handle to the CDP connection.
#[derive(Clone)]
pub(crate) struct CdpClient {
    cmd_tx: mpsc::UnboundedSender<CdpCommand>,
    next_id: Arc<AtomicU64>,
    event_tx: broadcast::Sender<CdpEvent>,
    pending: Pending,
    /// Flipped by [`Self::disconnect`] to tear the background loop down.
    shutdown: tokio::sync::watch::Sender<bool>,
}

/// How long [`CdpClient::connect`] keeps retrying a *refused* WebSocket before
/// giving up. The endpoint probe (`DevToolsActivePort` / `/json/version`) can
/// observe the debug port slightly before the HTTP listener finishes binding,
/// so a single attempt intermittently failed with WSAECONNREFUSED.
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Delay between WebSocket connect attempts while the listener comes up.
const WS_CONNECT_RETRY_DELAY: Duration = Duration::from_millis(100);

/// True for "nothing is listening yet" errors, which are the transient case
/// worth retrying. Everything else (DNS, TLS, bad URL, refused upgrade) is
/// reported immediately so a real failure keeps its cause.
///
/// `tungstenite` is not a direct dependency, so this matches the rendered error
/// (its `Io` variant displays the underlying `std::io::Error`, e.g. "…
/// (os error 10061)" on Windows / "Connection refused (os error 111)" on Unix).
fn is_connect_refused_msg(err: &str) -> bool {
    let msg = err.to_ascii_lowercase();
    msg.contains("connection refused")
        || msg.contains("actively refused")
        || msg.contains("connection reset")
        || msg.contains("os error 10061")
        || msg.contains("os error 111")
        || msg.contains("os error 104")
}

impl CdpClient {
    /// Connect to Chrome's DevTools WebSocket and spawn background reader/writer.
    ///
    /// Retries a refused connection until [`WS_CONNECT_TIMEOUT`]: the caller has
    /// already observed the debug port, and on both Windows and Unix a refused
    /// TCP/upgrade at this point means the listener is still binding, not that
    /// the browser is unusable. Any other error fails immediately.
    pub(crate) async fn connect(
        ws_url: &str,
    ) -> Result<(Self, tokio::task::JoinHandle<()>), String> {
        let ws_url = Self::resolve_browser_url(ws_url).await?;

        let deadline = std::time::Instant::now() + WS_CONNECT_TIMEOUT;
        let (ws_stream, _) = loop {
            match connect_async(&ws_url).await {
                Ok(ok) => break ok,
                Err(e) => {
                    let msg = format!("{e}");
                    if !is_connect_refused_msg(&msg) || std::time::Instant::now() >= deadline {
                        return Err(format!("CDP WebSocket connect failed: {e}"));
                    }
                    tokio::time::sleep(WS_CONNECT_RETRY_DELAY).await;
                }
            }
        };
        let (writer, reader) = ws_stream.split();
        let writer = Arc::new(tokio::sync::Mutex::new(writer));

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<CdpCommand>();
        let next_id = Arc::new(AtomicU64::new(1));
        let (event_tx, _) = broadcast::channel::<CdpEvent>(256);
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);

        let bg_writer = writer.clone();
        let bg_event_tx = event_tx.clone();
        let bg_pending = pending.clone();
        let handle = tokio::spawn(async move {
            Self::run_background(
                bg_writer,
                reader,
                cmd_rx,
                bg_event_tx,
                bg_pending,
                shutdown_rx,
            )
            .await;
        });

        Ok((
            Self {
                cmd_tx,
                next_id,
                event_tx,
                pending,
                shutdown: shutdown_tx,
            },
            handle,
        ))
    }

    /// Resolve a bare-origin CDP URL (e.g. "ws://127.0.0.1:9222") to the full
    /// browser WS endpoint via the /json/version HTTP endpoint ?Chrome requires
    /// the `/devtools/browser/<uuid>` path which changes on every launch.
    /// URLs that already carry a path are used as-is.
    /// Split a `ws://` / `wss://` URL into `(http_scheme, authority, rest)`.
    ///
    /// The authority ends at the first `/`, `?` or `#`: splitting on `/` alone
    /// treated `ws://host:9222?x` as an authority of `host:9222?x`, so the probe
    /// went to `http://host:9222?x/json/version`.
    fn split_ws_url(ws_url: &str) -> Option<(&'static str, &str, &str)> {
        let (rest, scheme) = ws_url
            .strip_prefix("wss://")
            .map(|r| (r, "https://"))
            .or_else(|| ws_url.strip_prefix("ws://").map(|r| (r, "http://")))?;
        let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        Some((scheme, &rest[..end], &rest[end..]))
    }

    async fn resolve_browser_url(ws_url: &str) -> Result<String, String> {
        let (scheme, authority, rest) = Self::split_ws_url(ws_url)
            .ok_or_else(|| format!("`cdpUrl` must be a ws:// or wss:// URL, got `{ws_url}`"))?;
        if authority.is_empty() {
            return Err(format!("`cdpUrl` has no host: `{ws_url}`"));
        }
        if rest.is_empty() || rest == "/" {
            // Bare origin: discover the browser endpoint via /json/version.
        } else if rest.starts_with('/') {
            // Already a full endpoint path; use it unchanged.
            return Ok(ws_url.to_string());
        } else {
            // A query or fragment with no path (`ws://host?x`).
            return Err(format!("`cdpUrl` is not a valid WebSocket URL: `{ws_url}`"));
        }

        let http_url = format!("{scheme}{authority}/json/version");
        let resp = reqwest::Client::new()
            .get(&http_url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| format!("failed to query CDP endpoint {http_url}: {e}"))?;
        let json: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("invalid /json/version response from {http_url}: {e}"))?;
        let returned = json
            .get("webSocketDebuggerUrl")
            .and_then(|v| v.as_str())
            .ok_or_else(|| format!("no webSocketDebuggerUrl in response from {http_url}"))?;

        // The response decides where we connect next, so require it to be a
        // WebSocket URL for the host we actually probed; otherwise a foreign or
        // compromised `/json/version` could redirect the connection.
        let (_, returned_host, _) = Self::split_ws_url(returned).ok_or_else(|| {
            format!("`webSocketDebuggerUrl` is not a ws:// or wss:// URL: `{returned}`")
        })?;
        if !returned_host.eq_ignore_ascii_case(authority) {
            return Err(format!(
                "`webSocketDebuggerUrl` host `{returned_host}` does not match the requested `{authority}`"
            ));
        }
        Ok(returned.to_string())
    }

    /// Background loop: forward commands to WS and dispatch responses / events.
    ///
    /// Exits on whichever comes first: the command channel closing, the reader
    /// stopping, or [`CdpClient::disconnect`]. Every exit path rejects the
    /// in-flight commands and publishes a shutdown event, so neither a caller
    /// nor a listener can be left waiting on a socket nobody reads.
    async fn run_background(
        writer: Arc<tokio::sync::Mutex<WsWriter>>,
        mut reader: WsReader,
        mut cmd_rx: mpsc::UnboundedReceiver<CdpCommand>,
        event_tx: broadcast::Sender<CdpEvent>,
        pending: Pending,
        mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
    ) {
        let pending_r = pending.clone();
        let event_tx_r = event_tx.clone();
        // The reader is the only source of responses: once it stops, every
        // in-flight command must fail rather than wait on a dead peer. It also
        // reports *why* it stopped, so a clean Close frame is not reported as a
        // dropped channel.
        let (reader_done_tx, mut reader_done_rx) = oneshot::channel::<String>();
        let reader_task = tokio::spawn(async move {
            let reason = loop {
                match reader.next().await {
                    Some(Ok(Message::Text(text))) => {
                        if !Self::try_dispatch_response(&pending_r, &text) {
                            Self::dispatch_event(&event_tx_r, &text);
                        }
                    }
                    // A Close frame is the peer saying goodbye, not a fault.
                    Some(Ok(Message::Close(frame))) => {
                        break match frame {
                            Some(f) => format!(
                                "CDP connection closed by the browser (code {}, {})",
                                u16::from(f.code),
                                f.reason
                            ),
                            None => "CDP connection closed by the browser".to_string(),
                        };
                    }
                    Some(Err(e)) => {
                        log::warn!("CDP WS read error: {e}");
                        break format!("CDP WebSocket read failed: {e}");
                    }
                    Some(_) => {}
                    None => break "CDP WebSocket stream ended".to_string(),
                }
            };
            let _ = reader_done_tx.send(reason);
        });

        let reason = loop {
            let cmd = tokio::select! {
                cmd = cmd_rx.recv() => match cmd {
                    Some(cmd) => cmd,
                    None => break "CDP command channel closed".to_string(),
                },
                done = &mut reader_done_rx => {
                    break done.unwrap_or_else(|_| "CDP reader task vanished".to_string());
                }
                // `Err` here means the sender is gone, which also means "gone".
                _ = shutdown_rx.changed() => {
                    break "CDP connection closed by the client".to_string();
                }
            };
            if Self::send_cmd(&writer, cmd, &pending).await.is_err() {
                break "CDP WebSocket write failed".to_string();
            }
        };
        log::debug!("CDP background loop shutting down: {reason}");

        // The reader task owns the read half, so aborting it and dropping the
        // writer is what actually closes the connection.
        reader_task.abort();
        drop(writer);

        // Wake every listener: the broadcast channel stays open because the
        // `CdpClient` clones each hold a sender, so `Closed` never arrives.
        let _ = event_tx.send(CdpEvent {
            method: String::new(),
            params: serde_json::Value::Null,
            session_id: None,
            shutdown: true,
        });

        // Nothing will answer the survivors now.
        for (_, (method, tx)) in pending.lock().unwrap().drain() {
            let _ = tx.send(Err(format!("CDP `{method}`: {reason}")));
        }
    }

    /// Write one command over the socket.
    ///
    /// Returns `Err` when the socket died: the caller tears the whole loop down,
    /// because a half-open WebSocket would otherwise swallow every later
    /// command without ever answering.
    async fn send_cmd(
        writer: &Arc<tokio::sync::Mutex<WsWriter>>,
        cmd: CdpCommand,
        pending: &Pending,
    ) -> Result<(), String> {
        let id = serde_json::from_str::<serde_json::Value>(&cmd.msg)
            .ok()
            .and_then(|v| v.get("id").and_then(|i| i.as_u64()))
            .unwrap_or(0);

        // Claim the id before writing so a response can never arrive unclaimed.
        pending
            .lock()
            .unwrap()
            .insert(id, (cmd.method, cmd.resp_tx));

        if let Err(e) = writer
            .lock()
            .await
            .send(Message::Text(cmd.msg.into()))
            .await
        {
            pending.lock().unwrap().remove(&id);
            return Err(format!("CDP WS send error: {e}"));
        }
        Ok(())
    }

    /// Parse a received WS text message and dispatch to the matching pending command.
    /// Returns `true` if it was a command response (had "id"), `false` if it was not.
    fn try_dispatch_response(pending: &Pending, text: &str) -> bool {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
            return false;
        };

        let id = match val.get("id").and_then(|v| v.as_u64()) {
            Some(id) => id,
            None => return false, // Not a command response, it's an event
        };

        let tx = pending.lock().unwrap().remove(&id);
        if let Some((method, tx)) = tx {
            let result = match val.get("error") {
                Some(error) => Err(Self::format_error(&method, error)),
                None => Ok(val
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)),
            };
            let _ = tx.send(result);
        }
        true
    }

    /// Render a CDP `error` object. Chrome puts a human sentence in `message`,
    /// but the numeric `code` and the domain-specific `data` are what make a
    /// failure actionable (which parameter was missing, which node was stale),
    /// so keep them instead of passing on `message` alone.
    fn format_error(method: &str, error: &serde_json::Value) -> String {
        let message = error
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("CDP error");
        let mut out = format!("CDP `{method}` failed: {message}");
        if let Some(code) = error.get("code").and_then(|v| v.as_i64()) {
            out.push_str(&format!(" (code {code})"));
        }
        if let Some(data) = error.get("data").filter(|d| !d.is_null()) {
            out.push_str(&format!(" [data: {data}]"));
        }
        out
    }

    /// Forward a CDP event (message without "id") to the broadcast channel.
    fn dispatch_event(event_tx: &broadcast::Sender<CdpEvent>, text: &str) {
        let Ok(val) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        let method = match val.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return,
        };
        let params = val
            .get("params")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let session_id = val
            .get("sessionId")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let _ = event_tx.send(CdpEvent {
            method,
            params,
            session_id,
            shutdown: false,
        });
    }

    /// Build a CDP JSON-RPC command message string.
    fn build_msg(
        method: &str,
        params: Option<serde_json::Value>,
        session_id: Option<&str>,
        id: u64,
    ) -> String {
        let mut msg = serde_json::json!({
            "id": id,
            "method": method,
        });
        if let Some(p) = params {
            msg["params"] = p;
        }
        if let Some(sid) = session_id {
            msg["sessionId"] = serde_json::Value::String(sid.to_string());
        }
        serde_json::to_string(&msg).unwrap()
    }

    /// Send a CDP command and await its response, bounded by [`CMD_TIMEOUT`].
    pub(crate) async fn send(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        session_id: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        self.send_timeout(method, params, session_id, CMD_TIMEOUT)
            .await
    }

    /// [`Self::send`] with an explicit timeout.
    ///
    /// A caller with its own deadline (a selector poll, for example) must not
    /// inherit the 30s global timeout: one stalled Chrome round-trip would then
    /// blow past the caller's budget entirely.
    pub(crate) async fn send_timeout(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        session_id: Option<&str>,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        // Nothing reads the command channel after a disconnect, so a command
        // issued then would sit until the timeout instead of failing at once.
        if self.is_closed() {
            return Err(format!("CDP `{method}`: connection already closed"));
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let msg = Self::build_msg(method, params, session_id, id);
        let (tx, mut rx) = oneshot::channel();

        if self
            .cmd_tx
            .send(CdpCommand {
                msg,
                method: method.to_string(),
                resp_tx: tx,
            })
            .is_err()
        {
            return Err("CDP command channel closed".to_string());
        }

        match tokio::time::timeout(timeout, &mut rx).await {
            Ok(res) => res.map_err(|_| "CDP connection closed".to_string())?,
            Err(_) => {
                // Drop the claim so a late response cannot resurrect the entry.
                self.pending.lock().unwrap().remove(&id);
                Err(format!("CDP `{method}` timed out after {timeout:?}"))
            }
        }
    }

    /// Send a command without waiting for its response.
    ///
    /// For fire-and-forget commands whose answer the caller cannot act on (the
    /// interception safety net resuming a request nobody claimed, a listener's
    /// `accept()`). Spawns on the extension's runtime so the JS thread never
    /// blocks; errors are dropped, which is the point.
    pub(crate) fn send_detached(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        session_id: Option<&str>,
    ) {
        let this = self.clone();
        let method = method.to_string();
        let params = params.clone();
        let session_id = session_id.map(|s| s.to_string());
        crate::helpers::spawn(async move {
            let _ = this.send(&method, params, session_id.as_deref()).await;
        });
    }

    /// Subscribe to CDP events (messages without "id").
    /// Returns a receiver that gets all CDP events from all sessions.
    ///
    /// The receiver also yields one synthetic event with `shutdown: true` when
    /// the connection goes away — including on [`Self::disconnect`], which the
    /// broadcast channel alone cannot report (its senders outlive it).
    pub(crate) fn subscribe_events(&self) -> broadcast::Receiver<CdpEvent> {
        self.event_tx.subscribe()
    }

    /// Close the connection. Idempotent.
    ///
    /// Used by `browser.close()` in attach mode: without it the WebSocket and
    /// its reader task outlive the `Browser` object, because every `Page` holds
    /// a client clone and the command channel is never closed.
    pub(crate) fn disconnect(&self) {
        let _ = self.shutdown.send(true);
    }

    /// True once [`Self::disconnect`] has been called or the loop has exited.
    pub(crate) fn is_closed(&self) -> bool {
        *self.shutdown.borrow()
    }

    /// Close a target (tab).
    pub(crate) async fn close_target(&self, target_id: &str) -> Result<(), String> {
        let resp = self
            .send(
                "Target.closeTarget",
                Some(serde_json::json!({ "targetId": target_id })),
                None,
            )
            .await?;
        // The command can succeed at the protocol level while the target
        // survives; `success: false` with an `errorText` is a real failure.
        if resp.get("success").and_then(|v| v.as_bool()) == Some(false) {
            let why = resp
                .get("errorText")
                .and_then(|v| v.as_str())
                .unwrap_or("target was not closed");
            return Err(format!("Target.closeTarget: {why}"));
        }
        Ok(())
    }
}

/// The shared tokio runtime backing all CDP async work. A zapi extension has
/// no access to the host's executor, so it owns one lazily-started multi-thread
/// runtime. Every `tokio::spawn` in the module runs on this.
static RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub(crate) fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("aike-browser: failed to start tokio runtime")
    })
}
