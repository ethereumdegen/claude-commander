use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::{SinkExt, StreamExt};
use tokio::sync::oneshot;
use warp::Filter;

use crate::claude::ClaudeSession;
use crate::{debug_log, shared, WsClient, WsMode};

// ── JSON response helpers ──

fn ws_error(msg: &str) -> String {
    serde_json::json!({"type": "error", "message": msg}).to_string()
}

fn ws_event(session_id: usize, event: &str) -> String {
    serde_json::json!({"type": "event", "session_id": session_id, "event": event}).to_string()
}

// ── Session lookup helpers ──

/// Look up a session, lock it, run closure, return result.
/// Returns ws_error if session not found.
fn with_session<F>(session_id: usize, f: F) -> Option<String>
where
    F: FnOnce(&mut ClaudeSession) -> Option<String>,
{
    let session_arc = {
        let state = shared().lock().unwrap();
        match state.sessions.get(&session_id) {
            Some(arc) => Arc::clone(arc),
            None => return Some(ws_error("Session not found")),
        }
    };
    let mut session = session_arc.lock().unwrap();
    f(&mut session)
}

/// Look up a session and return (session_guard, session_arc) for cases that
/// need to drop the session lock mid-operation.
fn get_session_arc(session_id: usize) -> Result<Arc<Mutex<ClaudeSession>>, String> {
    let state = shared().lock().unwrap();
    state
        .sessions
        .get(&session_id)
        .map(Arc::clone)
        .ok_or_else(|| "Session not found".to_string())
}

/// Start the local WebSocket server on the given port.
/// Returns a shutdown sender that stops the server when fired.
pub fn start_local_server(port: u16, secret: String) -> oneshot::Sender<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async move {
            let secret_filter = warp::any().map(move || secret.clone());

            let ws_route = warp::path("ws")
                .and(warp::ws())
                .and(warp::query::<std::collections::HashMap<String, String>>())
                .and(secret_filter)
                .and_then(
                    |ws: warp::ws::Ws,
                     params: std::collections::HashMap<String, String>,
                     secret: String| async move {
                        let key = params.get("key").cloned().unwrap_or_default();
                        if key != secret {
                            return Err(warp::reject::reject());
                        }
                        Ok(ws.on_upgrade(handle_ws_connection))
                    },
                );

            let addr: std::net::SocketAddr = ([0, 0, 0, 0], port).into();
            let (_, server) =
                warp::serve(ws_route).bind_with_graceful_shutdown(addr, async {
                    let _ = shutdown_rx.await;
                });

            ws_log(format!("Local server listening on ws://0.0.0.0:{}", port));
            update_status("Listening");

            server.await;
            ws_log("Local server stopped".into());
        });
    });

    shutdown_tx
}

/// Start the cloud relay client mode.
/// Connects to the relay as a host and bridges messages.
pub fn start_cloud_client(
    relay_url: String,
    room_id: String,
    secret: String,
) -> oneshot::Sender<()> {
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create tokio runtime");
        rt.block_on(async move {
            let url = format!(
                "{}/host?room={}&key={}",
                relay_url, room_id, secret
            );
            ws_log(format!("Connecting to relay: {}", relay_url));
            update_status("Connecting...");

            let connect_result = tokio_tungstenite::connect_async(&url).await;
            match connect_result {
                Ok((ws_stream, _)) => {
                    ws_log("Connected to cloud relay".into());
                    update_status("Connected to relay");

                    let (mut write, mut read) = ws_stream.split();

                    let mut shutdown_rx = shutdown_rx;

                    loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(msg)) => {
                                        if let Ok(text) = msg.to_text() {
                                            let response = handle_message(text, "relay-client");
                                            if let Some(resp) = response {
                                                let _ = write.send(
                                                    tokio_tungstenite::tungstenite::Message::Text(resp.into())
                                                ).await;
                                            }
                                        }
                                    }
                                    Some(Err(e)) => {
                                        ws_log(format!("Relay error: {}", e));
                                        update_status("Relay error");
                                        break;
                                    }
                                    None => {
                                        ws_log("Relay connection closed".into());
                                        update_status("Disconnected");
                                        break;
                                    }
                                }
                            }
                            _ = &mut shutdown_rx => {
                                ws_log("Cloud client shutting down".into());
                                let _ = write.close().await;
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    ws_log(format!("Failed to connect to relay: {}", e));
                    update_status("Connection failed");
                    // Revert to Off mode on failure
                    if let Ok(mut state) = shared().try_lock() {
                        state.ws_mode = WsMode::Off;
                    }
                }
            }
        });
    });

    shutdown_tx
}

async fn handle_ws_connection(ws: warp::ws::WebSocket) {
    let (mut tx, mut rx) = ws.split();
    let addr = format!("ws-client-{}", rand::random::<u16>());

    // Register client
    {
        let mut state = shared().lock().unwrap();
        state.ws_connections.push(WsClient {
            addr: addr.clone(),
            connected_at: Instant::now(),
        });
    }
    ws_log(format!("Client connected: {}", addr));

    while let Some(result) = rx.next().await {
        match result {
            Ok(msg) => {
                if msg.is_close() {
                    break;
                }
                if let Ok(text) = msg.to_str() {
                    let response = handle_message(text, &addr);
                    if let Some(resp) = response {
                        if tx
                            .send(warp::ws::Message::text(resp))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                }
            }
            Err(e) => {
                ws_log(format!("Client {} error: {}", addr, e));
                break;
            }
        }
    }

    // Unregister client
    {
        let mut state = shared().lock().unwrap();
        state.ws_connections.retain(|c| c.addr != addr);
    }
    ws_log(format!("Client disconnected: {}", addr));
}

/// Handle an incoming JSON message and return an optional JSON response.
fn handle_message(text: &str, client_id: &str) -> Option<String> {
    let msg: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Some(ws_error(&format!("Invalid JSON: {}", e))),
    };

    let action = msg.get("action").and_then(|a| a.as_str()).unwrap_or("");
    debug_log(format!("[ws] {} -> action={}", client_id, action));

    match action {
        "list_sessions" => {
            let session_entries: Vec<_> = {
                let state = shared().lock().unwrap();
                state.sessions
                    .iter()
                    .map(|(&id, arc)| (id, Arc::clone(arc)))
                    .collect()
            };
            let sessions: Vec<serde_json::Value> = session_entries
                .iter()
                .map(|(id, arc)| {
                    let s = arc.lock().unwrap();
                    serde_json::json!({
                        "id": id,
                        "title": s.title,
                        "state": s.state.as_str(),
                        "prompt_count": s.prompt_count,
                        "total_cost": s.total_cost,
                    })
                })
                .collect();
            Some(serde_json::json!({"type": "sessions", "data": sessions}).to_string())
        }
        "create_session" => {
            let id = crate::create_session();
            Some(serde_json::json!({"type": "session_created", "session_id": id}).to_string())
        }
        "send_prompt" => {
            let session_id = msg.get("session_id").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let prompt = msg
                .get("prompt")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if prompt.is_empty() {
                return Some(ws_error("Empty prompt"));
            }
            send_prompt_to_session(session_id, &prompt)
        }
        "get_output" => {
            let session_id = msg.get("session_id").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            get_session_output(session_id)
        }
        "get_status" => {
            let session_id = msg.get("session_id").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            get_session_status(session_id)
        }
        "approve_permission" => {
            let session_id = msg.get("session_id").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            handle_permission(session_id, true)
        }
        "deny_permission" => {
            let session_id = msg.get("session_id").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            handle_permission(session_id, false)
        }
        "kill_session" => {
            let session_id = msg.get("session_id").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            kill_session(session_id)
        }
        _ => Some(ws_error(&format!("Unknown action: {}", action))),
    }
}

fn send_prompt_to_session(session_id: usize, prompt: &str) -> Option<String> {
    let session_arc = match get_session_arc(session_id) {
        Ok(arc) => arc,
        Err(e) => return Some(ws_error(&e)),
    };
    let mut session = session_arc.lock().unwrap();

    // If running, queue the prompt
    if session.is_running() {
        session.output_lines.push(format!("  [ws] queued: {}", prompt));
        session.queued_prompt = Some(prompt.to_string());
        return Some(ws_event(session_id, "queued"));
    }

    debug_log(format!(
        "[ws] Sending prompt to session {}: {}",
        session_id,
        crate::claude::truncate_chars(prompt, 80)
    ));

    if let Some(e) = crate::claude::prepare_and_send_prompt(session, &session_arc, prompt) {
        return Some(ws_error(&e));
    }
    Some(ws_event(session_id, "prompt_sent"))
}

fn get_session_output(session_id: usize) -> Option<String> {
    with_session(session_id, |session| {
        Some(
            serde_json::json!({
                "type": "output",
                "session_id": session_id,
                "lines": session.output_lines,
                "state": session.state.as_str(),
            })
            .to_string(),
        )
    })
}

fn get_session_status(session_id: usize) -> Option<String> {
    with_session(session_id, |session| {
        Some(
            serde_json::json!({
                "type": "output",
                "session_id": session_id,
                "lines": [],
                "state": session.state.as_str(),
            })
            .to_string(),
        )
    })
}

fn handle_permission(session_id: usize, allow: bool) -> Option<String> {
    let session_arc = match get_session_arc(session_id) {
        Ok(arc) => arc,
        Err(e) => return Some(ws_error(&e)),
    };
    let mut session = session_arc.lock().unwrap();

    if !session.is_awaiting_permission() {
        return Some(ws_error("Session not awaiting permission"));
    }

    let (request_id, raw_input) =
        if let crate::claude::SessionState::AwaitingPermission(req) = &session.state {
            (req.request_id.clone(), req.raw_input.clone())
        } else {
            return None;
        };

    let Some(stdin) = session.begin_permission_response() else {
        session.force_idle();
        session.output_lines.push("  [error] process not running".into());
        return Some(ws_error("Process not running"));
    };

    let action_label = if allow { "allowed" } else { "denied" };
    session.output_lines.push(format!("  [ws permission: {}]", action_label));
    drop(session);

    let (updated_input, deny_msg) = if allow {
        (raw_input, None)
    } else {
        (None, Some("User denied this action via WebSocket"))
    };

    if let Err(e) = crate::claude::send_permission_response(
        &stdin, &request_id, allow, updated_input, deny_msg,
    ) {
        debug_log(format!("[ws] Permission send error: {}", e));
        let mut s = session_arc.lock().unwrap();
        s.force_idle();
        s.output_lines.push(format!("  [error] {}", e));
        return Some(ws_error(&e));
    }

    let event = if allow { "permission_allowed" } else { "permission_denied" };
    Some(ws_event(session_id, event))
}

fn kill_session(session_id: usize) -> Option<String> {
    with_session(session_id, |session| {
        session.force_idle();
        session.output_lines.push("  [ws] session killed".into());
        Some(ws_event(session_id, "killed"))
    })
}

/// Add a message to the WebSocket log
fn ws_log(msg: String) {
    debug_log(format!("[ws] {}", msg));
    if let Ok(mut state) = shared().try_lock() {
        state.ws_log.push(msg);
        let len = state.ws_log.len();
        if len > 100 {
            state.ws_log.drain(..len - 100);
        }
    }
}

fn update_status(status: &str) {
    if let Ok(mut state) = shared().try_lock() {
        state.ws_status = status.into();
    }
}
