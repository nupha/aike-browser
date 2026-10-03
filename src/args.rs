// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! Strict argument / option parsing shared by the `Browser` and `Page` methods.
//!
//! This replaces the previous `coerce_string` / `arg_bool` / `as i64` handling,
//! which silently stringified `undefined`, applied `ToBoolean`, and saturated
//! or truncated numbers.

use zuqe_zapi::*;

/// A wrong-typed value: clear whatever pending exception a prior strict read
/// left behind, then throw a `TypeError` naming the parameter.
fn wrong_type<T>(cx: ZqCtx, name: &str, expected: &str) -> Result<T, ZqError> {
    let _ = cx.take_exception();
    Err(cx.throw_type(&format!("`{name}` must be {expected}")))
}

// ---------------------------------------------------------------------------
// Scalar readers (value in hand)
// ---------------------------------------------------------------------------

/// A JS string — no coercion. A number, `null`, `undefined`, … is a `TypeError`.
pub(crate) fn to_string(cx: ZqCtx, v: ZqValue, name: &str) -> Result<String, ZqError> {
    match v.string_read_utf8(cx) {
        Ok(s) => Ok(s),
        Err(_) => wrong_type(cx, name, "a string"),
    }
}

/// A finite JS number — no `ToNumber`. Rejects strings, booleans and `NaN`.
pub(crate) fn to_f64(cx: ZqCtx, v: ZqValue, name: &str) -> Result<f64, ZqError> {
    if v.kind() != ZqValueType::Number {
        return wrong_type(cx, name, "a number");
    }
    let n = v.to_f64(cx)?;
    if !n.is_finite() {
        return Err(cx.throw_range(&format!("`{name}` must be a finite number")));
    }
    Ok(n)
}

/// An integer-valued JS number that fits in `i64`.
pub(crate) fn to_i64(cx: ZqCtx, v: ZqValue, name: &str) -> Result<i64, ZqError> {
    let n = to_f64(cx, v, name)?;
    if n.fract() != 0.0 {
        return Err(cx.throw_range(&format!("`{name}` must be an integer")));
    }
    if !(-9.223_372_036_854_776e18..=9.223_372_036_854_776e18).contains(&n) {
        return Err(cx.throw_range(&format!("`{name}` is out of range")));
    }
    Ok(n as i64)
}

/// A JS boolean — no `ToBoolean`.
pub(crate) fn to_bool(cx: ZqCtx, v: ZqValue, name: &str) -> Result<bool, ZqError> {
    if !v.is_bool() {
        return wrong_type(cx, name, "a boolean");
    }
    v.to_bool(cx)
}

/// Range check for an already-typed value.
pub(crate) fn in_range<T>(cx: ZqCtx, v: T, name: &str, min: T, max: T) -> Result<T, ZqError>
where
    T: PartialOrd + std::fmt::Display,
{
    if v < min || v > max {
        return Err(cx.throw_range(&format!("`{name}` must be within {min}..={max}; got {v}")));
    }
    Ok(v)
}

// ---------------------------------------------------------------------------
// Argument readers (a `ZqCallInfo` position)
// ---------------------------------------------------------------------------

macro_rules! arg_reader {
    ($required:ident, $optional:ident, $conv:ident, $ty:ty) => {
        /// Required variant: a missing (`undefined`) argument is a `TypeError`.
        pub(crate) fn $required(
            cx: ZqCtx,
            cc: &ZqCallInfo,
            nth: usize,
            name: &str,
        ) -> Result<$ty, ZqError> {
            let v = cc.arg(nth);
            if v.is_undefined() {
                return Err(cx.throw_type(&format!("`{name}` is required")));
            }
            $conv(cx, v, name)
        }

        /// Optional variant: `undefined` means "not provided".
        pub(crate) fn $optional(
            cx: ZqCtx,
            cc: &ZqCallInfo,
            nth: usize,
            name: &str,
        ) -> Result<Option<$ty>, ZqError> {
            let v = cc.arg(nth);
            if v.is_undefined() {
                Ok(None)
            } else {
                $conv(cx, v, name).map(Some)
            }
        }
    };
}

arg_reader!(arg_string, opt_arg_string, to_string, String);
arg_reader!(arg_f64, opt_arg_f64, to_f64, f64);
arg_reader!(arg_i64, opt_arg_i64, to_i64, i64);
arg_reader!(arg_bool, opt_arg_bool, to_bool, bool);

// ---------------------------------------------------------------------------
// Object-property readers
// ---------------------------------------------------------------------------

macro_rules! prop_reader {
    ($name:ident, $conv:ident, $ty:ty) => {
        /// Optional property: `undefined` means "not provided"; a present value
        /// of the wrong type throws.
        pub(crate) fn $name(cx: ZqCtx, obj: &ZqValue, key: &str) -> Result<Option<$ty>, ZqError> {
            let v = obj.get_str(cx, key)?;
            if v.is_undefined() {
                Ok(None)
            } else {
                $conv(cx, v, key).map(Some)
            }
        }
    };
}

prop_reader!(opt_string_prop, to_string, String);
prop_reader!(opt_f64_prop, to_f64, f64);
prop_reader!(opt_i64_prop, to_i64, i64);
prop_reader!(opt_bool_prop, to_bool, bool);

/// Optional nested object property; a non-object value throws.
pub(crate) fn opt_object_prop<'a>(
    cx: ZqCtx<'a>,
    obj: &ZqValue<'a>,
    key: &str,
) -> Result<Option<ZqValue<'a>>, ZqError> {
    let v = obj.get_str(cx, key)?;
    if v.is_undefined() {
        Ok(None)
    } else if v.as_object().is_some() {
        Ok(Some(v))
    } else {
        wrong_type(cx, key, "an object")
    }
}

/// Read `obj`'s own string-keyed properties into a JSON map, requiring every
/// value to be a string. `name` prefixes the `TypeError` for a bad value.
///
/// Iterates the keys instead of going through `JSON.stringify` (`to_json`):
/// that conversion silently returns `None` on a value JSON cannot represent and
/// leaves the pending exception behind, and it would stringify non-string
/// values rather than reject them.
pub(crate) fn string_map(
    cx: ZqCtx,
    obj: &ZqValue,
    name: &str,
) -> Result<serde_json::Map<String, serde_json::Value>, ZqError> {
    let names = obj.own_property_names(cx, false)?;
    let len = names.length(cx)?;
    let mut map = serde_json::Map::new();
    for i in 0..len {
        let key = names.get_index(cx, i as u32)?.string_read_utf8(cx)?;
        let value = obj.get_str(cx, &key)?;
        let s = to_string(cx, value, &format!("{name}.{key}"))?;
        map.insert(key, serde_json::Value::String(s));
    }
    Ok(map)
}

/// Optional nested array property; a non-array value throws.
pub(crate) fn opt_array_prop<'a>(
    cx: ZqCtx<'a>,
    obj: &ZqValue<'a>,
    key: &str,
) -> Result<Option<ZqValue<'a>>, ZqError> {
    let v = obj.get_str(cx, key)?;
    if v.is_undefined() {
        Ok(None)
    } else if v.is_array(cx)? {
        Ok(Some(v))
    } else {
        wrong_type(cx, key, "an array")
    }
}
