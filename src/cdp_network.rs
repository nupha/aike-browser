// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! CDP Network domain helpers ?request/response body, cookie.

use crate::cdp::CdpClient;

/// Get the HTTP response body for a given request ID.
/// Returns `{ body, base64Encoded }`.
pub(crate) async fn get_response_body(
    client: &CdpClient,
    session_id: &str,
    request_id: &str,
) -> Result<serde_json::Value, String> {
    client
        .send(
            "Network.getResponseBody",
            Some(serde_json::json!({ "requestId": request_id })),
            Some(session_id),
        )
        .await
}

/// Enable the Network domain (prerequisite for network events).
pub(crate) async fn enable_network(
    client: &CdpClient,
    session_id: &str,
) -> Result<serde_json::Value, String> {
    client.send("Network.enable", None, Some(session_id)).await
}

/// List cookies. `Storage.getCookies` replaces the deprecated
/// `Network.getAllCookies`; sent on the page session it scopes the result to
/// that session's browser context.
pub(crate) async fn get_cookies(
    client: &CdpClient,
    session_id: &str,
) -> Result<serde_json::Value, String> {
    client
        .send("Storage.getCookies", None, Some(session_id))
        .await
}

/// Set a single cookie. Params: name, value, url/domain/path, httpOnly, secure, etc.
pub(crate) async fn set_cookie(
    client: &CdpClient,
    session_id: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, String> {
    client
        .send("Network.setCookie", Some(params), Some(session_id))
        .await
}
