#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

/// Truncate a string to at most `max_chars` characters, respecting char boundaries.
pub fn truncate_chars(s: &str, max_chars: usize) -> &str {
    match s.char_indices().nth(max_chars) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

// ── Stream event types ──

/// A single question option from AskUserQuestion
#[derive(Clone)]
pub struct QuestionOption {
    pub label: String,
    pub description: String,
}

/// A question from AskUserQuestion
#[derive(Clone)]
pub struct UserQuestion {
    pub question: String,
    pub header: String,
    pub options: Vec<QuestionOption>,
    pub multi_select: bool,
}

/// A permission request from Claude CLI
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_name: String,
    pub input_preview: String,
    /// If this is an AskUserQuestion, the parsed questions
    pub questions: Vec<UserQuestion>,
    /// Raw input JSON (needed for sending back with updatedInput)
    pub raw_input: Option<serde_json::Value>,
}

/// Events produced by the streaming Claude process
pub enum StreamEvent {
    Text(String),
    SessionId(String),
    PermissionNeeded(PermissionRequest),
    Done { cost: f64 },
    Stderr(String),
    ProcessExited,
}

/// State of a Claude session
pub enum SessionState {
    Idle,
    Running,
    AwaitingPermission(PermissionRequest),
}

/// A single Claude CLI session running in a tile
pub struct ClaudeSession {
    pub id: usize,
    pub session_id: Option<String>,
    pub input_buf: String,
    pub cursor_pos: usize,
    pub output_lines: Vec<String>,
    pub scroll_offset: u16,
    pub state: SessionState,
    pub total_cost: f64,
    pub prompt_count: u32,
    pub title: String,
    pub rain_tick: AtomicU64,
    pub process_stdin: Option<Arc<Mutex<ChildStdin>>>,
    pub process_child: Option<Arc<Mutex<Child>>>,
    pub event_rx: Option<mpsc::Receiver<StreamEvent>>,
    /// Timestamp of last event received — used for stuck detection
    pub last_event_time: Option<Instant>,
    /// A prompt queued while the session is busy (sent automatically when idle)
    pub queued_prompt: Option<String>,
}

impl ClaudeSession {
    pub fn new(id: usize) -> Self {
        Self {
            id,
            session_id: None,
            input_buf: String::new(),
            cursor_pos: 0,
            output_lines: vec![
                "╔══════════════════════════════════════╗".into(),
                "║   Claude Session - ready             ║".into(),
                "║   Type a prompt and press Enter      ║".into(),
                "╚══════════════════════════════════════╝".into(),
                String::new(),
            ],
            scroll_offset: 0,
            state: SessionState::Idle,
            total_cost: 0.0,
            prompt_count: 0,
            title: format!("Session {}", id),
            rain_tick: AtomicU64::new(0),
            process_stdin: None,
            process_child: None,
            event_rx: None,
            last_event_time: None,
            queued_prompt: None,
        }
    }

    pub fn is_running(&self) -> bool {
        matches!(self.state, SessionState::Running | SessionState::AwaitingPermission(_))
    }

    pub fn is_awaiting_permission(&self) -> bool {
        matches!(self.state, SessionState::AwaitingPermission(_))
    }

    /// Whether there's a live process we can send messages to
    pub fn has_process(&self) -> bool {
        self.process_stdin.is_some() && self.event_rx.is_some()
    }

    /// Advance the rain animation by one tick
    pub fn tick_rain(&self) {
        self.rain_tick.fetch_add(1, Ordering::Relaxed);
    }

    /// Generate rain animation frame for display
    pub fn rain_frame(&self, width: usize) -> Vec<String> {
        let tick = self.rain_tick.load(Ordering::Relaxed);
        let drops = [
            '░', '▒', '▓', '│', '┃', '╎', '╏', '┆', '┇', '┊', '┋',
            '·', ':', '.', '⡀', '⠄', '⠂', '⠁', '⠈', '⠐', '⠠',
        ];
        let colors_cycle = [
            "⠁", "⠂", "⠄", "⡀", "⠈", "⠐", "⠠", "⢀",
            "⣀", "⢠", "⢐", "⢈", "⢁", "⢂", "⢄", "⣠",
        ];

        let mut lines = Vec::new();

        // 3-line rain effect
        for row in 0..3 {
            let mut line = String::new();
            for col in 0..width {
                let hash = (tick.wrapping_mul(7) + row * 13 + col as u64 * 31) % 37;
                let phase = (tick + col as u64 * 3 + row * 7) % 20;
                if phase < 3 {
                    let drop_idx = ((tick + col as u64 + row) as usize) % drops.len();
                    line.push(drops[drop_idx]);
                } else if hash < 5 {
                    let ci = ((tick + col as u64) as usize) % colors_cycle.len();
                    line.push_str(colors_cycle[ci]);
                } else {
                    line.push(' ');
                }
            }
            lines.push(line);
        }

        // Thinking message
        let dots = ".".repeat(((tick % 4) + 1) as usize);
        if let Some(ref queued) = self.queued_prompt {
            let preview: String = queued.chars().take(40).collect();
            lines.push(format!("  ⟳ thinking{:<4}  (Esc to cancel)", dots));
            lines.push(format!("  ⏳ queued: {}", preview));
        } else {
            lines.push(format!("  ⟳ thinking{:<4}  (Esc to cancel)", dots));
        }

        lines
    }

    /// Prepare to send a prompt - updates state and returns the session_id to use.
    pub fn prepare_prompt(&mut self, prompt: &str) -> String {
        self.state = SessionState::Running;
        self.prompt_count += 1;
        self.rain_tick.store(0, Ordering::Relaxed);
        self.last_event_time = Some(Instant::now());
        self.output_lines.push(format!("▸ {}", prompt));
        self.output_lines.push(String::new());

        match &self.session_id {
            Some(sid) => sid.clone(),
            None => {
                let new_id = uuid::Uuid::new_v4().to_string();
                self.session_id = Some(new_id.clone());
                new_id
            }
        }
    }

    /// Return to idle state. Keeps the process alive so the session can be reused.
    /// Returns true if there's a queued prompt ready to send.
    pub fn go_idle(&mut self) -> bool {
        self.state = SessionState::Idle;
        self.last_event_time = None;
        self.queued_prompt.is_some()
    }

    /// Hard reset: return to idle AND tear down the process.
    pub fn force_idle(&mut self) {
        self.state = SessionState::Idle;
        self.last_event_time = None;
        // Drop stdin first to signal the process
        self.process_stdin = None;
        self.event_rx = None;
        // Kill the child process if we still have a handle
        if let Some(child_arc) = self.process_child.take() {
            if let Ok(mut child) = child_arc.lock() {
                let _ = child.kill();
            }
        }
    }

    /// Cancel a running request: go idle, keep process alive for next prompt.
    /// If process is gone, do a full force_idle.
    pub fn cancel(&mut self) {
        // Kill the process so it actually stops (not just a UI state change)
        self.force_idle();
        self.output_lines.push("  [cancelled by user]".into());
    }

    /// Drain all pending stream events from the channel.
    /// Returns true if any events were received (or state changed).
    pub fn drain_events(&mut self) -> bool {
        use crate::debug_log;
        let sid = self.id;

        // Safety: if no channel exists but we think we're running, reset immediately
        if self.event_rx.is_none() {
            if !matches!(self.state, SessionState::Idle) {
                self.force_idle();
                self.output_lines.push("  [process ended unexpectedly]".into());
                debug_log(format!("[error] Session {} process ended unexpectedly", sid));
                return true;
            }
            return false;
        }

        let mut got_any = false;
        loop {
            let rx = self.event_rx.as_ref().unwrap();
            match rx.try_recv() {
                Ok(event) => {
                    got_any = true;
                    // Only reset watchdog for meaningful events (not empty stderr noise)
                    let is_meaningful = !matches!(&event, StreamEvent::Stderr(s) if s.is_empty());
                    if is_meaningful {
                        self.last_event_time = Some(Instant::now());
                    }
                    match event {
                        StreamEvent::Text(t) => {
                            // Log tool_use and tool_result events to debug panel
                            for line in t.lines() {
                                if line.starts_with("[tool_use: ") {
                                    debug_log(format!("[session {}] {}", sid, line));
                                } else if line.starts_with("[tool_result") {
                                    let preview = truncate_chars(&line, 120);
                                    debug_log(format!("[session {}] {}", sid, preview));
                                }
                            }
                            for line in t.lines() {
                                self.output_lines.push(format!("  {}", line));
                            }
                            self.scroll_offset = 0;
                        }
                        StreamEvent::SessionId(sid_str) => {
                            debug_log(format!("[session {}] Got session ID: {}", sid, sid_str));
                            self.session_id = Some(sid_str);
                        }
                        StreamEvent::PermissionNeeded(req) => {
                            let has_questions = !req.questions.is_empty();
                            debug_log(format!(
                                "[session {}] Permission requested: {} (questions={}) input={}",
                                sid,
                                req.tool_name,
                                has_questions,
                                truncate_chars(&req.input_preview, 120)
                            ));
                            self.state = SessionState::AwaitingPermission(req);
                        }
                        StreamEvent::Done { cost } => {
                            self.total_cost += cost;
                            debug_log(format!(
                                "[session {}] Done — cost: ${:.4} (total: ${:.4})",
                                sid, cost, self.total_cost
                            ));
                            self.output_lines.push(String::new());
                            self.scroll_offset = 0;
                            // Check for queued prompt before going idle
                            if let Some(queued) = self.queued_prompt.take() {
                                debug_log(format!("[session {}] Sending queued prompt: {}", sid, truncate_chars(&queued, 80)));
                                self.prepare_prompt(&queued);
                                if let Some(ref stdin) = self.process_stdin {
                                    let stdin = Arc::clone(stdin);
                                    if let Err(e) = send_prompt_to_process(&stdin, &queued, self.session_id.as_deref()) {
                                        debug_log(format!("[session {}] Queued send error: {}", sid, e));
                                        self.output_lines.push(format!("  [error] {}", e));
                                        self.go_idle();
                                    }
                                } else {
                                    self.go_idle();
                                }
                            } else {
                                self.go_idle();
                            }
                        }
                        StreamEvent::Stderr(line) => {
                            if !line.trim().is_empty() {
                                debug_log(format!("[session {}] stderr: {}", sid, truncate_chars(&line, 150)));
                                self.output_lines.push(format!("  [stderr] {}", line));
                            }
                        }
                        StreamEvent::ProcessExited => {
                            debug_log(format!("[session {}] Process exited", sid));
                            self.force_idle();
                            return true;
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    debug_log(format!("[error] Session {} channel disconnected", sid));
                    self.force_idle();
                    return true;
                }
            }
        }

        // Watchdog: if Running with no events for 2 minutes, assume stuck
        if matches!(self.state, SessionState::Running) {
            if let Some(last) = self.last_event_time {
                if last.elapsed() > std::time::Duration::from_secs(120) {
                    debug_log(format!("[error] Session {} timed out (2 min no events)", sid));
                    self.output_lines.push(
                        "  [timed out — no response for 2 min, press Esc to cancel]".into(),
                    );
                    self.force_idle();
                    return true;
                }
            }
        }

        got_any
    }
}

// ── Streaming process management ──

/// Parse a single NDJSON line from Claude's stream-json output into a StreamEvent.
/// NEVER returns None for valid lines — unknown formats become Stderr events
/// so the session doesn't silently get stuck.
fn parse_stream_line(line: &str) -> StreamEvent {
    let val: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            // Not valid JSON — emit as raw stderr so it's visible
            return StreamEvent::Stderr(format!("[raw] {}", truncate_chars(&line, 200)));
        }
    };

    let msg_type = match val.get("type").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => {
            return StreamEvent::Stderr(format!(
                "[no type field] {}",
                truncate_chars(&line, 200)
            ));
        }
    };

    match msg_type {
        "system" => {
            let subtype = val
                .get("subtype")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if subtype == "init" {
                if let Some(sid) = val.get("session_id").and_then(|v| v.as_str()) {
                    return StreamEvent::SessionId(sid.to_string());
                }
            }
            // System messages (api_retry, etc.) — don't drop, surface them
            StreamEvent::Stderr(format!("[system/{}]", subtype))
        }
        "assistant" => {
            // Extract text from message.content array
            let content = val
                .get("message")
                .and_then(|m| m.get("content"))
                .and_then(|c| c.as_array());

            let Some(content) = content else {
                // assistant message without parseable content — don't silently drop
                return StreamEvent::Stderr("[assistant message with no content]".into());
            };

            let mut text_parts = Vec::new();
            for block in content {
                match block.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                            text_parts.push(t.to_string());
                        }
                    }
                    Some("thinking") => {
                        // Extended thinking block — silently ignore, not an error
                    }
                    Some("tool_use") => {
                        let tool_name = block
                            .get("name")
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let input = block
                            .get("input")
                            .map(|i| serde_json::to_string_pretty(i).unwrap_or_default())
                            .unwrap_or_default();
                        text_parts.push(format!("[tool_use: {}]\n{}", tool_name, input));
                    }
                    Some("tool_result") => {
                        if let Some(t) = block.get("content").and_then(|c| c.as_str()) {
                            text_parts.push(format!("[tool_result] {}", t));
                        }
                    }
                    _ => {}
                }
            }

            if text_parts.is_empty() {
                // No visible content (e.g. thinking-only message) — suppress noise
                StreamEvent::Stderr(String::new())
            } else {
                StreamEvent::Text(text_parts.join("\n"))
            }
        }
        "control_request" => {
            let request_id = val
                .get("request_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();

            let tool_name = val
                .pointer("/request/tool_name")
                .or_else(|| val.pointer("/request/tool/name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let raw_input = val
                .pointer("/request/input")
                .or_else(|| val.pointer("/request/tool/input"))
                .cloned();

            let input_preview = raw_input
                .as_ref()
                .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
                .unwrap_or_default();

            let questions = if tool_name == "AskUserQuestion" {
                parse_ask_user_questions(raw_input.as_ref())
            } else {
                Vec::new()
            };

            StreamEvent::PermissionNeeded(PermissionRequest {
                request_id,
                tool_name,
                input_preview,
                questions,
                raw_input,
            })
        }
        "result" => {
            let cost = val
                .get("total_cost_usd")
                .or_else(|| val.get("cost_usd"))
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            StreamEvent::Done { cost }
        }
        "tool_result" => {
            let content = val
                .get("content")
                .and_then(|c| c.as_str())
                .or_else(|| val.get("output").and_then(|o| o.as_str()))
                .unwrap_or("");
            if content.is_empty() {
                StreamEvent::Stderr("[tool_result: empty]".into())
            } else {
                StreamEvent::Text(format!("[tool_result] {}", content))
            }
        }
        // stream_event = streaming deltas (including during extended thinking).
        // Use non-empty sentinel so the watchdog treats it as a heartbeat.
        "stream_event" => StreamEvent::Stderr(" ".into()),
        // Other benign types — truly silent (empty stderr won't reset watchdog)
        "user" | "rate_limit_event" => StreamEvent::Stderr(String::new()),
        other => {
            StreamEvent::Stderr(format!("[unhandled: {}]", other))
        }
    }
}

/// Spawn a long-lived streaming Claude process.
/// Returns (stdin handle, child handle, event receiver).
pub fn spawn_session_process(
    session_id: &str,
    is_resume: bool,
    workdir: Option<&str>,
) -> Result<(Arc<Mutex<ChildStdin>>, Arc<Mutex<Child>>, mpsc::Receiver<StreamEvent>), String> {
    let mut cmd = Command::new("claude");
    cmd.arg("-p")
        .arg("--output-format")
        .arg("stream-json")
        .arg("--input-format")
        .arg("stream-json")
        .arg("--verbose")
        .arg("--permission-prompt-tool")
        .arg("stdio");

    if is_resume {
        cmd.arg("--resume").arg(session_id);
    } else {
        cmd.arg("--session-id").arg(session_id);
    }

    if let Some(wd) = workdir {
        cmd.current_dir(wd);
    }

    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn claude: {}", e))?;

    let stdout = child.stdout.take().ok_or("No stdout")?;
    let stderr = child.stderr.take().ok_or("No stderr")?;
    let stdin = child.stdin.take().ok_or("No stdin")?;
    let stdin_arc = Arc::new(Mutex::new(stdin));
    let child_arc = Arc::new(Mutex::new(child));

    let (tx, rx) = mpsc::channel();

    // Stdout reader thread
    let tx_stdout = tx.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(stdout);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if l.trim().is_empty() {
                        continue;
                    }
                    // Log the raw message type for debugging
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) {
                        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("?");
                        let subtype = v.get("subtype").and_then(|t| t.as_str()).unwrap_or("");
                        let preview = truncate_chars(&l, 200);
                        crate::debug_log(format!("[stream] type={} sub={} | {}", msg_type, subtype, preview));
                    }
                    let event = parse_stream_line(&l);
                    if tx_stdout.send(event).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = tx_stdout.send(StreamEvent::ProcessExited);
    });

    // Stderr reader thread
    let tx_stderr = tx;
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines() {
            match line {
                Ok(l) => {
                    if !l.is_empty() {
                        if tx_stderr.send(StreamEvent::Stderr(l)).is_err() {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });

    Ok((stdin_arc, child_arc, rx))
}

/// Send a user prompt to the running Claude process via stdin.
pub fn send_prompt_to_process(
    stdin: &Arc<Mutex<ChildStdin>>,
    prompt: &str,
    session_id: Option<&str>,
) -> Result<(), String> {
    let msg = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": prompt
        },
        "session_id": session_id.unwrap_or("default"),
        "parent_tool_use_id": null
    });
    let mut line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    crate::debug_log(format!("[send_prompt] {}", truncate_chars(&line, 200)));
    line.push('\n');

    let mut guard = stdin.lock().map_err(|e| e.to_string())?;
    guard
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    guard.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Send a permission response (allow or deny) to the Claude process.
///
/// When allowing: `updated_input` MUST contain the tool's input (original or modified).
///   Claude hangs if updatedInput is missing on allow.
/// When denying: `deny_message` should explain why (Claude sees it and may adjust).
pub fn send_permission_response(
    stdin: &Arc<Mutex<ChildStdin>>,
    request_id: &str,
    allow: bool,
    updated_input: Option<serde_json::Value>,
    deny_message: Option<&str>,
) -> Result<(), String> {
    let response = if allow {
        let mut r = serde_json::json!({
            "behavior": "allow",
            "updatedPermissions": []
        });
        if let Some(input) = updated_input {
            r["updatedInput"] = input;
        } else {
            // Claude hangs if updatedInput is missing on allow — use empty object as fallback
            crate::debug_log("[ctrl_resp] WARNING: no updatedInput for allow, using empty object");
            r["updatedInput"] = serde_json::json!({});
        }
        r
    } else {
        serde_json::json!({
            "behavior": "deny",
            "message": deny_message.unwrap_or("User denied this action")
        })
    };
    let msg = serde_json::json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": response
        }
    });
    let mut line = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    crate::debug_log(format!("[ctrl_resp] {}", truncate_chars(&line, 300)));
    line.push('\n');

    let mut guard = stdin.lock().map_err(|e| e.to_string())?;
    guard
        .write_all(line.as_bytes())
        .map_err(|e| e.to_string())?;
    guard.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Parse questions from an AskUserQuestion control_request input
fn parse_ask_user_questions(input: Option<&serde_json::Value>) -> Vec<UserQuestion> {
    let Some(input) = input else { return Vec::new() };
    let Some(questions) = input.get("questions").and_then(|q| q.as_array()) else {
        return Vec::new();
    };
    questions
        .iter()
        .filter_map(|q| {
            let question = q.get("question")?.as_str()?.to_string();
            let header = q
                .get("header")
                .and_then(|h| h.as_str())
                .unwrap_or("")
                .to_string();
            let multi_select = q
                .get("multiSelect")
                .and_then(|m| m.as_bool())
                .unwrap_or(false);
            let options = q
                .get("options")
                .and_then(|o| o.as_array())
                .map(|opts| {
                    opts.iter()
                        .filter_map(|o| {
                            Some(QuestionOption {
                                label: o.get("label")?.as_str()?.to_string(),
                                description: o
                                    .get("description")
                                    .and_then(|d| d.as_str())
                                    .unwrap_or("")
                                    .to_string(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default();
            Some(UserQuestion {
                question,
                header,
                options,
                multi_select,
            })
        })
        .collect()
}
