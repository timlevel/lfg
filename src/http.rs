use axum::{
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Json},
    routing::{get, post},
    Router,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::info;

use crate::event;
use crate::sprites::THEMES;
use crate::state::SharedState;

const MAX_SESSION_ID_LEN: usize = 256;
const MAX_TOOL_NAME_LEN: usize = 128;
const MAX_EVENT_NAME_LEN: usize = 64;
const MAX_CLIENT_KEY_LEN: usize = 128;
const MAX_PIXELS_B64_LEN: usize = 32_768; // ~24KB base64 → ~18KB raw
const MAX_PIXELS_RAW_LEN: usize = 16_384; // 16KB raw

async fn auth_middleware(
    State(token): State<String>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let provided = request
        .headers()
        .get("Authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "));
    if provided != Some(token.as_str()) {
        return (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }
    next.run(request).await
}

fn resolve_param<'a>(
    query_val: Option<&'a str>,
    headers: &'a HeaderMap,
    header_name: &str,
) -> Option<&'a str> {
    query_val
        .filter(|s| !s.is_empty())
        .or_else(|| {
            headers
                .get(header_name)
                .and_then(|v| v.to_str().ok())
                .filter(|s| !s.is_empty())
        })
}

pub fn router(state: SharedState, token: Option<String>) -> Router {
    let router = Router::new()
        .route("/webhook", post(webhook))
        .route("/status", get(status))
        .route("/reset", post(reset))
        .route("/hosts", get(hosts))
        .route("/theme", get(list_themes))
        .route("/theme/{n}", post(set_theme))
        .route("/debug/gif", post(debug_gif))
        .route("/sprites/export", post(sprites_export))
        .route("/sprites/reload", post(sprites_reload))
        .route("/debug/ble-disconnect-simulate", post(debug_ble_disconnect_simulate))
        .with_state(state);

    if let Some(t) = token {
        router.route_layer(axum::middleware::from_fn_with_state(
            t,
            auth_middleware,
        ))
    } else {
        router
    }
}

#[derive(serde::Deserialize)]
struct WebhookBody {
    text: Option<String>,
}

#[derive(serde::Deserialize)]
struct WebhookQuery {
    host: Option<String>,
    profile: Option<String>,
    pixels: Option<String>,
}

async fn webhook(
    State(state): State<SharedState>,
    Query(query): Query<WebhookQuery>,
    headers: HeaderMap,
    Json(body): Json<WebhookBody>,
) -> Json<Value> {
    let raw = body.text.unwrap_or_default();
    let parts: Vec<&str> = raw.split('|').collect();
    let event_name = parts.first().map(|s| s.trim()).unwrap_or("");
    let session_id = parts.get(1).map(|s| s.trim()).unwrap_or("");
    let tool_name = parts.get(2).map(|s| s.trim()).unwrap_or("");

    let client_key = resolve_param(query.host.as_deref(), &headers, "X-Agent-Host")
        .unwrap_or("unknown");
    let profile = resolve_param(query.profile.as_deref(), &headers, "X-Agent-Profile");
    let pixels_b64 = resolve_param(query.pixels.as_deref(), &headers, "X-Agent-Pixels");

    if event_name.len() > MAX_EVENT_NAME_LEN
        || session_id.len() > MAX_SESSION_ID_LEN
        || tool_name.len() > MAX_TOOL_NAME_LEN
        || client_key.len() > MAX_CLIENT_KEY_LEN
    {
        return Json(json!({"ok": false, "error": "input too long"}));
    }

    let pixels = pixels_b64.and_then(|b64| {
        if b64.len() > MAX_PIXELS_B64_LEN {
            return None;
        }
        use base64::Engine;
        let raw = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        if raw.len() > MAX_PIXELS_RAW_LEN {
            return None;
        }
        if raw.starts_with(&[0x89, b'P', b'N', b'G']) {
            png_to_pixels(&raw)
        } else if raw.len() == 192 {
            Some(raw)
        } else {
            None
        }
    });

    if !event_name.is_empty() {
        let mut s = state.write().await;
        event::handle_event(&mut s, event_name, session_id, tool_name, client_key, profile, pixels);

        // Schedule SessionEnd cleanup
        if event_name == "SessionEnd" {
            let session_id = session_id.to_string();
            let state_clone = state.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let mut s = state_clone.write().await;
                event::clear_agent_slot(&mut s, &session_id);
            });
        }
    }

    Json(json!({"ok": true}))
}

fn png_to_pixels(data: &[u8]) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;
    let img = img.resize_exact(8, 8, image::imageops::FilterType::Nearest);
    let rgb = img.to_rgb8();
    Some(rgb.into_raw())
}

async fn status(State(state): State<SharedState>) -> Json<Value> {
    let s = state.read().await;
    let themes = &*THEMES;

    let mut hosts_json = serde_json::Map::new();
    for (hid, host) in &s.hosts {
        hosts_json.insert(
            hid.clone(),
            json!({
                "theme": themes[host.theme_index].name,
                "columns": host.columns,
                "agent_count": host.agent_count,
            }),
        );
    }

    let columns_json: Vec<Value> = (0..crate::state::MAX_COLUMNS)
        .map(|i| match &s.columns[i] {
            None => Value::Null,
            Some(col) => json!({
                "host_id": col.host_id,
                "slots": col.slots.iter().map(|slot| {
                    match slot {
                        None => Value::Null,
                        Some(a) => json!({
                            "agent_id": a.agent_id,
                            "state": a.state.as_str(),
                            "tool_name": a.tool_name,
                            "last_event": a.last_event,
                        }),
                    }
                }).collect::<Vec<_>>(),
            }),
        })
        .collect();

    let mut agents_json = serde_json::Map::new();
    for (sid, info) in &s.session_map {
        agents_json.insert(
            info.agent_id.clone(),
            json!({
                "session_id": &sid[..sid.len().min(8)],
                "host_id": info.host_id,
                "col": info.col,
                "row": info.row,
            }),
        );
    }

    Json(json!({
        "hosts": hosts_json,
        "columns": columns_json,
        "agents": agents_json,
        "stats": {
            "tool_calls": s.stats.tool_calls,
            "unique_agents": s.stats.total_unique_agents(),
            "agent_minutes": (s.stats.agent_minutes * 10.0).round() / 10.0,
        },
    }))
}

async fn hosts(State(state): State<SharedState>) -> Json<Value> {
    let s = state.read().await;
    let themes = &*THEMES;

    let mut result = serde_json::Map::new();
    for (hid, host) in &s.hosts {
        let mut agents = Vec::new();
        for &col_idx in host.columns.iter() {
            if let Some(col) = &s.columns[col_idx] {
                for row in 0..2 {
                    if let Some(agent) = &col.slots[row] {
                        agents.push(json!({
                            "agent_id": agent.agent_id,
                            "state": agent.state.as_str(),
                            "tool_name": agent.tool_name,
                            "col": col_idx,
                            "row": row,
                        }));
                    }
                }
            }
        }
        result.insert(
            hid.clone(),
            json!({
                "theme": themes[host.theme_index].name,
                "columns": host.columns,
                "agents": agents,
            }),
        );
    }

    Json(Value::Object(result))
}

async fn reset(State(state): State<SharedState>) -> Json<Value> {
    let mut s = state.write().await;
    event::reset_all(&mut s);
    info!("All hosts/agents reset");
    Json(json!({"ok": true}))
}

async fn list_themes(State(state): State<SharedState>) -> Json<Value> {
    let s = state.read().await;
    let themes = &*THEMES;

    let theme_list: HashMap<String, String> = themes
        .iter()
        .enumerate()
        .map(|(i, t)| (i.to_string(), t.name.to_string()))
        .collect();

    let host_themes: HashMap<String, String> = s
        .hosts
        .iter()
        .map(|(hid, h)| (hid.clone(), themes[h.theme_index].name.to_string()))
        .collect();

    Json(json!({
        "themes": theme_list,
        "host_themes": host_themes,
    }))
}

fn set_all_themes(s: &mut crate::state::DisplayState, n: usize) {
    // Collect column indices first to avoid double-borrow
    let col_indices: Vec<usize> = s.hosts.values().flat_map(|h| h.columns.iter().copied()).collect();
    for host in s.hosts.values_mut() {
        host.theme_index = n;
    }
    for col_idx in col_indices {
        if let Some(col) = s.columns[col_idx].as_mut() {
            for slot in col.slots.iter_mut() {
                if let Some(ag) = slot {
                    ag.theme_index = n;
                }
            }
        }
    }
}

async fn set_theme(
    State(state): State<SharedState>,
    Path(n): Path<usize>,
) -> Json<Value> {
    let themes = &*THEMES;
    if n >= themes.len() {
        return Json(json!({"error": format!("theme 0-{}", themes.len() - 1)}));
    }

    let mut s = state.write().await;
    set_all_themes(&mut s, n);

    info!("Theme -> {} ({}) for all hosts", n, themes[n].name);
    Json(json!({"ok": true, "theme": themes[n].name}))
}

async fn debug_gif(State(state): State<SharedState>) -> Json<Value> {
    let (snap, any_requesting) = {
        let s = state.read().await;
        (crate::render::snapshot_state(&s), s.any_requesting())
    };
    let gif_data = crate::render::build_animated_gif(&snap, any_requesting);
    let path = "/tmp/lfg_debug.gif";
    std::fs::write(path, &gif_data).ok();
    info!("Debug GIF saved to {} ({} bytes)", path, gif_data.len());
    Json(json!({"ok": true, "path": path, "size": gif_data.len()}))
}

async fn sprites_export() -> Json<Value> {
    Json(json!({"ok": true, "message": "Sprite export not yet implemented in Rust build"}))
}

async fn sprites_reload() -> Json<Value> {
    Json(json!({"ok": true, "message": "Sprite reload not yet implemented in Rust build"}))
}

async fn debug_ble_disconnect_simulate(State(state): State<SharedState>) -> Json<Value> {
    let mut s = state.write().await;
    s.force_ble_reconnect = true;
    info!("BLE disconnect simulated via /debug/ble-disconnect-simulate");
    Json(json!({"ok": true, "message": "BLE disconnect simulated — will auto-reconnect"}))
}
