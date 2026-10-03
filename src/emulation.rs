// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class ?Emulation domain methods (flat on `Page`).

use zuqe_zapi::*;

use crate::class_page::{get_str_prop, send_page_void_cmd};

/// `page.setGeolocation(latitude, longitude, accuracy?)`
pub(crate) fn js_page_set_geolocation<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let latitude = cc.arg_f64(cx, 0).unwrap_or(0.0);
    let longitude = cc.arg_f64(cx, 1).unwrap_or(0.0);
    let accuracy = if cc.len() > 2 {
        cc.arg_f64(cx, 2).unwrap_or(1.0)
    } else {
        1.0
    };
    send_page_void_cmd(
        cx,
        &cc,
        "Emulation.setGeolocationOverride",
        Some(serde_json::json!({
            "latitude": latitude,
            "longitude": longitude,
            "accuracy": accuracy,
        })),
    )
}

/// `page.setMediaFeatures(features)`
pub(crate) fn js_page_set_media_features<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let mut features = Vec::new();
    if cc.len() > 0
        && let Some(arr) = cc.arg(0).as_object()
        && let Ok(len) = arr.length(cx)
    {
        for i in 0..len {
            if let Ok(v) = arr.get_index(cx, i as u32)
                && let Some(obj) = v.as_object()
            {
                let name = get_str_prop(cx, &obj, "name").unwrap_or_default();
                let value = get_str_prop(cx, &obj, "value").unwrap_or_default();
                features.push(serde_json::json!({ "name": name, "value": value }));
            }
        }
    }
    send_page_void_cmd(
        cx,
        &cc,
        "Emulation.setEmulatedMedia",
        Some(serde_json::json!({ "features": features })),
    )
}

/// `page.setTimezone(timezone)`
pub(crate) fn js_page_set_timezone<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let tz = cc.arg(0).coerce_string(cx).unwrap_or_default();
    send_page_void_cmd(
        cx,
        &cc,
        "Emulation.setTimezoneOverride",
        Some(serde_json::json!({ "timezoneId": tz })),
    )
}

/// `page.setLocale(locale)`
pub(crate) fn js_page_set_locale<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let locale = cc.arg(0).coerce_string(cx).unwrap_or_default();
    send_page_void_cmd(
        cx,
        &cc,
        "Emulation.setLocaleOverride",
        Some(serde_json::json!({ "locale": locale })),
    )
}

/// `page.setTouchEnabled(bool)`
pub(crate) fn js_page_set_touch_enabled<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let enabled = cc.arg_bool(cx, 0).unwrap_or(false);
    send_page_void_cmd(
        cx,
        &cc,
        "Emulation.setTouchEmulationEnabled",
        Some(serde_json::json!({ "enabled": enabled })),
    )
}

/// `page.setCPUThrottling(rate)`
pub(crate) fn js_page_set_cpu_throttling<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    let rate = if cc.len() > 0 {
        cc.arg_f64(cx, 0).unwrap_or(1.0)
    } else {
        1.0
    };
    send_page_void_cmd(
        cx,
        &cc,
        "Emulation.setCPUThrottlingRate",
        Some(serde_json::json!({ "rate": rate })),
    )
}
