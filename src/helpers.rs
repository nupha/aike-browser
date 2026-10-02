// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Extension-local policy: the tokio runtime backing all CDP async work.
//!
//! zapi is deliberately runtime-agnostic (`spawn_promise_async*` takes a
//! caller-supplied executor), so the extension owns its executor here.

use std::future::Future;

use zuqe_zapi::{ZqCtx, ZqValue};

use crate::cdp;

/// Spawn a future on the extension's tokio runtime.
pub(crate) fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    cdp::runtime().spawn(fut)
}

/// Executor argument for `ZqCtx::spawn_promise_async*` (all async work runs on
/// the shared tokio runtime).
pub(crate) fn executor(
    fut: std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'static>>,
) {
    spawn(fut);
}

/// Bridge an async `Result<T, String>` task to a JS Promise on the shared tokio
/// runtime. Thin sugar over `ZqCtx::spawn_promise_async_result` that pins the
/// executor so call sites stay short.
pub(crate) fn spawn_result<'a, T, F, C>(
    cx: ZqCtx<'a>,
    fut: F,
    convert: C,
) -> Result<ZqValue<'a>, zuqe_zapi::ZqError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, String>> + Send + 'static,
    C: for<'b> FnOnce(ZqCtx<'b>, T) -> Result<ZqValue<'b>, zuqe_zapi::ZqError> + Send + 'static,
{
    cx.spawn_promise_async_result(fut, executor, convert)
}

/// Alias for [`spawn_result`] with the context first (reads naturally at call
/// sites that need a custom convert closure).
pub(crate) fn promise<'a, T, F, C>(
    cx: ZqCtx<'a>,
    fut: F,
    convert: C,
) -> Result<ZqValue<'a>, zuqe_zapi::ZqError>
where
    T: Send + 'static,
    F: Future<Output = Result<T, String>> + Send + 'static,
    C: for<'b> FnOnce(ZqCtx<'b>, T) -> Result<ZqValue<'b>, zuqe_zapi::ZqError> + Send + 'static,
{
    spawn_result(cx, fut, convert)
}

/// Promise resolving to `undefined`.
pub(crate) fn promise_void<'a>(
    cx: ZqCtx<'a>,
    fut: impl Future<Output = Result<(), String>> + Send + 'static,
) -> Result<ZqValue<'a>, zuqe_zapi::ZqError> {
    promise(cx, fut, |_cx, ()| Ok(ZqValue::undefined()))
}

/// Promise resolving to a JS string.
pub(crate) fn promise_string<'a>(
    cx: ZqCtx<'a>,
    fut: impl Future<Output = Result<String, String>> + Send + 'static,
) -> Result<ZqValue<'a>, zuqe_zapi::ZqError> {
    promise(cx, fut, |cx, s| ZqValue::new_string(cx, &s))
}

/// Promise resolving to a JS value built from `serde_json::Value`.
pub(crate) fn promise_json<'a>(
    cx: ZqCtx<'a>,
    fut: impl Future<Output = Result<serde_json::Value, String>> + Send + 'static,
) -> Result<ZqValue<'a>, zuqe_zapi::ZqError> {
    promise(cx, fut, |cx, v| ZqValue::from_json(cx, &v))
}

/// Print a taken JS exception's message under debug builds instead of silently
/// discarding it. Event callbacks cannot propagate a throw, so this is the only
/// available diagnostic.
#[cold]
#[cfg(debug_assertions)]
pub(crate) fn print_exc(cx: ZqCtx, exc: ZqValue<'_>) {
    if exc.is_undefined() {
        return;
    }
    let msg = exc
        .get_str(cx, "message")
        .ok()
        .and_then(|v| v.try_string_read_utf8(cx))
        .unwrap_or_else(|| "<unprintable exception>".to_string());
    eprintln!("[browser:cleared-exc] {msg}");
}

/// No-op in release builds (keeps call sites free of `#[cfg]`).
#[cold]
#[cfg(not(debug_assertions))]
pub(crate) fn print_exc(_cx: ZqCtx, _exc: ZqValue<'_>) {}
