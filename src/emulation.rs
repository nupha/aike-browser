// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Page class ?Emulation domain methods (flat on `Page`).

use zuqe_zapi::*;

use crate::{args, class_page::send_page_void_cmd};

/// `page.setGeolocation(latitude, longitude, accuracy?)`
pub(crate) fn js_page_set_geolocation<'a>(
    cx: ZqCtx<'a>,
    cc: ZqCallInfo<'a>,
) -> Result<ZqValue<'a>, ZqError> {
    // Required coordinates. Omitting them used to send `{0,0,1}`, whose protocol
    // meaning is "position unavailable" — not "use the browser default".
    let latitude = args::arg_f64(cx, &cc, 0, "latitude")?;
    let longitude = args::arg_f64(cx, &cc, 1, "longitude")?;
    args::in_range(cx, latitude, "latitude", -90.0, 90.0)?;
    args::in_range(cx, longitude, "longitude", -180.0, 180.0)?;
    let accuracy = match args::opt_arg_f64(cx, &cc, 2, "accuracy")? {
        Some(a) if a > 0.0 => a,
        Some(_) => return Err(cx.throw_range("`accuracy` must be > 0")),
        None => 1.0,
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
    // A non-array used to become `features: []`, which silently *clears* every
    // emulated media feature instead of reporting the caller's mistake.
    let arr = cc
        .arg(0)
        .as_array(cx)?
        .ok_or_else(|| cx.throw_type("`features` must be an array"))?;
    let len = arr.length(cx)?;
    let mut features = Vec::with_capacity(len);
    for i in 0..len {
        let item = arr.get_index(cx, i as u32)?;
        let obj = item
            .as_object()
            .ok_or_else(|| cx.throw_type(&format!("`features[{i}]` must be an object")))?;
        let name = args::opt_string_prop(cx, &obj, "name")?
            .ok_or_else(|| cx.throw_type(&format!("`features[{i}].name` is required")))?;
        let value = args::opt_string_prop(cx, &obj, "value")?
            .ok_or_else(|| cx.throw_type(&format!("`features[{i}].value` is required")))?;
        features.push(serde_json::json!({ "name": name, "value": value }));
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
    let tz = args::arg_string(cx, &cc, 0, "timezoneId")?;
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
    let locale = args::arg_string(cx, &cc, 0, "locale")?;
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
    let enabled = args::arg_bool(cx, &cc, 0, "enabled")?;
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
    // A rate is a positive multiplier; omitting it used to JSON-encode `NaN`
    // (which serde renders as `null`), and `-1`/`0` were passed straight on.
    let rate = args::arg_f64(cx, &cc, 0, "rate")?;
    if rate <= 0.0 {
        return Err(cx.throw_range("`rate` must be > 0"));
    }
    send_page_void_cmd(
        cx,
        &cc,
        "Emulation.setCPUThrottlingRate",
        Some(serde_json::json!({ "rate": rate })),
    )
}
