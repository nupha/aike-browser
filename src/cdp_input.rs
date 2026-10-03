// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: Copyright (c) 2026 John Ray <996351336@qq.com>

//! CDP Input domain helpers ?mouse, keyboard, text input.

use crate::cdp::CdpClient;

/// `(key, code, windowsVirtualKeyCode, text)` for a key that has a name.
/// `text` is what the key inserts; empty for keys that only act.
type NamedKey = (&'static str, &'static str, i32, &'static str);

/// Named (non-printable) keys. A printable single character is handled
/// directly from its code point.
const NAMED_KEYS: &[NamedKey] = &[
    ("Enter", "Enter", 13, "\r"),
    ("Tab", "Tab", 9, "\t"),
    ("Escape", "Escape", 27, ""),
    ("Backspace", "Backspace", 8, ""),
    ("Delete", "Delete", 46, ""),
    ("ArrowLeft", "ArrowLeft", 37, ""),
    ("ArrowUp", "ArrowUp", 38, ""),
    ("ArrowRight", "ArrowRight", 39, ""),
    ("ArrowDown", "ArrowDown", 40, ""),
    ("Home", "Home", 36, ""),
    ("End", "End", 35, ""),
    ("PageUp", "PageUp", 33, ""),
    ("PageDown", "PageDown", 34, ""),
    ("Insert", "Insert", 45, ""),
    ("Shift", "ShiftLeft", 16, ""),
    ("Control", "ControlLeft", 17, ""),
    ("Alt", "AltLeft", 18, ""),
    ("Meta", "MetaLeft", 91, ""),
    ("CapsLock", "CapsLock", 20, ""),
    ("F1", "F1", 112, ""),
    ("F2", "F2", 113, ""),
    ("F3", "F3", 114, ""),
    ("F4", "F4", 115, ""),
    ("F5", "F5", 116, ""),
    ("F6", "F6", 117, ""),
    ("F7", "F7", 118, ""),
    ("F8", "F8", 119, ""),
    ("F9", "F9", 120, ""),
    ("F10", "F10", 121, ""),
    ("F11", "F11", 122, ""),
    ("F12", "F12", 123, ""),
];

/// Resolve a `keyPress` argument into the fields a real key event carries.
///
/// Returns `Err` with a caller-facing message when the key is neither a single
/// printable character nor a known name: sending `text: "Enter"` is a protocol
/// error, and sending `'E' as i32` as the virtual key code silently types `E`.
pub(crate) fn resolve_key(key: &str) -> Result<(String, String, i32, String), String> {
    if let Some((k, code, vk, text)) = NAMED_KEYS.iter().find(|n| n.0 == key) {
        return Ok((k.to_string(), code.to_string(), *vk, text.to_string()));
    }
    let mut chars = key.chars();
    let Some(ch) = chars.next() else {
        return Err("`key` must not be empty".to_string());
    };
    if chars.next().is_some() {
        return Err(format!(
            "unknown key {key:?}; pass a single character or one of: {}",
            NAMED_KEYS
                .iter()
                .map(|n| n.0)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    // A printable character: the char *is* the key, its own code point is the
    // virtual key code (US layout), and it carries `text`.
    let code = match ch {
        'a'..='z' => format!("Key{}", ch.to_ascii_uppercase()),
        '0'..='9' => format!("Digit{ch}"),
        ' ' => "Space".to_string(),
        _ => String::new(),
    };
    Ok((ch.to_string(), code, ch as i32, ch.to_string()))
}

/// Dispatch a full mouse click (mousePressed + mouseReleased) at (x,y).
pub(crate) async fn mouse_click(
    client: &CdpClient,
    session_id: &str,
    x: f64,
    y: f64,
    button: &str, // "left" | "middle" | "right"
    click_count: i32,
) -> Result<(), String> {
    dispatch_mouse(
        client,
        session_id,
        "mousePressed",
        x,
        y,
        button,
        click_count,
        None,
    )
    .await?;
    dispatch_mouse(
        client,
        session_id,
        "mouseReleased",
        x,
        y,
        button,
        click_count,
        None,
    )
    .await
}

/// Move mouse to (x,y).
pub(crate) async fn mouse_move(
    client: &CdpClient,
    session_id: &str,
    x: f64,
    y: f64,
) -> Result<(), String> {
    dispatch_mouse(client, session_id, "mouseMoved", x, y, "none", 0, None).await
}

/// Move the mouse while a drag button is held.
///
/// `buttons` is the pressed-button mask (`Input.dispatchMouseEvent.buttons`:
/// Left=1, Right=2, Middle=4). Omitting it — as a plain [`mouse_move`] does —
/// tells the renderer no button is down, which cancels the in-flight drag: the
/// page then sees no `drag*` event at all.
pub(crate) async fn mouse_move_dragging(
    client: &CdpClient,
    session_id: &str,
    x: f64,
    y: f64,
    buttons: i32,
) -> Result<(), String> {
    dispatch_mouse(
        client,
        session_id,
        "mouseMoved",
        x,
        y,
        "left",
        0,
        Some(buttons),
    )
    .await
}

/// Dispatch an `Input.dispatchDragEvent` of `event_type` at (x,y).
///
/// `type` is one of `dragEnter` / `dragOver` / `drop` / `dragCancel`; this
/// Chrome has no `dragLeave`.
pub(crate) async fn drag_event(
    client: &CdpClient,
    session_id: &str,
    event_type: &str,
    x: f64,
    y: f64,
) -> Result<(), String> {
    client
        .send(
            "Input.dispatchDragEvent",
            Some(serde_json::json!({
                "type": event_type,
                "x": x,
                "y": y,
                // No `items`: the extension moves an element, it does not carry a
                // payload. `dragOperationsMask` 1 = copy.
                "data": { "items": [], "dragOperationsMask": 1 },
            })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

/// Dispatch `mousePressed` at (x,y) without releasing ?pairs with `mouse_up`
/// to perform drag-and-drop or custom press/hold gestures.
pub(crate) async fn mouse_down(
    client: &CdpClient,
    session_id: &str,
    x: f64,
    y: f64,
    button: &str, // "left" | "middle" | "right"
    click_count: i32,
) -> Result<(), String> {
    dispatch_mouse(
        client,
        session_id,
        "mousePressed",
        x,
        y,
        button,
        click_count,
        None,
    )
    .await
}

/// Dispatch `mouseReleased` at (x,y) ?release after a `mouse_down`.
pub(crate) async fn mouse_up(
    client: &CdpClient,
    session_id: &str,
    x: f64,
    y: f64,
    button: &str, // "left" | "middle" | "right"
    click_count: i32,
) -> Result<(), String> {
    dispatch_mouse(
        client,
        session_id,
        "mouseReleased",
        x,
        y,
        button,
        click_count,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_mouse(
    client: &CdpClient,
    session_id: &str,
    event_type: &str,
    x: f64,
    y: f64,
    button: &str,
    click_count: i32,
    buttons: Option<i32>,
) -> Result<(), String> {
    let mut params = serde_json::json!({
        "type": event_type,
        "x": x,
        "y": y,
        "button": button,
        "clickCount": click_count,
    });
    if let Some(b) = buttons {
        params["buttons"] = serde_json::json!(b);
    }
    client
        .send("Input.dispatchMouseEvent", Some(params), Some(session_id))
        .await?;
    Ok(())
}

/// Press and release one key.
///
/// The shape mirrors what a real browser produces: a `keyDown` carrying `text`
/// for keys that insert characters (printable keys, `Enter`, `Tab`), a
/// `rawKeyDown` for keys that only act (`Escape`, arrows, `Backspace`, …), then
/// `keyUp`. Putting `text` on both `keyDown` and a separate `char` event
/// inserts every character twice.
pub(crate) async fn press_key(
    client: &CdpClient,
    session_id: &str,
    key: &str,
    modifiers: i32,
) -> Result<(), String> {
    let (key, code, vk, text) = resolve_key(key)?;

    let mut down = serde_json::json!({
        "type": if text.is_empty() { "rawKeyDown" } else { "keyDown" },
        "key": key,
        "code": code,
        "windowsVirtualKeyCode": vk,
        "nativeVirtualKeyCode": vk,
        "modifiers": modifiers,
    });
    if !text.is_empty() {
        down["text"] = serde_json::Value::String(text.clone());
        down["unmodifiedText"] = serde_json::Value::String(text);
    }
    client
        .send("Input.dispatchKeyEvent", Some(down), Some(session_id))
        .await?;

    client
        .send(
            "Input.dispatchKeyEvent",
            Some(serde_json::json!({
                "type": "keyUp",
                "key": key,
                "code": code,
                "windowsVirtualKeyCode": vk,
                "nativeVirtualKeyCode": vk,
                "modifiers": modifiers,
            })),
            Some(session_id),
        )
        .await?;
    Ok(())
}

/// Insert text via Input.insertText (reliable for CJK / Unicode).
pub(crate) async fn insert_text(
    client: &CdpClient,
    session_id: &str,
    text: &str,
) -> Result<(), String> {
    client
        .send(
            "Input.insertText",
            Some(serde_json::json!({ "text": text })),
            Some(session_id),
        )
        .await?;
    Ok(())
}
