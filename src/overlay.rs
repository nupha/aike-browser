// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class — Overlay domain methods (flat on `Page`).

use zuqe_zapi::*;

use crate::{
    class_page::{
        get_bool_prop, get_number_prop, get_page_client, get_page_session, send_page_void_cmd,
    },
    dom,
};

/// `Overlay.HighlightConfig` field names this method lets a caller override.
/// The rest of the config (and its defaults) is fixed, so an unknown key is a
/// caller mistake worth reporting rather than dropping.
const HIGHLIGHT_COLORS: &[&str] = &["contentColor", "paddingColor", "borderColor", "marginColor"];

/// `page.highlight(selector, options?)`
///
/// `options` is a subset of `Overlay.HighlightConfig`: `showInfo` and the four
/// `RGBA` colours. It used to be ignored outright, so every call painted the
/// same hardcoded box.
pub(crate) fn js_page_highlight<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let selector = cc.arg(0).coerce_string(cx).unwrap_or_default();
    let mut config = default_highlight_config();
    if cc.len() > 1
        && let Some(opts) = cc.arg(1).as_object()
    {
        if let Some(info) = get_bool_prop(cx, &opts, "showInfo") {
            config["showInfo"] = serde_json::Value::Bool(info);
        }
        for &key in HIGHLIGHT_COLORS {
            let Some(color) = opts.try_get_object_str(cx, key) else {
                continue;
            };
            config[key] = rgba(cx, &color, key)?;
        }
    }
    let client = get_page_client(cx, cc.this())?;
    let session_id = get_page_session(cx, cc.this())?;
    cx.promise_fut_void(async move {
        // Neither call may be swallowed: with the domain off, or the node not
        // paintable, `highlight` used to resolve without drawing anything and
        // without a word about why.
        //
        // `Overlay` is layered on `DOM` and answers "DOM should be enabled
        // first" otherwise — a precondition the old swallowed error hid.
        let _ = client.send("DOM.enable", None, Some(&session_id)).await;
        client
            .send("Overlay.enable", None, Some(&session_id))
            .await?;
        let node_id = dom::resolve_selector(&client, &session_id, &selector).await?;
        client
            .send(
                "Overlay.highlightNode",
                Some(serde_json::json!({ "nodeId": node_id, "highlightConfig": config })),
                Some(&session_id),
            )
            .await?;
        Ok(())
    })
}

/// `{ r?, g?, b?, a? }` → `Overlay.HighlightConfig`'s `RGBA`, which requires all
/// four channels.
///
/// `r`/`g`/`b` are 0-255 integers and `a` a 0-1 double: this Chrome's bindings
/// reject a fractional `r` outright (`int32 value expected`), which is also why
/// the defaults below are written the same way.
fn rgba(cx: ZqCtx, color: &ZqValue, key: &str) -> Result<serde_json::Value, ZqError> {
    let channel = |name: &str, default: f64| get_number_prop(cx, color, name).unwrap_or(default);
    let mut out = serde_json::Map::new();
    for name in ["r", "g", "b"] {
        let v = channel(name, 0.0);
        if !(0.0..=255.0).contains(&v) || v.fract() != 0.0 {
            return Err(cx.throw_range(&format!(
                "highlight `{key}.{name}` must be an integer within 0..255"
            )));
        }
        out.insert(name.into(), serde_json::json!(v));
    }
    let a = channel("a", 1.0);
    if !(0.0..=1.0).contains(&a) {
        return Err(cx.throw_range(&format!("highlight `{key}.a` must be within 0..1")));
    }
    out.insert("a".into(), serde_json::json!(a));
    Ok(serde_json::Value::Object(out))
}

/// Default highlight config: a translucent blue content box plus borders.
fn default_highlight_config() -> serde_json::Value {
    serde_json::json!({
        "showInfo": true,
        "contentColor": { "r": 111, "g": 168, "b": 220, "a": 0.5 },
        "paddingColor": { "r": 147, "g": 196, "b": 125, "a": 0.5 },
        "borderColor": { "r": 255, "g": 229, "b": 153, "a": 0.5 },
        "marginColor": { "r": 246, "g": 178, "b": 107, "a": 0.5 },
    })
}

/// `page.hideHighlight()`
pub(crate) fn js_page_hide_highlight<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    send_page_void_cmd(cx, &cc, "Overlay.hideHighlight", None)
}
