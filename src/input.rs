// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Input domain methods.
//!
//! Mouse click, mouse move, keyboard press, text insertion.
//! These methods use native CDP Input domain calls.

use zuqe_zapi::*;

use crate::{
    args, cdp_input,
    class_page::{get_enum_prop, get_i64_prop, get_page_payload, send_page_void_cmd},
    dom,
};

/// `Input.dispatchMouseEvent.button` values.
const BUTTONS: &[&str] = &["left", "middle", "right"];

/// Parse `options = { button?, clickCount? }`.
fn click_options<'a>(cx: ZqCtx<'a>, cc: &ZqCallInfo<'a>) -> Result<(String, i32), ZqError> {
    let mut button = "left".to_string();
    let mut click_count = 1i32;
    if cc.len() > 2
        && let Some(opts) = cc.arg(2).as_object()
    {
        if let Some(b) = get_enum_prop(cx, &opts, "button", BUTTONS)? {
            button = b;
        }
        if let Some(c) = get_i64_prop(cx, &opts, "clickCount")? {
            click_count = args::in_range(cx, c, "clickCount", 1, 1000)? as i32;
        }
    }
    Ok((button, click_count))
}

/// `page.mouseClick(x, y, options?)`
pub(crate) fn js_page_mouse_click<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let x = args::arg_f64(cx, &cc, 0, "x")?;
    let y = args::arg_f64(cx, &cc, 1, "y")?;
    let (button, click_count) = click_options(cx, &cc)?;
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
    let x = args::arg_f64(cx, &cc, 0, "x")?;
    let y = args::arg_f64(cx, &cc, 1, "y")?;
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
    let x = args::arg_f64(cx, &cc, 0, "x")?;
    let y = args::arg_f64(cx, &cc, 1, "y")?;
    let (button, click_count) = click_options(cx, &cc)?;
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
    let x = args::arg_f64(cx, &cc, 0, "x")?;
    let y = args::arg_f64(cx, &cc, 1, "y")?;
    let (button, click_count) = click_options(cx, &cc)?;
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
    let key = args::arg_string(cx, &cc, 0, "key")?;
    let mut modifiers = 0i32;
    if cc.len() > 1
        && let Some(opts) = cc.arg(1).as_object()
        && let Some(m) = get_i64_prop(cx, &opts, "modifiers")?
    {
        modifiers = args::in_range(cx, m, "modifiers", 0, i32::MAX as i64)? as i32;
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
///
/// The press / masked move / explicit drop sequence below is what actually
/// makes HTML5 drag-and-drop fire. Moving without the `buttons` mask told the
/// renderer no button was held, so it cancelled the drag: the page saw no
/// `dragstart`, `dragover` or `drop` at all. Verified against Chrome 153 —
/// `mousedown, dragstart, dragenter, dragover, drop, dragend, mouseup`.
pub(crate) fn js_page_drag_and_drop<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    // Both selectors are required: omitting them used to drag the root element
    // onto itself and resolve.
    let source = args::arg_string(cx, &cc, 0, "sourceSelector")?;
    let target = args::arg_string(cx, &cc, 1, "targetSelector")?;
    let payload = get_page_payload(cx, cc.this())?;
    let client = payload.client.clone();
    let session_id = payload.session_id.clone();
    cx.promise_fut_void(async move {
        let (sx, sy) = dom::content_center(&client, &session_id, &source).await?;
        let (tx, ty) = dom::content_center(&client, &session_id, &target).await?;
        cdp_input::mouse_move(&client, &session_id, sx, sy).await?;
        cdp_input::mouse_down(&client, &session_id, sx, sy, "left", 1).await?;
        cdp_input::mouse_move_dragging(&client, &session_id, tx, ty, LEFT_BUTTON_MASK).await?;
        // The drop is dispatched explicitly rather than inferred from the
        // release; a release alone leaves the gesture to the renderer.
        cdp_input::drag_event(&client, &session_id, "drop", tx, ty).await?;
        cdp_input::mouse_up(&client, &session_id, tx, ty, "left", 1).await
    })
}

/// `Input.dispatchMouseEvent.buttons` bit for the left button.
const LEFT_BUTTON_MASK: i32 = 1;

/// `page.insertText(text)`
pub(crate) fn js_page_insert_text<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let text = args::arg_string(cx, &cc, 0, "text")?;
    send_page_void_cmd(
        cx,
        &cc,
        "Input.insertText",
        Some(serde_json::json!({ "text": text })),
    )
}
