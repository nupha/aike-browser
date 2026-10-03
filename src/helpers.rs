// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2025-2026 John Ray <996351336@qq.com>

//! Extension-local policy: the tokio runtime backing all CDP async work.
//!
//! zapi is deliberately runtime-agnostic (`promise_fut_on` takes a caller-supplied
//! executor), so the extension owns its executor here and hands it to
//! [`ZqCtx::set_default_executor`] once, at module init. After that every promise
//! goes through zapi's own `promise_fut*` sugar.

use std::future::Future;
use std::pin::Pin;

use crate::cdp;

/// Executor argument for the zapi promise bridge: every async CDP task runs on
/// the shared tokio runtime.
pub(crate) fn executor(fut: Pin<Box<dyn Future<Output = ()> + Send + 'static>>) {
    spawn(fut);
}

/// Spawn a future on the extension's tokio runtime.
pub(crate) fn spawn<F>(fut: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    cdp::runtime().spawn(fut)
}

/// Render a `Runtime.exceptionDetails` payload as the message a JS caller
/// should see. `text` alone is always the generic `"Uncaught"`; the real
/// message lives in the exception object's `description`.
pub(crate) fn format_exception(details: &serde_json::Value) -> String {
    let exception = details.get("exception");
    let message = exception
        .and_then(|e| e.get("description"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            exception
                .and_then(|e| e.get("value"))
                .and_then(|v| v.as_str())
        })
        .map(|s| s.to_string())
        .or_else(|| {
            details
                .get("text")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_else(|| "JS exception".to_string());

    match (
        details.get("lineNumber").and_then(|v| v.as_i64()),
        details.get("columnNumber").and_then(|v| v.as_i64()),
    ) {
        (Some(line), Some(col)) if line > 0 => {
            format!("{message} (line {line}, column {})", col + 1)
        }
        _ => message,
    }
}
