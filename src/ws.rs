use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use tokio::sync::oneshot;
use warp::Filter;

use crate::claude::{ClaudeSession, SessionState};
use crate::{debug_log, sessions, shared, WsClient, WsMode};

// ── JSON message helpers ──

fn ws_error(msg: &str) -> String {
    serde_json::json!({"type": "error", "message": msg}).to_string()
}

fn ws_connected(session_id: usize, cwd: &str) -> String {
    serde_json::json!({"type": "connected", "session_id": session_id, "cwd": cwd}).to_string()
}

fn ws_cwd(path: &str) -> String {
    serde_json::json!({"type": "cwd", "path": path}).to_string()
}

fn ws_output(line: &str) -> String {
    serde_json::json!({"type": "output", "line": line}).to_string()
}

fn ws_state(state: &str) -> String {
    serde_json::json!({"type": "state", "state": state}).to_string()
}

fn ws_permission(tool: &str, command: &str) -> String {
    serde_json::json!({"type": "permission", "tool": tool, "command": command}).to_string()
}

// ── Server start functions ──

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

                    // Auto-create session for relay connection
                    let session_id = crate::create_session();
                    ws_log(format!("Auto-created session {} for relay client", session_id));

                    // Send connected message with cwd
                    let cwd = {
                        sessions().try_lock().ok()
                            .and_then(|sess| sess.get(&session_id)
                                .and_then(|a| a.try_lock().ok().map(|s| s.effective_cwd())))
                            .unwrap_or_default()
                    };
                    let _ = write.send(
                        tokio_tungstenite::tungstenite::Message::Text(ws_connected(session_id, &cwd).into())
                    ).await;

                    let mut shutdown_rx = shutdown_rx;

                    // Spawn output streaming task
                    let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                    spawn_output_streamer(session_id, stream_tx);

                    loop {
                        tokio::select! {
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(msg)) => {
                                        if let Ok(text) = msg.to_text() {
                                            let response = handle_message(text, session_id, "relay-client");
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
                            line = stream_rx.recv() => {
                                if let Some(line) = line {
                                    let _ = write.send(
                                        tokio_tungstenite::tungstenite::Message::Text(line.into())
                                    ).await;
                                }
                            }
                            _ = &mut shutdown_rx => {
                                ws_log("Cloud client shutting down".into());
                                let _ = write.close().await;
                                break;
                            }
                        }
                    }

                    // Kill and remove session on disconnect
                    kill_session(session_id);
                    remove_session(session_id);
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

// ── Connection handler ──

async fn handle_ws_connection(ws: warp::ws::WebSocket) {
    let (mut tx, mut rx) = ws.split();
    let addr = format!("ws-client-{}", rand::random::<u16>());

    // Register client
    if let Ok(mut state) = shared().try_lock() {
        state.ws_connections.push(WsClient {
            addr: addr.clone(),
            connected_at: Instant::now(),
        });
    }
    ws_log(format!("Client connected: {}", addr));

    // Auto-create a session for this connection
    let session_id = crate::create_session();
    ws_log(format!("Auto-created session {} for {}", session_id, addr));

    // Send connected message with cwd
    let cwd = {
        sessions().try_lock().ok()
            .and_then(|sess| sess.get(&session_id)
                .and_then(|a| a.try_lock().ok().map(|s| s.effective_cwd())))
            .unwrap_or_default()
    };
    if tx.send(warp::ws::Message::text(ws_connected(session_id, &cwd))).await.is_err() {
        cleanup_connection(&addr, session_id);
        return;
    }

    // Spawn output streaming task
    let (stream_tx, mut stream_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    spawn_output_streamer(session_id, stream_tx);

    loop {
        tokio::select! {
            result = rx.next() => {
                match result {
                    Some(Ok(msg)) => {
                        if msg.is_close() {
                            break;
                        }
                        if let Ok(text) = msg.to_str() {
                            let response = handle_message(text, session_id, &addr);
                            if let Some(resp) = response {
                                if tx.send(warp::ws::Message::text(resp)).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                    Some(Err(e)) => {
                        ws_log(format!("Client {} error: {}", addr, e));
                        break;
                    }
                    None => break,
                }
            }
            line = stream_rx.recv() => {
                if let Some(line) = line {
                    if tx.send(warp::ws::Message::text(line)).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    cleanup_connection(&addr, session_id);
}

fn cleanup_connection(addr: &str, session_id: usize) {
    // Kill and remove session to prevent unbounded HashMap growth
    kill_session(session_id);
    remove_session(session_id);

    // Unregister client
    if let Ok(mut state) = shared().try_lock() {
        state.ws_connections.retain(|c| c.addr != addr);
    }
    ws_log(format!("Client disconnected: {}", addr));
}

/// Remove a session from the global sessions map (prevents leak from WS connections)
fn remove_session(session_id: usize) {
    if let Ok(mut sess) = sessions().try_lock() {
        sess.remove(&session_id);
    }
}

// ── Message handling ──

/// Handle an incoming JSON message for the session-like protocol.
fn handle_message(text: &str, session_id: usize, client_id: &str) -> Option<String> {
    let msg: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Some(ws_error(&format!("Invalid JSON: {}", e))),
    };

    let msg_type = msg.get("type").and_then(|t| t.as_str()).unwrap_or("");
    debug_log(format!("[ws] {} -> type={}", client_id, msg_type));

    match msg_type {
        "prompt" => {
            let message = msg
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if message.is_empty() {
                return Some(ws_error("Empty prompt"));
            }
            send_prompt(session_id, &message)
        }
        "permission" => {
            let allow = msg.get("allow").and_then(|v| v.as_bool()).unwrap_or(false);
            handle_permission(session_id, allow)
        }
        "cd" => {
            let raw_path = msg
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            handle_cd(session_id, &raw_path)
        }
        "kill" => {
            handle_kill(session_id);
            None
        }
        "clear" => {
            handle_clear(session_id);
            None
        }
        "resume" => {
            let sid = msg
                .get("session_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            handle_resume(session_id, &sid)
        }
        "skip_permissions" => {
            handle_skip_permissions(session_id)
        }
        _ => Some(ws_error(&format!("Unknown message type: {}", msg_type))),
    }
}

fn send_prompt(session_id: usize, prompt: &str) -> Option<String> {
    let session_arc = match get_session_arc(session_id) {
        Ok(arc) => arc,
        Err(e) => return Some(ws_error(&e)),
    };
    let session = session_arc.lock().unwrap();

    // If running, queue the prompt
    if session.is_running() {
        drop(session);
        let mut s = session_arc.lock().unwrap();
        s.output_lines.push(format!("  [ws] queued: {}", prompt));
        s.queued_prompt = Some(prompt.to_string());
        return None; // No response needed, client will see output via stream
    }

    debug_log(format!(
        "[ws] Sending prompt to session {}: {}",
        session_id,
        crate::claude::truncate_chars(prompt, 80)
    ));

    if let Some(e) = crate::claude::prepare_and_send_prompt(session, &session_arc, prompt) {
        return Some(ws_error(&e));
    }
    None // No response needed, output streams via push
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
        if let SessionState::AwaitingPermission(req) = &session.state {
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

    None // State change will be pushed via stream
}

fn handle_cd(session_id: usize, raw_path: &str) -> Option<String> {
    let raw_path = if raw_path.is_empty() {
        dirs::home_dir()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "~".into())
    } else {
        raw_path.to_string()
    };

    let session_arc = match get_session_arc(session_id) {
        Ok(arc) => arc,
        Err(e) => return Some(ws_error(&e)),
    };

    let expanded = if raw_path.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            format!("{}{}", home.display(), &raw_path[1..])
        } else {
            raw_path.clone()
        }
    } else if std::path::Path::new(&raw_path).is_relative() {
        let base = session_arc.lock().unwrap().effective_cwd();
        format!("{}/{}", base, raw_path)
    } else {
        raw_path.clone()
    };

    let path = std::path::Path::new(&expanded);
    if !path.is_dir() {
        return Some(ws_error(&format!("Not a directory: {}", expanded)));
    }
    let canonical = path
        .canonicalize()
        .map(|p| p.display().to_string())
        .unwrap_or(expanded);

    let mut session = session_arc.lock().unwrap();
    session.workdir = Some(canonical.clone());
    session.output_lines.push(format!("  [cd] working directory -> {}", canonical));

    // Kill existing process so next prompt starts in new dir
    if session.process_child.is_some() {
        if let Some(child_arc) = session.process_child.take() {
            if let Ok(mut child) = child_arc.lock() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        session.process_stdin = None;
        session.event_rx = None;
        session.state = crate::claude::SessionState::Idle;
        session.output_lines.push("  [cd] session will resume in new directory on next prompt".into());
    }

    Some(serde_json::json!({"type": "cd", "cwd": canonical}).to_string())
}

fn handle_kill(session_id: usize) {
    let session_arc = match get_session_arc(session_id) {
        Ok(arc) => arc,
        Err(_) => return,
    };
    let mut session = session_arc.lock().unwrap();
    if let Some(child_arc) = session.process_child.take() {
        if let Ok(mut child) = child_arc.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    session.process_stdin = None;
    session.event_rx = None;
    session.state = crate::claude::SessionState::Idle;
    session.output_lines.push("  [kill] session process terminated".into());
}

fn handle_clear(session_id: usize) {
    let session_arc = match get_session_arc(session_id) {
        Ok(arc) => arc,
        Err(_) => return,
    };
    let mut session = session_arc.lock().unwrap();
    session.output_lines.clear();
}

fn handle_resume(session_id: usize, resume_id: &str) -> Option<String> {
    if resume_id.is_empty() {
        return Some(ws_error("Usage: /resume <session-id>"));
    }
    let session_arc = match get_session_arc(session_id) {
        Ok(arc) => arc,
        Err(e) => return Some(ws_error(&e)),
    };
    let mut session = session_arc.lock().unwrap();
    // Kill existing process if any
    if let Some(child_arc) = session.process_child.take() {
        if let Ok(mut child) = child_arc.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    session.process_stdin = None;
    session.event_rx = None;
    session.state = crate::claude::SessionState::Idle;
    session.session_id = Some(resume_id.to_string());
    session.prompt_count = 1; // ensures is_resume=true on next prompt
    session.output_lines.push(format!("  [resume] session set to {}", resume_id));
    session.output_lines.push("  [resume] type your next prompt to resume the conversation".into());
    None
}

fn handle_skip_permissions(session_id: usize) -> Option<String> {
    let session_arc = match get_session_arc(session_id) {
        Ok(arc) => arc,
        Err(e) => return Some(ws_error(&e)),
    };
    let mut session = session_arc.lock().unwrap();
    session.auto_accept_permissions = !session.auto_accept_permissions;
    let status = if session.auto_accept_permissions { "ON" } else { "OFF" };
    session.output_lines.push(format!("  [permissions] auto-accept is now {}", status));
    Some(serde_json::json!({"type": "skip_permissions", "enabled": session.auto_accept_permissions}).to_string())
}

fn kill_session(session_id: usize) {
    let session_arc = {
        let Ok(sess) = sessions().try_lock() else { return };
        match sess.get(&session_id) {
            Some(arc) => Arc::clone(arc),
            None => return,
        }
    };
    // shared() lock is dropped before locking session
    if let Ok(mut session) = session_arc.try_lock() {
        session.force_idle();
        session.output_lines.push("  [ws] session killed".into());
    }
}

// ── Output streaming ──

/// Spawn a background task that watches a session and pushes new output lines,
/// state changes, and permission requests to the client via the channel.
///
/// This also calls `drain_events()` on the session since WS-created sessions
/// have no TUI tile to do it — without this, the event channel is never read
/// and output_lines never gets populated.
fn spawn_output_streamer(
    session_id: usize,
    tx: tokio::sync::mpsc::UnboundedSender<String>,
) {
    tokio::spawn(async move {
        let mut last_line_count: usize = 0;
        let mut last_state = String::new();
        let mut last_cwd = String::new();

        loop {
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Check if channel is closed (client disconnected)
            if tx.is_closed() {
                break;
            }

            // Use try_lock to avoid blocking the TUI render thread.
            let session_arc = {
                let Ok(sess) = sessions().try_lock() else {
                    continue; // Skip this tick, try again next cycle
                };
                match sess.get(&session_id) {
                    Some(arc) => Arc::clone(arc),
                    None => break, // Session removed
                }
            };

            let Ok(mut session) = session_arc.try_lock() else {
                continue; // Session locked by TUI or prompt handler, skip
            };

            // Drain events from the Claude process — this is critical for WS
            // sessions that have no TUI tile calling drain_events() on tick.
            session.drain_events();

            // Send new output lines (reset tracking if lines were cleared/trimmed)
            let current_count = session.output_lines.len();
            if current_count < last_line_count {
                // Lines were cleared or trimmed — reset tracker
                last_line_count = 0;
            }
            if current_count > last_line_count {
                for i in last_line_count..current_count {
                    let _ = tx.send(ws_output(&session.output_lines[i]));
                }
                last_line_count = current_count;
            }

            // Check for state changes
            let current_state = session.state.as_str().to_string();
            if current_state != last_state {
                // Send state change
                let _ = tx.send(ws_state(&current_state));

                // If entering permission state, send the permission details
                if let SessionState::AwaitingPermission(req) = &session.state {
                    let _ = tx.send(ws_permission(&req.tool_name, &req.input_preview));
                }

                last_state = current_state;
            }

            // Check for cwd changes
            let current_cwd = session.effective_cwd();
            if current_cwd != last_cwd {
                let _ = tx.send(ws_cwd(&current_cwd));
                last_cwd = current_cwd;
            }
        }
    });
}

// ── Helpers ──

fn get_session_arc(session_id: usize) -> Result<Arc<Mutex<ClaudeSession>>, String> {
    let sess = sessions().try_lock().map_err(|_| "Sessions lock busy".to_string())?;
    sess
        .get(&session_id)
        .map(Arc::clone)
        .ok_or_else(|| "Session not found".to_string())
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
