// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Input domain methods.
//!
//! Mouse click, mouse move, keyboard press, text insertion.
//! These methods use native CDP Input domain calls.

use zuqe_zapi::*;

use crate::{
    cdp::CdpClient,
    cdp_input,
    class_page::{get_number_prop, get_page_payload, get_str_prop, send_page_void_cmd},
    dom::resolve_selector,
};

/// Resolve a selector to its on-screen center via `DOM.getBoxModel`.
async fn box_center(
    client: &CdpClient,
    session_id: &str,
    selector: &str,
) -> Result<(f64, f64), String> {
    let node_id = resolve_selector(client, session_id, selector).await?;
    let m = client
        .send(
            "DOM.getBoxModel",
            Some(serde_json::json!({ "nodeId": node_id })),
            Some(session_id),
        )
        .await?;
    let quad = m
        .get("model")
        .and_then(|mo| mo.get("content"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| "no box model content".to_string())?;
    let xs: Vec<f64> = quad.iter().step_by(2).filter_map(|v| v.as_f64()).collect();
    let ys: Vec<f64> = quad
        .iter()
        .skip(1)
        .step_by(2)
        .filter_map(|v| v.as_f64())
        .collect();
    if xs.is_empty() || ys.is_empty() {
        return Err("empty box model".into());
    }
    Ok((
        xs.iter().sum::<f64>() / xs.len() as f64,
        ys.iter().sum::<f64>() / ys.len() as f64,
    ))
}

/// Parse `options = { button?, clickCount? }`.
fn click_options<'a>(cx: ZqCtx<'a>, cc: &ZqCallInfo<'a>) -> (String, i32) {
    let mut button = "left".to_string();
    let mut click_count = 1i32;
    if cc.len() > 2
        && let Some(opts) = cc.arg(2).as_object()
    {
        if let Some(b) = get_str_prop(cx, &opts, "button") {
            button = b;
        }
        if let Some(c) = get_number_prop(cx, &opts, "clickCount") {
            click_count = c as i32;
        }
    }
    (button, click_count)
}

/// `page.mouseClick(x, y, options?)`
pub(crate) fn js_page_mouse_click<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let x = cc.arg_f64(cx, 0).unwrap_or(0.0);
    let y = cc.arg_f64(cx, 1).unwrap_or(0.0);
    let (button, click_count) = click_options(cx, &cc);
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_void(async move {
        cdp_input::mouse_click(&client, &session_id, x, y, &button, click_count).await
    })
}

/// `page.mouseMove(x, y)`
pub(crate) fn js_page_mouse_move<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let x = cc.arg_f64(cx, 0).unwrap_or(0.0);
    let y = cc.arg_f64(cx, 1).unwrap_or(0.0);
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_void(async move { cdp_input::mouse_move(&client, &session_id, x, y).await })
}

/// `page.mouseDown(x, y, options?)`
pub(crate) fn js_page_mouse_down<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let x = cc.arg_f64(cx, 0).unwrap_or(0.0);
    let y = cc.arg_f64(cx, 1).unwrap_or(0.0);
    let (button, click_count) = click_options(cx, &cc);
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_void(async move {
        cdp_input::mouse_down(&client, &session_id, x, y, &button, click_count).await
    })
}

/// `page.mouseUp(x, y, options?)`
pub(crate) fn js_page_mouse_up<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let x = cc.arg_f64(cx, 0).unwrap_or(0.0);
    let y = cc.arg_f64(cx, 1).unwrap_or(0.0);
    let (button, click_count) = click_options(cx, &cc);
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_void(async move {
        cdp_input::mouse_up(&client, &session_id, x, y, &button, click_count).await
    })
}

/// `page.keyPress(key, options?)`
pub(crate) fn js_page_key_press<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let key = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let mut modifiers = 0i32;
    if cc.len() > 1
        && let Some(opts) = cc.arg(1).as_object()
        && let Some(m) = get_number_prop(cx, &opts, "modifiers")
    {
        modifiers = m as i32;
    }
    // Reject an unusable key here rather than letting Chrome answer with
    // `Invalid 'text' parameter` or typing the wrong character.
    cdp_input::resolve_key(&key).map_err(|e| cx.throw_type(&e))?;
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_void(
        async move { cdp_input::press_key(&client, &session_id, &key, modifiers).await },
    )
}

/// `page.dragAndDrop(sourceSelector, targetSelector)`
pub(crate) fn js_page_drag_and_drop<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let source = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let target = cc.arg(1).coerce_string(cx).unwrap_or_default();
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_void(async move {
        let (sx, sy) = box_center(&client, &session_id, &source).await?;
        let (tx, ty) = box_center(&client, &session_id, &target).await?;
        cdp_input::mouse_move(&client, &session_id, sx, sy).await?;
        cdp_input::mouse_down(&client, &session_id, sx, sy, "left", 1).await?;
        cdp_input::mouse_move(&client, &session_id, (sx + tx) / 2.0, (sy + ty) / 2.0).await?;
        cdp_input::mouse_move(&client, &session_id, tx, ty).await?;
        cdp_input::mouse_up(&client, &session_id, tx, ty, "left", 1).await
    })
}

/// `page.insertText(text)`
pub(crate) fn js_page_insert_text<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let text = cc.arg(0).coerce_string(cx).unwrap_or_default();
    send_page_void_cmd(
        cx,
        &cc,
        "Input.insertText",
        Some(serde_json::json!({ "text": text })),
    )
}
