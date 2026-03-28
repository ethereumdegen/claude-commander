mod claude;
mod theme;
mod usage;
mod ws;

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// File-based trace log for diagnosing freezes (works even when TUI is stuck).
/// Writes to /tmp/claude-commander-trace.log
fn trace_log(msg: &str) {
    use std::fs::OpenOptions;
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open("/tmp/claude-commander-trace.log") {
        let _ = writeln!(f, "[{:?}] {}", Instant::now(), msg);
    }
}

use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use unicode_width::UnicodeWidthStr;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Widget, Wrap};
use ratatui_hypertile::{EventOutcome, HypertileEvent, KeyChord, KeyCode as HtKeyCode, PaneId};
use ratatui_hypertile_extras::{
    AnimationConfig, HypertilePlugin, HypertileRuntime, InputMode, ModeIndicator, SplitBehavior,
    WorkspaceRuntime, event_from_crossterm,
};

use crate::usage::{LiveUsage, StatsCache};

/// Copy text to the system clipboard, using platform-specific persistence on Linux.
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    // Run clipboard ops in a background thread to avoid blocking the TUI.
    // On Linux, arboard's .wait() blocks indefinitely until another app
    // reads the clipboard, which freezes the entire event loop.
    let text = text.to_owned();
    std::thread::spawn(move || {
        let Ok(mut clipboard) = arboard::Clipboard::new() else { return };
        #[cfg(target_os = "linux")]
        {
            use arboard::SetExtLinux;
            let _ = clipboard.set().wait().text(text);
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = clipboard.set_text(text);
        }
    });
    Ok(())
}


/// Skip `cols` display columns from the start of `s`, returning the remaining substring.
fn skip_display_cols(s: &str, cols: usize) -> &str {
    let mut skipped = 0;
    for (i, ch) in s.char_indices() {
        if skipped >= cols {
            return &s[i..];
        }
        skipped += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
    }
    ""
}

// ── Shared state for Claude sessions across panes ──

/// Mouse text selection state
#[derive(Default, Clone)]
struct TextSelection {
    /// Whether a drag is in progress
    active: bool,
    /// Start position (col, row) in terminal coordinates
    start: (u16, u16),
    /// Current end position (col, row) in terminal coordinates
    end: (u16, u16),
    /// The selected text once finalized
    selected_text: String,
    /// The pane rect where the selection started (constrains selection to one tile)
    pane_rect: Option<Rect>,
}

impl TextSelection {
    fn is_cell_selected(&self, col: u16, row: u16) -> bool {
        if !self.active && self.selected_text.is_empty() {
            return false;
        }
        let (start, end) = self.ordered();
        if row < start.1 || row > end.1 {
            return false;
        }
        if row == start.1 && row == end.1 {
            return col >= start.0 && col <= end.0;
        }
        if row == start.1 {
            return col >= start.0;
        }
        if row == end.1 {
            return col <= end.0;
        }
        true
    }

    fn ordered(&self) -> ((u16, u16), (u16, u16)) {
        if self.start.1 < self.end.1
            || (self.start.1 == self.end.1 && self.start.0 <= self.end.0)
        {
            (self.start, self.end)
        } else {
            (self.end, self.start)
        }
    }

    fn has_selection(&self) -> bool {
        self.active || !self.selected_text.is_empty()
    }
}

/// WebSocket operating mode
#[derive(Clone, PartialEq)]
pub enum WsMode {
    Off,
    Local { port: u16 },
    Cloud { relay_url: String, room_id: String },
}

/// A connected WebSocket client
#[derive(Clone)]
pub struct WsClient {
    pub addr: String,
    pub connected_at: Instant,
}

struct SharedState {
    usage_stats: StatsCache,
    usage_live: LiveUsage,
    debug_log: Vec<(Instant, String)>,
    selection: TextSelection,
    // WebSocket fields
    ws_secret: String,
    ws_mode: WsMode,
    ws_connections: Vec<WsClient>,
    ws_status: String,
    ws_log: Vec<String>,
    ws_shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

pub fn debug_log(msg: impl Into<String>) {
    if let Ok(mut state) = shared().try_lock() {
        state.debug_log.push((Instant::now(), msg.into()));
        // Keep last 500 entries
        let len = state.debug_log.len();
        if len > 500 {
            state.debug_log.drain(..len - 500);
        }
    }
}

static SHARED: OnceLock<Mutex<SharedState>> = OnceLock::new();
static INPUT_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Atomic quit flag — set by Ctrl+C, checked at the top of the event loop
/// so the app exits even if a render frame was blocked on a mutex.
static QUIT: AtomicBool = AtomicBool::new(false);
static NEXT_SESSION_ID: AtomicUsize = AtomicUsize::new(1);
static SESSIONS: OnceLock<Mutex<HashMap<usize, Arc<Mutex<claude::ClaudeSession>>>>> = OnceLock::new();

pub fn sessions() -> &'static Mutex<HashMap<usize, Arc<Mutex<claude::ClaudeSession>>>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn generate_secret_key() -> String {
    use rand::Rng;
    let mut rng = rand::rng();
    let bytes: Vec<u8> = (0..16).map(|_| rng.random::<u8>()).collect();
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn shared() -> &'static Mutex<SharedState> {
    SHARED.get_or_init(|| {
        let stats = usage::load_stats_cache();
        let live = usage::fetch_live_usage();
        let secret = generate_secret_key();
        Mutex::new(SharedState {
            usage_stats: stats,
            usage_live: live,
            debug_log: vec![(Instant::now(), "Claude Commander started".into())],
            selection: TextSelection::default(),
            ws_secret: secret,
            ws_mode: WsMode::Off,
            ws_connections: Vec::new(),
            ws_status: "Off".into(),
            ws_log: Vec::new(),
            ws_shutdown: None,
        })
    })
}

/// Create a standard tile Block with focused/unfocused styling.
fn make_tile_block(title: impl Into<String>, title_color: Color, is_focused: bool) -> Block<'static> {
    let title = title.into();
    if is_focused {
        Block::default()
            .borders(Borders::ALL)
            .border_set(border::THICK)
            .border_style(
                Style::default()
                    .fg(theme::BORDER_FOCUSED())
                    .add_modifier(Modifier::BOLD),
            )
            .title(title)
            .title_style(
                Style::default()
                    .fg(title_color)
                    .add_modifier(Modifier::BOLD),
            )
            .style(Style::default().bg(theme::bg_primary()))
    } else {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_NORMAL()))
            .title(title)
            .title_style(Style::default().fg(theme::text_secondary()))
            .style(Style::default().bg(theme::bg_primary()))
    }
}

/// Find any session that is awaiting permission and dismiss it.
/// Returns true if a permission was dismissed (caller should skip hypertile).
fn dismiss_any_awaiting_permission() -> bool {
    // Collect arcs first, then drop the sessions lock before locking individual sessions
    let arcs: Vec<(usize, Arc<Mutex<claude::ClaudeSession>>)> = {
        let Ok(sessions) = sessions().try_lock() else { return false };
        sessions.iter().map(|(&id, arc)| (id, Arc::clone(arc))).collect()
    };
    // Now iterate WITHOUT holding sessions lock
    let awaiting = arcs.iter().find_map(|(id, arc)| {
        let Ok(s) = arc.try_lock() else { return None };
        if s.is_awaiting_permission() {
            Some((*id, Arc::clone(arc)))
        } else {
            None
        }
    });

    let Some((sid, session_arc)) = awaiting else {
        return false;
    };

    let Ok(mut session) = session_arc.try_lock() else { return false };
    let (request_id, is_question) = match &session.state {
        claude::SessionState::AwaitingPermission(req) => {
            (req.request_id.clone(), !req.questions.is_empty())
        }
        _ => return false,
    };

    let deny_message = if is_question {
        session.output_lines.push("  [question: dismissed]".into());
        "User dismissed the question"
    } else {
        session.output_lines.push("  [permission: denied]".into());
        "User denied this action"
    };

    if let Some(stdin) = session.begin_permission_response() {
        drop(session);
        if let Err(e) = claude::send_permission_response(
            &stdin, &request_id, false, None, Some(deny_message),
        ) {
            debug_log(format!("[session {}] Send error: {}", sid, e));
            if let Ok(mut s) = session_arc.try_lock() {
                s.force_idle();
                s.output_lines.push(format!("  [error] {}", e));
            }
        }
    } else {
        session.force_idle();
        session.output_lines.push("  [error] process not running".into());
    }
    true
}

fn create_session() -> usize {
    let id = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
    let session = claude::ClaudeSession::new(id);
    sessions().lock().unwrap().insert(id, Arc::new(Mutex::new(session)));
    debug_log(format!("[session] Created session {}", id));
    id
}

// ── Claude Session Plugin ──

struct ClaudePlugin {
    session_id: usize,
}

impl ClaudePlugin {
    fn new() -> Self {
        let id = create_session();
        Self { session_id: id }
    }
}

impl HypertilePlugin for ClaudePlugin {
    fn render(&self, area: Rect, buf: &mut Buffer, is_focused: bool) {
        // Extract what we need from shared state, then drop the lock BEFORE
        // locking the session — holding both simultaneously causes deadlocks
        // with WS/other threads that lock in the opposite order.
        let session_arc = {
            let Ok(sess) = sessions().try_lock() else {
                // sessions map locked — show placeholder, skip this frame
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER_NORMAL()))
                    .title(format!(" Session {} │ ⟳ ", self.session_id))
                    .style(Style::default().bg(theme::bg_primary()))
                    .render(area, buf);
                return;
            };
            let Some(arc) = sess.get(&self.session_id) else {
                Paragraph::new("Session not found").render(area, buf);
                return;
            };
            Arc::clone(arc)
        };
        let in_input_mode = is_focused && INPUT_MODE_ACTIVE.load(Ordering::Relaxed);
        // Use try_lock to avoid blocking the render thread when WS or other
        // threads hold the session lock — a blocked render freezes Ctrl+C too.
        let Ok(mut session) = session_arc.try_lock() else {
            // Lock contention — show a minimal placeholder and skip this frame.
            let block = Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_NORMAL()))
                .title(format!(" Session {} │ ⟳ ", self.session_id))
                .style(Style::default().bg(theme::bg_primary()));
            block.render(area, buf);
            return;
        };

        let is_awaiting = session.is_awaiting_permission();
        let is_running = session.is_running();

        let status_indicator = if is_awaiting {
            "⚠ PERMISSION"
        } else if is_running {
            "⟳"
        } else {
            ""
        };

        let title = format!(
            " {} │ prompts:{} │ ${:.4} {} ",
            session.title,
            session.prompt_count,
            session.total_cost,
            status_indicator,
        );

        let (border_color, tile_bg) = if is_awaiting {
            (theme::YELLOW(), theme::bg_primary())
        } else if in_input_mode {
            (theme::GREEN(), theme::bg_input_active())
        } else if is_focused {
            (theme::BORDER_FOCUSED(), theme::bg_primary())
        } else {
            (theme::BORDER_NORMAL(), theme::bg_primary())
        };

        let block = if is_focused {
            Block::default()
                .borders(Borders::ALL)
                .border_set(border::THICK)
                .border_style(
                    Style::default()
                        .fg(border_color)
                        .add_modifier(Modifier::BOLD),
                )
                .title(title)
                .title_style(Style::default().fg(if in_input_mode {
                    theme::GREEN()
                } else {
                    theme::CYAN()
                }))
                .style(Style::default().bg(tile_bg))
        } else {
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color))
                .title(title)
                .title_style(Style::default().fg(theme::text_secondary()))
                .style(Style::default().bg(tile_bg))
        };

        let inner = block.inner(area);
        block.render(area, buf);

        // Split inner into output area + input area
        let [output_area, input_area] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(3)]).areas(inner);

        // Render output lines (scrolled to bottom)
        let visible_height = output_area.height as usize;

        // If running (but not awaiting permission), reserve space for rain animation
        let show_rain = is_running && !is_awaiting;
        let rain_lines_count = if show_rain { 4 } else { 0 };
        let output_visible = visible_height.saturating_sub(rain_lines_count);
        // Store for consistent scroll clamping in the input handler
        session.last_output_visible = output_visible as u16;

        // Clamp scroll_offset to valid range
        let max_scroll = session.output_lines.len().saturating_sub(output_visible);
        let scroll = session.scroll_offset.min(max_scroll as u16) as usize;

        // Show a scroll indicator if scrolled up
        let scroll_indicator = if scroll > 0 {
            format!(" [{} more lines below] ", scroll)
        } else {
            String::new()
        };

        // Select lines from the bottom, accounting for line wrapping.
        // Each logical line may wrap to multiple visual rows, so we can't
        // just .take(output_visible) logical lines — that would overshoot
        // and the Paragraph widget would clip the bottom.
        let wrap_width = output_area.width.max(1) as usize;
        let mut visual_rows_used = 0usize;
        let mut lines_to_take = 0usize;
        for l in session.output_lines.iter().rev().skip(scroll) {
            let line_width = UnicodeWidthStr::width(l.as_str());
            let rows = if line_width == 0 { 1 } else { (line_width + wrap_width - 1) / wrap_width };
            if visual_rows_used + rows > output_visible && lines_to_take > 0 {
                break;
            }
            visual_rows_used += rows;
            lines_to_take += 1;
        }

        let mut lines: Vec<Line> = session
            .output_lines
            .iter()
            .rev()
            .skip(scroll)
            .take(lines_to_take)
            .rev()
            .map(|l| {
                let style = if l.starts_with("▸") {
                    Style::default()
                        .fg(theme::CYAN())
                        .add_modifier(Modifier::BOLD)
                } else if l.contains("[error]") || l.contains("[stderr]") {
                    Style::default().fg(theme::RED())
                } else {
                    Style::default().fg(theme::text_primary())
                };
                Line::styled(l.as_str(), style)
            })
            .collect();

        // Append rain animation if running
        if show_rain {
            let rain_width = output_area.width.saturating_sub(1) as usize;
            let rain = session.rain_frame(rain_width);
            let rain_colors = [theme::BLUE(), theme::CYAN(), theme::MAGENTA(), theme::GREEN()];
            for (i, rline) in rain.into_iter().enumerate() {
                lines.push(Line::from(Span::styled(
                    rline,
                    Style::default().fg(rain_colors[i % rain_colors.len()]),
                )));
            }
        }

        // Add scroll indicator if scrolled up
        if scroll > 0 {
            lines.push(Line::from(Span::styled(
                scroll_indicator,
                Style::default()
                    .fg(theme::YELLOW())
                    .add_modifier(Modifier::BOLD),
            )));
        }

        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .style(Style::default().bg(tile_bg))
            .render(output_area, buf);

        // Render input box with per-session cwd
        let raw_cwd = session.effective_cwd();
        let cwd = if let Some(home) = dirs::home_dir() {
            if let Some(rest) = raw_cwd.strip_prefix(&home.display().to_string()) {
                format!("~{}", rest)
            } else {
                raw_cwd
            }
        } else {
            raw_cwd
        };

        let danger = session.auto_accept_permissions;
        let input_title = if is_focused {
            if danger {
                format!(" \u{2620} {} ▸ ", cwd)
            } else {
                format!(" {} ▸ ", cwd)
            }
        } else if danger {
            format!(" \u{2620} {} ", cwd)
        } else {
            format!(" {} ", cwd)
        };

        let input_border_color = if danger {
            theme::RED()
        } else if is_focused {
            theme::GREEN()
        } else {
            theme::BORDER_NORMAL()
        };

        let input_block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(input_border_color))
            .title(input_title)
            .title_style(Style::default().fg(if danger {
                theme::RED()
            } else if is_focused {
                theme::GREEN()
            } else {
                theme::text_muted()
            }))
            .style(Style::default().bg(theme::bg_secondary()));

        let input_inner = input_block.inner(input_area);
        input_block.render(input_area, buf);

        if is_focused {
            // Render input with cursor at position, blinking
            let tick = session.rain_tick.load(std::sync::atomic::Ordering::Relaxed);
            let cursor_visible = tick % 5 < 3; // blink pattern
            let pos = session.cursor_pos;
            let before = &session.input_buf[..pos];
            let after = &session.input_buf[pos..];
            let cursor_ch = if cursor_visible { "█" } else { " " };

            // Horizontal scroll: keep cursor visible within input width
            let w = input_inner.width as usize;
            let cursor_col = UnicodeWidthStr::width(before);
            let scroll = if cursor_col >= w { cursor_col - w + 1 } else { 0 };

            // Skip `scroll` display columns from the start of `before`
            let visible_before = skip_display_cols(before, scroll);

            Paragraph::new(Line::from(vec![
                Span::styled(visible_before, Style::default().fg(theme::text_primary())),
                Span::styled(cursor_ch, Style::default().fg(theme::GREEN())),
                Span::styled(after, Style::default().fg(theme::text_primary())),
            ]))
            .style(Style::default().bg(theme::bg_secondary()))
            .render(input_inner, buf);
        } else {
            // Unfocused: show the end of the text if it overflows
            let w = input_inner.width as usize;
            let text_width = UnicodeWidthStr::width(session.input_buf.as_str());
            let scroll = if text_width > w { text_width - w } else { 0 };
            let visible = skip_display_cols(&session.input_buf, scroll);

            Paragraph::new(visible)
                .style(
                    Style::default()
                        .fg(theme::text_primary())
                        .bg(theme::bg_secondary()),
                )
                .render(input_inner, buf);
        }

        // ── Slash command popup ──
        if session.slash_popup_visible {
            render_slash_popup(buf, input_area, &session);
        }

        // ── Permission overlay ──
        if let claude::SessionState::AwaitingPermission(req) = &session.state {
            render_permission_overlay(buf, inner, req);
        }
    }

    fn on_event(&mut self, event: &HypertileEvent) -> EventOutcome {
        // Tick: drain stream events + animate
        if matches!(event, HypertileEvent::Tick) {
            let session_arc = {
                let Ok(sess) = sessions().try_lock() else {
                    return EventOutcome::Ignored;
                };
                let Some(arc) = sess.get(&self.session_id) else {
                    return EventOutcome::Ignored;
                };
                Arc::clone(arc)
            };

            // Use try_lock — if WS threads hold the lock, skip this tick
            let Ok(mut session) = session_arc.try_lock() else {
                return EventOutcome::Ignored;
            };
            session.tick_rain(); // always tick for cursor blink
            let drained = session.drain_events();
            let is_active = session.is_running();
            drop(session);

            if drained || is_active {
                return EventOutcome::Consumed;
            }
            return EventOutcome::Ignored;
        }

        let HypertileEvent::Key(key) = event else {
            return EventOutcome::Ignored;
        };

        let session_arc = {
            let Ok(sess) = sessions().try_lock() else {
                return EventOutcome::Ignored;
            };
            let Some(arc) = sess.get(&self.session_id) else {
                return EventOutcome::Ignored;
            };
            Arc::clone(arc)
        };

        let Ok(mut session) = session_arc.try_lock() else {
            return EventOutcome::Ignored;
        };

        // Permission key interception
        if session.is_awaiting_permission() {
            let is_question = matches!(
                &session.state,
                claude::SessionState::AwaitingPermission(req) if !req.questions.is_empty()
            );

            // Extract permission details, decide response, then DROP the session
            // lock BEFORE writing to stdin (prevents deadlock).
            let response_action: Option<(String, bool, Option<serde_json::Value>, Option<String>)> =
            if is_question {
                // AskUserQuestion: number keys pick an option, Esc dismisses
                match key.code {
                    HtKeyCode::Char(ch @ '1'..='9') => {
                        let idx = (ch as u8 - b'1') as usize;
                        let (request_id, raw_input, questions) = {
                            if let claude::SessionState::AwaitingPermission(req) = &session.state {
                                (req.request_id.clone(), req.raw_input.clone(), req.questions.clone())
                            } else {
                                return EventOutcome::Consumed;
                            }
                        };

                        // Build answers from selected option
                        let mut answers = serde_json::Map::new();
                        for q in &questions {
                            if let Some(opt) = q.options.get(idx) {
                                answers.insert(q.question.clone(), serde_json::Value::String(opt.label.clone()));
                                session.output_lines.push(format!("  [answered: {}]", opt.label));
                            }
                        }
                        let updated = raw_input.map(|mut input| {
                            input["answers"] = serde_json::Value::Object(answers);
                            input
                        });

                        Some((request_id, true, updated, None))
                    }
                    HtKeyCode::Char('o') => {
                        // "Other" — tell Claude the user wants something else
                        let (request_id, raw_input, questions) = {
                            if let claude::SessionState::AwaitingPermission(req) = &session.state {
                                (req.request_id.clone(), req.raw_input.clone(), req.questions.clone())
                            } else {
                                return EventOutcome::Consumed;
                            }
                        };
                        let mut answers = serde_json::Map::new();
                        for q in &questions {
                            answers.insert(q.question.clone(), serde_json::Value::String("Other".into()));
                        }
                        session.output_lines.push("  [answered: Other]".into());
                        let updated = raw_input.map(|mut input| {
                            input["answers"] = serde_json::Value::Object(answers);
                            input
                        });
                        Some((request_id, true, updated, None))
                    }
                    HtKeyCode::Escape | HtKeyCode::Char('n') => {
                        let request_id = if let claude::SessionState::AwaitingPermission(req) = &session.state {
                            req.request_id.clone()
                        } else {
                            return EventOutcome::Consumed;
                        };
                        session.output_lines.push("  [question: dismissed]".into());
                        Some((request_id, false, None, Some("User dismissed the question".to_string())))
                    }
                    _ => return EventOutcome::Consumed,
                }
            } else {
                // Normal tool permission: y/n/Enter/Esc
                match key.code {
                    HtKeyCode::Char('y') | HtKeyCode::Enter => {
                        let (request_id, raw_input) = if let claude::SessionState::AwaitingPermission(req) = &session.state {
                            (req.request_id.clone(), req.raw_input.clone())
                        } else {
                            return EventOutcome::Consumed;
                        };
                        session.output_lines.push("  [permission: allowed]".into());
                        Some((request_id, true, raw_input, None))
                    }
                    HtKeyCode::Char('n') | HtKeyCode::Escape => {
                        let request_id = if let claude::SessionState::AwaitingPermission(req) = &session.state {
                            req.request_id.clone()
                        } else {
                            return EventOutcome::Consumed;
                        };
                        session.output_lines.push("  [permission: denied]".into());
                        Some((request_id, false, None, Some("User denied this action".to_string())))
                    }
                    _ => return EventOutcome::Consumed,
                }
            };

            // Now send the response with the session lock properly released
            if let Some((request_id, allow, updated_input, deny_message)) = response_action {
                if let Some(stdin) = session.begin_permission_response() {
                    let sid = self.session_id;
                    // CRITICAL: drop the MutexGuard before writing to stdin
                    drop(session);
                    if let Err(e) = claude::send_permission_response(
                        &stdin, &request_id, allow, updated_input,
                        deny_message.as_deref(),
                    ) {
                        debug_log(format!("[session {}] Send error: {}", sid, e));
                        // Safe to re-acquire: we dropped the guard above
                        if let Ok(mut s) = session_arc.try_lock() {
                            s.force_idle();
                            s.output_lines.push(format!("  [error] {}", e));
                        }
                    }
                } else {
                    session.force_idle();
                    session.output_lines.push("  [error] process not running".into());
                }
            }
            return EventOutcome::Consumed;
        }

        // Slash popup navigation (intercept before scroll/input)
        if session.slash_popup_visible {
            let prefix = session.input_buf.trim_start().split_whitespace().next().unwrap_or("").to_string();
            let matches = filtered_slash_commands(&prefix);
            match key.code {
                HtKeyCode::Up => {
                    if session.slash_popup_selected > 0 {
                        session.slash_popup_selected -= 1;
                    } else {
                        session.slash_popup_selected = matches.len().saturating_sub(1);
                    }
                    return EventOutcome::Consumed;
                }
                HtKeyCode::Down => {
                    if session.slash_popup_selected + 1 < matches.len() {
                        session.slash_popup_selected += 1;
                    } else {
                        session.slash_popup_selected = 0;
                    }
                    return EventOutcome::Consumed;
                }
                HtKeyCode::Tab | HtKeyCode::Enter => {
                    // Autocomplete selected command
                    if let Some((name, _)) = matches.get(session.slash_popup_selected) {
                        session.input_buf = format!("{} ", name);
                        session.cursor_pos = session.input_buf.len();
                    }
                    session.slash_popup_visible = false;
                    session.slash_popup_selected = 0;
                    return EventOutcome::Consumed;
                }
                HtKeyCode::Escape => {
                    session.slash_popup_visible = false;
                    session.slash_popup_selected = 0;
                    return EventOutcome::Consumed;
                }
                _ => {} // fall through for char input etc.
            }
        }

        // Allow scrolling and cancel even while running
        match key.code {
            HtKeyCode::PageUp | HtKeyCode::Up => {
                let step = if matches!(key.code, HtKeyCode::PageUp) { 10 } else { 3 };
                let max_scroll = session.output_lines.len()
                    .saturating_sub(session.last_output_visible as usize);
                session.scroll_offset = (session.scroll_offset + step).min(max_scroll as u16);
                return EventOutcome::Consumed;
            }
            HtKeyCode::PageDown | HtKeyCode::Down => {
                let step = if matches!(key.code, HtKeyCode::PageDown) { 10 } else { 3 };
                session.scroll_offset = session.scroll_offset.saturating_sub(step);
                return EventOutcome::Consumed;
            }
            // Escape while running = cancel and return to idle
            HtKeyCode::Escape if session.is_running() => {
                debug_log(format!("[session {}] User cancelled via Escape", self.session_id));
                session.queued_prompt = None; // also clear any queued prompt
                session.cancel();
                return EventOutcome::Consumed;
            }
            _ => {}
        }

        // Input is always available — only Enter (submit) is blocked while running
        match key.code {
            HtKeyCode::Char(ch) => {
                let pos = session.cursor_pos;
                session.input_buf.insert(pos, ch);
                session.cursor_pos = pos + ch.len_utf8();
                update_slash_popup(&mut session);
                EventOutcome::Consumed
            }
            HtKeyCode::Backspace => {
                if session.cursor_pos > 0 {
                    let pos = session.cursor_pos;
                    let prev = session.input_buf[..pos]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                    session.input_buf.remove(prev);
                    session.cursor_pos = prev;
                }
                update_slash_popup(&mut session);
                EventOutcome::Consumed
            }
            HtKeyCode::Delete => {
                let pos = session.cursor_pos;
                if pos < session.input_buf.len() {
                    session.input_buf.remove(pos);
                }
                update_slash_popup(&mut session);
                EventOutcome::Consumed
            }
            HtKeyCode::Left => {
                if session.cursor_pos > 0 {
                    session.cursor_pos = session.input_buf[..session.cursor_pos]
                        .char_indices()
                        .last()
                        .map(|(i, _)| i)
                        .unwrap_or(0);
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Right => {
                if session.cursor_pos < session.input_buf.len() {
                    session.cursor_pos += session.input_buf[session.cursor_pos..]
                        .chars()
                        .next()
                        .map(|c| c.len_utf8())
                        .unwrap_or(0);
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Home => {
                session.cursor_pos = 0;
                EventOutcome::Consumed
            }
            HtKeyCode::End => {
                session.cursor_pos = session.input_buf.len();
                EventOutcome::Consumed
            }
            HtKeyCode::Enter => {
                if session.input_buf.trim().is_empty() {
                    return EventOutcome::Consumed;
                }
                // Hide slash popup on submit
                session.slash_popup_visible = false;
                session.slash_popup_selected = 0;

                let trimmed = session.input_buf.trim().to_string();

                // Handle /clear command
                if trimmed == "/clear" {
                    session.output_lines.clear();
                    session.scroll_offset = 0;
                    session.input_buf.clear();
                    session.cursor_pos = 0;
                    return EventOutcome::Consumed;
                }

                // Handle /kill command
                if trimmed == "/kill" {
                    if let Some(child_arc) = session.process_child.take() {
                        if let Ok(mut child) = child_arc.lock() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                    }
                    session.process_stdin = None;
                    session.event_rx = None;
                    session.state = claude::SessionState::Idle;
                    session.output_lines.push("  [kill] session process terminated".into());
                    session.input_buf.clear();
                    session.cursor_pos = 0;
                    return EventOutcome::Consumed;
                }

                // Handle /dangerously-skip-permissions toggle
                if trimmed == "/dangerously-skip-permissions" {
                    session.auto_accept_permissions = !session.auto_accept_permissions;
                    let status = if session.auto_accept_permissions { "ON" } else { "OFF" };
                    session.output_lines.push(format!(
                        "  [permissions] auto-accept is now {}",
                        status
                    ));
                    session.input_buf.clear();
                    session.cursor_pos = 0;
                    return EventOutcome::Consumed;
                }

                // Handle /cd command to change per-session working directory
                if trimmed == "/cd" || trimmed.starts_with("/cd ") {
                    let raw_path = if trimmed == "/cd" {
                        dirs::home_dir()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "~".into())
                    } else {
                        trimmed[4..].trim().to_string()
                    };
                    let expanded = if raw_path.starts_with('~') {
                        if let Some(home) = dirs::home_dir() {
                            format!("{}{}", home.display(), &raw_path[1..])
                        } else {
                            raw_path.clone()
                        }
                    } else if std::path::Path::new(&raw_path).is_relative() {
                        let base = session.effective_cwd();
                        format!("{}/{}", base, raw_path)
                    } else {
                        raw_path.clone()
                    };
                    let path = std::path::Path::new(&expanded);
                    if !path.is_dir() {
                        session.output_lines.push(format!("  [cd] not a directory: {}", expanded));
                        session.input_buf.clear();
                        session.cursor_pos = 0;
                        return EventOutcome::Consumed;
                    }
                    let canonical = path.canonicalize()
                        .map(|p| p.display().to_string())
                        .unwrap_or(expanded);
                    session.workdir = Some(canonical.clone());
                    let display_path = if let Some(home) = dirs::home_dir() {
                        if let Some(rest) = canonical.strip_prefix(&home.display().to_string()) {
                            format!("~{}", rest)
                        } else {
                            canonical.clone()
                        }
                    } else {
                        canonical.clone()
                    };
                    session.output_lines.push(format!("  [cd] working directory -> {}", display_path));
                    if session.process_child.is_some() {
                        if let Some(child_arc) = session.process_child.take() {
                            if let Ok(mut child) = child_arc.lock() {
                                let _ = child.kill();
                                let _ = child.wait();
                            }
                        }
                        session.process_stdin = None;
                        session.event_rx = None;
                        session.state = claude::SessionState::Idle;
                        session.output_lines.push("  [cd] session will resume in new directory on next prompt".into());
                    }
                    session.input_buf.clear();
                    session.cursor_pos = 0;
                    return EventOutcome::Consumed;
                }

                // Handle /resume command to resume an existing Claude session
                if trimmed == "/resume" || trimmed.starts_with("/resume ") {
                    let resume_id = if trimmed == "/resume" {
                        String::new()
                    } else {
                        trimmed[8..].trim().to_string()
                    };
                    if resume_id.is_empty() {
                        session.output_lines.push("  [resume] usage: /resume <session-id>".into());
                        session.input_buf.clear();
                        session.cursor_pos = 0;
                        return EventOutcome::Consumed;
                    }
                    // Kill existing process if any
                    if let Some(child_arc) = session.process_child.take() {
                        if let Ok(mut child) = child_arc.lock() {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                    }
                    session.process_stdin = None;
                    session.event_rx = None;
                    session.state = claude::SessionState::Idle;
                    // Set session_id and prompt_count so next spawn uses --resume
                    session.session_id = Some(resume_id.clone());
                    session.prompt_count = 1; // ensures is_resume=true on next prompt
                    session.output_lines.push(format!("  [resume] session set to {}", resume_id));
                    session.output_lines.push("  [resume] type your next prompt to resume the conversation".into());
                    session.input_buf.clear();
                    session.cursor_pos = 0;
                    return EventOutcome::Consumed;
                }

                // Queue prompt if session is busy
                if session.is_running() {
                    let prompt = session.input_buf.clone();
                    session.input_buf.clear();
                    session.cursor_pos = 0;
                    if session.queued_prompt.is_some() {
                        session.output_lines.push("  [replaced queued message]".into());
                    }
                    session.output_lines.push(format!("  ⏳ queued: {}", prompt));
                    session.queued_prompt = Some(prompt);
                    return EventOutcome::Consumed;
                }
                let prompt = session.input_buf.clone();
                session.input_buf.clear();
                session.cursor_pos = 0;

                let plugin_sid = self.session_id;
                debug_log(format!("[session {}] Sending prompt: {}", plugin_sid, claude::truncate_chars(&prompt, 80)));

                claude::prepare_and_send_prompt(session, &session_arc, &prompt);
                EventOutcome::Consumed
            }
            _ => EventOutcome::Ignored,
        }
    }
}

// ── Slash command definitions ──

const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/btw", "Send /btw to Claude session"),
    ("/cd", "Change working directory"),
    ("/clear", "Clear output"),
    ("/kill", "Kill session process"),
    ("/resume", "Resume a Claude session by ID"),
    ("/dangerously-skip-permissions", "Auto-skip all permissions"),
];

/// Returns the subset of SLASH_COMMANDS whose names start with `prefix`.
fn filtered_slash_commands(prefix: &str) -> Vec<(&'static str, &'static str)> {
    SLASH_COMMANDS
        .iter()
        .filter(|(name, _)| name.starts_with(prefix))
        .copied()
        .collect()
}

/// Update slash popup visibility based on current input buffer.
fn update_slash_popup(session: &mut claude::ClaudeSession) {
    let trimmed = session.input_buf.trim_start();
    if trimmed.starts_with('/') {
        let prefix = trimmed.split_whitespace().next().unwrap_or(trimmed);
        // Hide popup once the user is typing arguments after a complete command
        let is_exact_match = SLASH_COMMANDS.iter().any(|(name, _)| *name == prefix);
        if is_exact_match {
            session.slash_popup_visible = false;
            session.slash_popup_selected = 0;
            return;
        }
        let matches = filtered_slash_commands(prefix);
        if !matches.is_empty() {
            session.slash_popup_visible = true;
            if session.slash_popup_selected >= matches.len() {
                session.slash_popup_selected = 0;
            }
        } else {
            session.slash_popup_visible = false;
            session.slash_popup_selected = 0;
        }
    } else {
        session.slash_popup_visible = false;
        session.slash_popup_selected = 0;
    }
}

fn render_slash_popup(buf: &mut Buffer, input_area: Rect, session: &claude::ClaudeSession) {
    let prefix = session.input_buf.trim_start().split_whitespace().next().unwrap_or("");
    let commands = filtered_slash_commands(prefix);
    if commands.is_empty() {
        return;
    }

    let popup_width = input_area.width.min(44).max(20);
    let popup_height = (commands.len() as u16 + 2).min(input_area.y); // +2 for borders
    if popup_height < 3 {
        return;
    }

    let x = input_area.x;
    let y = input_area.y.saturating_sub(popup_height);
    let overlay = Rect::new(x, y, popup_width, popup_height);

    let bg = theme::bg_secondary();
    let border_color = theme::CYAN();

    // Clear
    for row in overlay.y..overlay.y + overlay.height {
        for col in overlay.x..overlay.x + overlay.width {
            if let Some(cell) = buf.cell_mut((col, row)) {
                cell.set_char(' ');
                cell.set_bg(bg);
                cell.set_fg(theme::text_primary());
            }
        }
    }

    let set_cell = |buf: &mut Buffer, x: u16, y: u16, ch: char, fg: ratatui::style::Color| {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_fg(fg);
            cell.set_bg(bg);
        }
    };

    let write_str = |buf: &mut Buffer, x: u16, y: u16, s: &str, fg: ratatui::style::Color, max_w: usize| {
        for (i, ch) in s.chars().take(max_w).enumerate() {
            set_cell(buf, x + i as u16, y, ch, fg);
        }
    };

    // Top border
    set_cell(buf, overlay.x, overlay.y, '┏', border_color);
    set_cell(buf, overlay.x + overlay.width - 1, overlay.y, '┓', border_color);
    for col in (overlay.x + 1)..(overlay.x + overlay.width - 1) {
        set_cell(buf, col, overlay.y, '━', border_color);
    }

    // Bottom border
    set_cell(buf, overlay.x, overlay.y + overlay.height - 1, '┗', border_color);
    set_cell(buf, overlay.x + overlay.width - 1, overlay.y + overlay.height - 1, '┛', border_color);
    for col in (overlay.x + 1)..(overlay.x + overlay.width - 1) {
        set_cell(buf, col, overlay.y + overlay.height - 1, '━', border_color);
    }

    // Side borders
    for row in (overlay.y + 1)..(overlay.y + overlay.height - 1) {
        set_cell(buf, overlay.x, row, '┃', border_color);
        set_cell(buf, overlay.x + overlay.width - 1, row, '┃', border_color);
    }

    let inner_x = overlay.x + 2;
    let inner_w = overlay.width.saturating_sub(4) as usize;

    for (i, (name, desc)) in commands.iter().enumerate() {
        let row = overlay.y + 1 + i as u16;
        if row >= overlay.y + overlay.height - 1 {
            break;
        }
        let is_selected = i == session.slash_popup_selected;
        // Highlight selected row background
        if is_selected {
            for col in (overlay.x + 1)..(overlay.x + overlay.width - 1) {
                if let Some(cell) = buf.cell_mut((col, row)) {
                    cell.set_bg(theme::bg_primary());
                }
            }
        }
        let name_fg = if is_selected { theme::GREEN() } else { theme::CYAN() };
        write_str(buf, inner_x, row, name, name_fg, inner_w);
        let desc_start = inner_x + name.len() as u16 + 1;
        let desc_w = inner_w.saturating_sub(name.len() + 1);
        write_str(buf, desc_start, row, desc, theme::text_muted(), desc_w);
    }
}

// ── Permission overlay rendering ──

fn render_permission_overlay(buf: &mut Buffer, area: Rect, req: &claude::PermissionRequest) {
    let is_question = !req.questions.is_empty();
    let overlay_width = area.width.min(60).max(30);

    // Calculate content height
    let content_lines: u16 = if is_question {
        let q = &req.questions[0]; // show first question
        2 + q.options.len() as u16 + 1 // question + blank + options + hint
    } else {
        let input_lines = req.input_preview.lines().count().min(8) as u16;
        3 + input_lines + 1 // tool + input label + lines + hint
    };
    let overlay_height = (2 + content_lines).min(area.height); // +2 for borders

    let x = area.x + area.width.saturating_sub(overlay_width) / 2;
    let y = area.y + area.height.saturating_sub(overlay_height) / 2;
    let overlay = Rect::new(x, y, overlay_width, overlay_height);

    let bg = theme::bg_secondary();
    let border_color = if is_question { theme::CYAN() } else { theme::YELLOW() };

    // Clear overlay area
    for row in overlay.y..overlay.y + overlay.height {
        for col in overlay.x..overlay.x + overlay.width {
            if let Some(cell) = buf.cell_mut((col, row)) {
                cell.set_char(' ');
                cell.set_bg(bg);
                cell.set_fg(theme::text_primary());
            }
        }
    }

    let set_cell = |buf: &mut Buffer, x: u16, y: u16, ch: char, fg: ratatui::style::Color| {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_char(ch);
            cell.set_fg(fg);
            cell.set_bg(bg);
        }
    };

    let write_str = |buf: &mut Buffer, x: u16, y: u16, s: &str, fg: ratatui::style::Color, max_w: usize| {
        for (i, ch) in s.chars().take(max_w).enumerate() {
            set_cell(buf, x + i as u16, y, ch, fg);
        }
    };

    // Top border with title
    let title = if is_question { " Claude asks " } else { " Permission Required " };
    set_cell(buf, overlay.x, overlay.y, '┏', border_color);
    set_cell(buf, overlay.x + overlay.width - 1, overlay.y, '┓', border_color);
    for col in (overlay.x + 1)..(overlay.x + overlay.width - 1) {
        set_cell(buf, col, overlay.y, '━', border_color);
    }
    let title_start = overlay.x + 1 + (overlay.width.saturating_sub(2 + title.len() as u16)) / 2;
    for (i, ch) in title.chars().enumerate() {
        let cx = title_start + i as u16;
        if cx < overlay.x + overlay.width - 1 {
            set_cell(buf, cx, overlay.y, ch, border_color);
        }
    }

    // Bottom + side borders
    set_cell(buf, overlay.x, overlay.y + overlay.height - 1, '┗', border_color);
    set_cell(buf, overlay.x + overlay.width - 1, overlay.y + overlay.height - 1, '┛', border_color);
    for col in (overlay.x + 1)..(overlay.x + overlay.width - 1) {
        set_cell(buf, col, overlay.y + overlay.height - 1, '━', border_color);
    }
    for row in (overlay.y + 1)..(overlay.y + overlay.height - 1) {
        set_cell(buf, overlay.x, row, '┃', border_color);
        set_cell(buf, overlay.x + overlay.width - 1, row, '┃', border_color);
    }

    let inner_x = overlay.x + 2;
    let inner_w = overlay.width.saturating_sub(4) as usize;
    let mut row = overlay.y + 1;

    if is_question {
        // AskUserQuestion overlay: show question + numbered options
        let q = &req.questions[0];
        write_str(buf, inner_x, row, &q.question, theme::text_primary(), inner_w);
        row += 2; // blank line after question

        for (i, opt) in q.options.iter().enumerate() {
            if row >= overlay.y + overlay.height - 2 { break; }
            let num = format!("[{}] ", i + 1);
            write_str(buf, inner_x, row, &num, theme::GREEN(), inner_w);
            write_str(buf, inner_x + num.len() as u16, row, &opt.label, theme::CYAN(), inner_w - num.len());
            row += 1;
            // Show description on next line if there's room
            if !opt.description.is_empty() && row < overlay.y + overlay.height - 2 {
                let desc = format!("    {}", opt.description);
                write_str(buf, inner_x, row, &desc, theme::text_muted(), inner_w);
                row += 1;
            }
        }

        // Hint at bottom
        let hint_row = overlay.y + overlay.height - 2;
        write_str(buf, inner_x, hint_row, "1-9:choose ", theme::text_muted(), inner_w);
        write_str(buf, inner_x + 11, hint_row, "[o]other ", theme::CYAN(), inner_w.saturating_sub(11));
        write_str(buf, inner_x + 20, hint_row, "[n/Esc]no", theme::RED(), inner_w.saturating_sub(20));
    } else {
        // Normal permission overlay: tool name + input preview + y/n
        let tool_line = format!("Tool: {}", req.tool_name);
        write_str(buf, inner_x, row, &tool_line, theme::ORANGE(), inner_w);
        row += 1;
        write_str(buf, inner_x, row, "Input:", theme::text_secondary(), inner_w);
        row += 1;

        for line in req.input_preview.lines().take(8) {
            if row >= overlay.y + overlay.height - 2 { break; }
            let display = format!("  {}", line);
            write_str(buf, inner_x, row, &display, theme::text_secondary(), inner_w);
            row += 1;
        }

        // Hint at bottom
        let hint_row = overlay.y + overlay.height - 2;
        let parts: &[(&str, ratatui::style::Color)] = &[
            ("[y/Enter]", theme::GREEN()),
            (" Allow   ", theme::text_primary()),
            ("[n/Esc]", theme::RED()),
            (" Deny", theme::text_primary()),
        ];
        let total_len: usize = parts.iter().map(|(s, _)| s.len()).sum();
        let mut cx = inner_x + (inner_w.saturating_sub(total_len)) as u16 / 2;
        for (text, color) in parts {
            for ch in text.chars() {
                if cx < overlay.x + overlay.width - 1 {
                    set_cell(buf, cx, hint_row, ch, *color);
                    cx += 1;
                }
            }
        }
    }
}

// ── Usage Dashboard Plugin ──

struct UsagePlugin;

impl HypertilePlugin for UsagePlugin {
    fn render(&self, area: Rect, buf: &mut Buffer, is_focused: bool) {
        let Ok(state) = shared().try_lock() else {
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_NORMAL()))
                .title("  Token Usage Dashboard  ")
                .style(Style::default().bg(theme::bg_primary()))
                .render(area, buf);
            return;
        };

        let block = make_tile_block("  Token Usage Dashboard  ", theme::CYAN(), is_focused);
        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),  // utilization bars
                Constraint::Length(2),  // heading
                Constraint::Min(4),    // daily bars
                Constraint::Length(6), // model breakdown
            ])
            .split(inner);

        render_utilization_buf(buf, chunks[0], &state.usage_live);

        Paragraph::new(Line::from(Span::styled(
            " Daily Activity (last 14 days)",
            Style::default()
                .fg(theme::text_primary())
                .add_modifier(Modifier::BOLD),
        )))
        .style(Style::default().bg(theme::bg_primary()))
        .render(chunks[1], buf);

        render_daily_bars_buf(buf, chunks[2], &state.usage_stats);
        render_model_buf(buf, chunks[3], &state.usage_stats);
    }

    fn on_event(&mut self, _event: &HypertileEvent) -> EventOutcome {
        EventOutcome::Ignored
    }
}

fn render_utilization_buf(buf: &mut Buffer, area: Rect, live: &LiveUsage) {
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
            Constraint::Percentage(25),
        ])
        .split(area);

    let buckets = [
        ("5h Window", &live.five_hour),
        ("7d Overall", &live.seven_day),
        ("7d Sonnet", &live.seven_day_sonnet),
        ("7d Opus", &live.seven_day_opus),
    ];

    for (i, (label, bucket)) in buckets.iter().enumerate() {
        // API returns utilization as a percentage (0–100), not a fraction
        let pct = bucket.utilization.min(100.0);
        let frac = pct / 100.0;
        let color = theme::utilization_color(frac);
        let bar_width = cols[i].width.saturating_sub(4) as usize;
        let filled = (bar_width as f64 * frac).min(bar_width as f64) as usize;

        let bar_str = "█".repeat(filled) + &"░".repeat(bar_width.saturating_sub(filled));

        let lines = vec![
            Line::from(Span::styled(
                format!(" {}", label),
                Style::default()
                    .fg(theme::text_secondary())
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!(" {}", bar_str),
                Style::default().fg(color),
            )),
            Line::from(Span::styled(
                format!(" {:>5.1}%", pct),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            )),
        ];

        Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::RIGHT)
                    .border_style(Style::default().fg(theme::BORDER_NORMAL())),
            )
            .style(Style::default().bg(theme::bg_secondary()))
            .render(cols[i], buf);
    }
}

fn render_daily_bars_buf(buf: &mut Buffer, area: Rect, stats: &StatsCache) {
    let days: Vec<&usage::DailyActivity> = stats
        .daily_activity
        .iter()
        .rev()
        .take(14)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();

    if days.is_empty() {
        Paragraph::new("  No daily activity data found")
            .style(Style::default().fg(theme::text_muted()).bg(theme::bg_primary()))
            .render(area, buf);
        return;
    }

    let max_val = days.iter().map(|d| d.message_count).max().unwrap_or(1).max(1);
    let bar_height = area.height.saturating_sub(2) as u64;
    let col_width = area.width as usize / days.len().max(1);

    for (i, day) in days.iter().enumerate() {
        let x_start = area.x + (i * col_width) as u16;
        let filled = if max_val > 0 {
            (day.message_count * bar_height / max_val) as u16
        } else {
            0
        };
        let color = theme::BAR_COLORS()[i % theme::BAR_COLORS().len()];

        // Draw bar from bottom up
        for row in 0..bar_height as u16 {
            let y = area.y + area.height.saturating_sub(2) - row;
            if y < area.y {
                break;
            }
            let ch = if row < filled { '█' } else { ' ' };
            for dx in 0..col_width.saturating_sub(1).min(3) {
                let x = x_start + dx as u16;
                if x < area.x + area.width {
                    if let Some(cell) = buf.cell_mut((x, y)) {
                        cell.set_char(ch);
                        if row < filled {
                            cell.set_fg(color);
                        }
                        cell.set_bg(theme::bg_primary());
                    }
                }
            }
        }

        // Label at bottom
        let label = if day.date.len() >= 10 {
            &day.date[5..10]
        } else {
            &day.date
        };
        let label_y = area.y + area.height.saturating_sub(1);
        for (ci, ch) in label.chars().take(col_width).enumerate() {
            let x = x_start + ci as u16;
            if x < area.x + area.width {
                if let Some(cell) = buf.cell_mut((x, label_y)) {
                    cell.set_char(ch);
                    cell.set_fg(theme::text_muted());
                    cell.set_bg(theme::bg_primary());
                }
            }
        }
    }
}

fn render_model_buf(buf: &mut Buffer, area: Rect, stats: &StatsCache) {
    let mut lines = vec![Line::from(Span::styled(
        " Model Token Breakdown",
        Style::default()
            .fg(theme::text_primary())
            .add_modifier(Modifier::BOLD),
    ))];

    if stats.model_usage.is_empty() {
        lines.push(Line::from(Span::styled(
            "   No model usage data",
            Style::default().fg(theme::text_muted()),
        )));
    } else {
        let mut models: Vec<_> = stats.model_usage.iter().collect();
        models.sort_by(|a, b| {
            (b.1.input_tokens + b.1.output_tokens).cmp(&(a.1.input_tokens + a.1.output_tokens))
        });

        for (i, (name, u)) in models.iter().take(4).enumerate() {
            let total = u.input_tokens + u.output_tokens;
            let color = theme::BAR_COLORS()[i % theme::BAR_COLORS().len()];
            let short = name.split('/').last().unwrap_or(name);
            let short: String = short.chars().take(25).collect();

            lines.push(Line::from(vec![
                Span::styled(format!("   ● {:<25}", short), Style::default().fg(color)),
                Span::styled(
                    format!(
                        " in:{:>8}  out:{:>8}  total:{:>9}",
                        fmt_tok(u.input_tokens),
                        fmt_tok(u.output_tokens),
                        fmt_tok(total),
                    ),
                    Style::default().fg(theme::text_secondary()),
                ),
            ]));
        }
    }

    Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(Style::default().bg(theme::bg_primary()))
        .render(area, buf);
}

fn fmt_tok(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ── Debug Log Plugin ──

#[derive(Clone, Copy, PartialEq)]
enum LogCategory {
    Error,
    Session,
    Usage,
    Debug,
    Other,
}

impl LogCategory {
    fn classify(msg: &str) -> Self {
        if msg.contains("[error]") || msg.contains("[stderr]") {
            LogCategory::Error
        } else if msg.contains("[session]") {
            LogCategory::Session
        } else if msg.contains("[usage]") {
            LogCategory::Usage
        } else if msg.contains("[debug]") {
            LogCategory::Debug
        } else {
            LogCategory::Other
        }
    }

    fn badge(self) -> &'static str {
        match self {
            LogCategory::Error => "ERR",
            LogCategory::Session => "SES",
            LogCategory::Usage => "USE",
            LogCategory::Debug => "DBG",
            LogCategory::Other => "---",
        }
    }

    fn color(self) -> Color {
        match self {
            LogCategory::Error => theme::RED(),
            LogCategory::Session => theme::CYAN(),
            LogCategory::Usage => theme::GREEN(),
            LogCategory::Debug => theme::ORANGE(),
            LogCategory::Other => theme::text_secondary(),
        }
    }

    fn index(self) -> usize {
        match self {
            LogCategory::Error => 0,
            LogCategory::Session => 1,
            LogCategory::Usage => 2,
            LogCategory::Debug => 3,
            LogCategory::Other => 4,
        }
    }
}

struct DebugPlugin {
    scroll_offset: usize,
    auto_scroll: bool,
    filter_categories: [bool; 5],
    search_active: bool,
    search_query: String,
    search_matches: Vec<usize>,
    search_cursor: usize,
    wrap_enabled: bool,
    pending_g: bool,
    show_help: bool,
    filter_preset: usize,
    last_visible_height: usize,
}

impl DebugPlugin {
    fn new() -> Self {
        debug_log("[debug] Debug panel opened");
        Self {
            scroll_offset: 0,
            auto_scroll: true,
            filter_categories: [true; 5],
            search_active: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_cursor: 0,
            wrap_enabled: true,
            pending_g: false,
            show_help: false,
            filter_preset: 0,
            last_visible_height: 20,
        }
    }

    fn filtered_indices(&self, debug_log: &[(Instant, String)]) -> Vec<usize> {
        let query_lower = self.search_query.to_lowercase();
        debug_log
            .iter()
            .enumerate()
            .filter(|(_, (_, msg))| {
                let cat = LogCategory::classify(msg);
                if !self.filter_categories[cat.index()] {
                    return false;
                }
                if self.search_active && !self.search_query.is_empty() {
                    return msg.to_lowercase().contains(&query_lower);
                }
                true
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn update_search_matches(&mut self, debug_log: &[(Instant, String)]) {
        if self.search_query.is_empty() {
            self.search_matches.clear();
            self.search_cursor = 0;
            return;
        }
        let query_lower = self.search_query.to_lowercase();
        self.search_matches = debug_log
            .iter()
            .enumerate()
            .filter(|(_, (_, msg))| msg.to_lowercase().contains(&query_lower))
            .map(|(i, _)| i)
            .collect();
        if self.search_cursor >= self.search_matches.len() {
            self.search_cursor = 0;
        }
    }
}

impl HypertilePlugin for DebugPlugin {
    fn render(&self, area: Rect, buf: &mut Buffer, is_focused: bool) {
        let Ok(state) = shared().try_lock() else {
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_NORMAL()))
                .title("  Debug Log  ")
                .style(Style::default().bg(theme::bg_primary()))
                .render(area, buf);
            return;
        };
        let start = Instant::now();

        let filtered = self.filtered_indices(&state.debug_log);
        let total = state.debug_log.len();
        let shown = filtered.len();

        let mut title_parts = format!("  Debug Log ({}/{})  ", shown, total);
        if self.search_active {
            title_parts.push_str(&format!("[/{}] ", self.search_query));
        }
        if self.auto_scroll {
            title_parts.push_str("[tail] ");
        }
        if !self.wrap_enabled {
            title_parts.push_str("[nowrap] ");
        }

        let block = make_tile_block(title_parts, theme::ORANGE(), is_focused);
        let inner = block.inner(area);
        block.render(area, buf);

        // Reserve 1 line for help bar when focused
        let help_height = if is_focused { 1u16 } else { 0u16 };
        let content_height = inner.height.saturating_sub(help_height) as usize;

        let app_start = state.debug_log.first().map(|(t, _)| *t).unwrap_or(start);

        let scroll = if self.auto_scroll { 0 } else { self.scroll_offset };

        let query_lower = self.search_query.to_lowercase();

        let lines: Vec<Line> = filtered
            .iter()
            .rev()
            .skip(scroll)
            .take(content_height)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|&idx| {
                let (timestamp, msg) = &state.debug_log[idx];
                let elapsed = timestamp.duration_since(app_start);
                let secs = elapsed.as_secs();
                let ms = elapsed.subsec_millis();
                let time_str = format!("{:>4}.{:03}", secs, ms);

                let cat = LogCategory::classify(msg);
                let msg_color = cat.color();

                let mut spans = vec![
                    Span::styled(
                        format!(" {} ", time_str),
                        Style::default().fg(theme::text_muted()),
                    ),
                    Span::styled(
                        format!("{} ", cat.badge()),
                        Style::default()
                            .fg(Color::Black)
                            .bg(cat.color()),
                    ),
                ];

                // Build message spans with search highlighting
                if self.search_active && !self.search_query.is_empty() {
                    let msg_lower = msg.to_lowercase();
                    let mut pos = 0;
                    while pos < msg.len() {
                        if let Some(found) = msg_lower[pos..].find(&query_lower) {
                            let abs = pos + found;
                            if abs > pos {
                                spans.push(Span::styled(
                                    &msg[pos..abs],
                                    Style::default().fg(msg_color),
                                ));
                            }
                            spans.push(Span::styled(
                                &msg[abs..abs + query_lower.len()],
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(Color::Yellow)
                                    .add_modifier(Modifier::BOLD),
                            ));
                            pos = abs + query_lower.len();
                        } else {
                            spans.push(Span::styled(
                                &msg[pos..],
                                Style::default().fg(msg_color),
                            ));
                            break;
                        }
                    }
                    if pos >= msg.len() && pos == 0 {
                        spans.push(Span::styled(msg.as_str(), Style::default().fg(msg_color)));
                    }
                } else {
                    spans.push(Span::styled(msg.as_str(), Style::default().fg(msg_color)));
                }

                Line::from(spans)
            })
            .collect();

        let content_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: content_height as u16,
        };

        let mut para = Paragraph::new(lines)
            .style(Style::default().bg(theme::bg_primary()));
        if self.wrap_enabled {
            para = para.wrap(Wrap { trim: false });
        }
        para.render(content_area, buf);

        // Help bar
        if is_focused && inner.height > 1 {
            let help_area = Rect {
                x: inner.x,
                y: inner.y + inner.height - 1,
                width: inner.width,
                height: 1,
            };
            let help_text = if self.search_active {
                "type to search | Enter/Esc:exit search | n/N:next/prev match"
            } else if self.show_help {
                "j/k:scroll G:bottom gg:top /:search 1-5:filter F:tail w:wrap y:copy s:save ?:help"
            } else {
                "j/k:scroll /:search F:tail ?:help"
            };
            Paragraph::new(Line::from(Span::styled(
                help_text,
                Style::default()
                    .fg(theme::text_muted())
                    .add_modifier(Modifier::DIM),
            )))
            .style(Style::default().bg(theme::bg_secondary()))
            .render(help_area, buf);
        }
    }

    fn on_event(&mut self, event: &HypertileEvent) -> EventOutcome {
        let HypertileEvent::Key(key) = event else {
            return EventOutcome::Ignored;
        };

        // Search input mode
        if self.search_active {
            match key.code {
                HtKeyCode::Escape | HtKeyCode::Enter => {
                    self.search_active = false;
                    if self.search_query.is_empty() {
                        self.search_matches.clear();
                    }
                    return EventOutcome::Consumed;
                }
                HtKeyCode::Backspace => {
                    self.search_query.pop();
                    let log = shared().lock().unwrap();
                    let debug_log = log.debug_log.clone();
                    drop(log);
                    self.update_search_matches(&debug_log);
                    return EventOutcome::Consumed;
                }
                HtKeyCode::Char(c) => {
                    self.search_query.push(c);
                    let log = shared().lock().unwrap();
                    let debug_log = log.debug_log.clone();
                    drop(log);
                    self.update_search_matches(&debug_log);
                    return EventOutcome::Consumed;
                }
                _ => return EventOutcome::Consumed,
            }
        }

        // Handle pending 'g' for 'gg' combo
        if self.pending_g {
            self.pending_g = false;
            if key.code == HtKeyCode::Char('g') {
                // gg -> jump to oldest
                let state = shared().lock().unwrap();
                let filtered = self.filtered_indices(&state.debug_log);
                drop(state);
                self.scroll_offset = filtered.len().saturating_sub(1);
                self.auto_scroll = false;
                return EventOutcome::Consumed;
            }
            // Not 'g' after 'g', fall through to handle as normal key
        }

        match key.code {
            HtKeyCode::Char('j') => {
                if self.scroll_offset > 0 {
                    self.scroll_offset -= 1;
                }
                self.auto_scroll = false;
                EventOutcome::Consumed
            }
            HtKeyCode::Char('k') => {
                self.scroll_offset += 1;
                self.auto_scroll = false;
                EventOutcome::Consumed
            }
            HtKeyCode::Char('G') => {
                self.scroll_offset = 0;
                self.auto_scroll = true;
                EventOutcome::Consumed
            }
            HtKeyCode::Char('g') => {
                self.pending_g = true;
                EventOutcome::Consumed
            }
            HtKeyCode::Char('d') if key.modifiers.contains(ratatui_hypertile::Modifiers::CTRL) => {
                // Ctrl+d: page down (toward newest)
                let half = self.last_visible_height / 2;
                self.scroll_offset = self.scroll_offset.saturating_sub(half);
                self.auto_scroll = false;
                EventOutcome::Consumed
            }
            HtKeyCode::Char('u') if key.modifiers.contains(ratatui_hypertile::Modifiers::CTRL) => {
                // Ctrl+u: page up (toward oldest)
                let half = self.last_visible_height / 2;
                self.scroll_offset += half;
                self.auto_scroll = false;
                EventOutcome::Consumed
            }
            HtKeyCode::Char('/') => {
                self.search_active = true;
                self.search_query.clear();
                self.search_matches.clear();
                self.search_cursor = 0;
                EventOutcome::Consumed
            }
            HtKeyCode::Char('n') => {
                // Next search match
                if !self.search_matches.is_empty() {
                    self.search_cursor = (self.search_cursor + 1) % self.search_matches.len();
                    let state = shared().lock().unwrap();
                    let filtered = self.filtered_indices(&state.debug_log);
                    drop(state);
                    let target_idx = self.search_matches[self.search_cursor];
                    // Find position in filtered list and scroll to it
                    if let Some(pos) = filtered.iter().position(|&i| i == target_idx) {
                        let from_end = filtered.len().saturating_sub(1) - pos;
                        self.scroll_offset = from_end;
                        self.auto_scroll = false;
                    }
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('N') => {
                // Prev search match
                if !self.search_matches.is_empty() {
                    if self.search_cursor == 0 {
                        self.search_cursor = self.search_matches.len() - 1;
                    } else {
                        self.search_cursor -= 1;
                    }
                    let state = shared().lock().unwrap();
                    let filtered = self.filtered_indices(&state.debug_log);
                    drop(state);
                    let target_idx = self.search_matches[self.search_cursor];
                    if let Some(pos) = filtered.iter().position(|&i| i == target_idx) {
                        let from_end = filtered.len().saturating_sub(1) - pos;
                        self.scroll_offset = from_end;
                        self.auto_scroll = false;
                    }
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('1') => { self.filter_categories[0] = !self.filter_categories[0]; EventOutcome::Consumed }
            HtKeyCode::Char('2') => { self.filter_categories[1] = !self.filter_categories[1]; EventOutcome::Consumed }
            HtKeyCode::Char('3') => { self.filter_categories[2] = !self.filter_categories[2]; EventOutcome::Consumed }
            HtKeyCode::Char('4') => { self.filter_categories[3] = !self.filter_categories[3]; EventOutcome::Consumed }
            HtKeyCode::Char('5') => { self.filter_categories[4] = !self.filter_categories[4]; EventOutcome::Consumed }
            HtKeyCode::Char('0') => {
                self.filter_categories = [true; 5];
                EventOutcome::Consumed
            }
            HtKeyCode::Char('f') => {
                // Cycle filter presets: all -> errors-only -> sessions-only -> all
                self.filter_preset = (self.filter_preset + 1) % 3;
                match self.filter_preset {
                    0 => self.filter_categories = [true; 5],
                    1 => self.filter_categories = [true, false, false, false, false],
                    2 => self.filter_categories = [false, true, false, false, false],
                    _ => {}
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('F') => {
                self.auto_scroll = !self.auto_scroll;
                if self.auto_scroll {
                    self.scroll_offset = 0;
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('w') => {
                self.wrap_enabled = !self.wrap_enabled;
                EventOutcome::Consumed
            }
            HtKeyCode::Char('y') => {
                // Copy top-visible entry to clipboard
                let state = shared().lock().unwrap();
                let filtered = self.filtered_indices(&state.debug_log);
                let scroll = if self.auto_scroll { 0 } else { self.scroll_offset };
                let top_idx = filtered.len().saturating_sub(scroll + self.last_visible_height);
                if let Some(&log_idx) = filtered.get(top_idx) {
                    let (_, msg) = &state.debug_log[log_idx];
                    let text = msg.clone();
                    drop(state);
                    match copy_to_clipboard(&text) {
                        Ok(_) => debug_log(format!("[debug] Copied log entry to clipboard: {}",
                            claude::truncate_chars(&text, 60))),
                        Err(e) => debug_log(format!("[debug] clipboard copy failed: {e}")),
                    }
                } else {
                    drop(state);
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('s') => {
                // Export filtered logs to file
                let state = shared().lock().unwrap();
                let filtered = self.filtered_indices(&state.debug_log);
                let app_start = state.debug_log.first().map(|(t, _)| *t).unwrap_or_else(Instant::now);
                let mut output = String::new();
                for &idx in &filtered {
                    let (timestamp, msg) = &state.debug_log[idx];
                    let elapsed = timestamp.duration_since(app_start);
                    let secs = elapsed.as_secs();
                    let ms = elapsed.subsec_millis();
                    let cat = LogCategory::classify(msg);
                    output.push_str(&format!("{:>4}.{:03} [{}] {}\n", secs, ms, cat.badge(), msg));
                }
                drop(state);
                let path = "/tmp/claude-commander-debug.log";
                match std::fs::write(path, &output) {
                    Ok(_) => debug_log(format!("[debug] Exported {} entries to {}", filtered.len(), path)),
                    Err(e) => debug_log(format!("[error] Failed to export logs: {}", e)),
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('?') => {
                self.show_help = !self.show_help;
                EventOutcome::Consumed
            }
            _ => EventOutcome::Ignored,
        }
    }
}

// ── Session List Plugin ──

struct SessionListPlugin {
    selected: usize,
}

impl SessionListPlugin {
    fn new() -> Self {
        debug_log("[debug] Session list panel opened");
        Self { selected: 0 }
    }
}

impl HypertilePlugin for SessionListPlugin {
    fn render(&self, area: Rect, buf: &mut Buffer, is_focused: bool) {
        // Collect session arcs and drop shared() lock before locking sessions
        // to avoid deadlock with WS/other threads.
        let (session_count, session_entries) = {
            let Ok(sess) = sessions().try_lock() else {
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER_NORMAL()))
                    .title("  Sessions  ")
                    .style(Style::default().bg(theme::bg_primary()))
                    .render(area, buf);
                return;
            };
            let mut ids: Vec<usize> = sess.keys().copied().collect();
            ids.sort();
            let entries: Vec<(usize, Arc<Mutex<claude::ClaudeSession>>)> = ids
                .iter()
                .filter_map(|&id| sess.get(&id).map(|arc| (id, Arc::clone(arc))))
                .collect();
            (sess.len(), entries)
        };

        let title = format!("  Sessions ({})  ", session_count);
        let block = make_tile_block(title, theme::MAGENTA(), is_focused);
        let inner = block.inner(area);
        block.render(area, buf);

        // Header
        let mut lines: Vec<Line> = vec![
            Line::from(vec![
                Span::styled(
                    "  ID  ",
                    Style::default()
                        .fg(theme::text_primary())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Title                    ",
                    Style::default()
                        .fg(theme::text_primary())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Status   ",
                    Style::default()
                        .fg(theme::text_primary())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Prompts  ",
                    Style::default()
                        .fg(theme::text_primary())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Cost     ",
                    Style::default()
                        .fg(theme::text_primary())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "Session ID",
                    Style::default()
                        .fg(theme::text_primary())
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                " ─".to_string() + &"─".repeat(inner.width.saturating_sub(3) as usize),
                Style::default().fg(theme::BORDER_NORMAL()),
            )),
        ];

        for (idx, (sid, session_arc)) in session_entries.iter().enumerate() {
                let Ok(session) = session_arc.try_lock() else { continue };
                let sid = *sid;
                let is_selected = is_focused && idx == self.selected;

                let (status, status_color) = if session.is_awaiting_permission() {
                    ("⚠ wait perm", theme::ORANGE())
                } else if session.is_running() {
                    ("⟳ running  ", theme::YELLOW())
                } else {
                    ("● idle     ", theme::GREEN())
                };

                let sid_display = session
                    .session_id
                    .as_deref()
                    .map(|s| claude::truncate_chars(s, 12))
                    .unwrap_or("(new)");

                let row_bg = if is_selected {
                    theme::bg_secondary()
                } else {
                    theme::bg_primary()
                };
                let indicator = if is_selected { " ▸ " } else { "   " };

                lines.push(Line::from(vec![
                    Span::styled(
                        indicator,
                        Style::default().fg(theme::CYAN()).bg(row_bg),
                    ),
                    Span::styled(
                        format!("{:<4}", sid),
                        Style::default().fg(theme::text_primary()).bg(row_bg),
                    ),
                    Span::styled(
                        format!("{:<25}", &session.title),
                        Style::default().fg(theme::CYAN()).bg(row_bg),
                    ),
                    Span::styled(
                        status.to_string(),
                        Style::default().fg(status_color).bg(row_bg),
                    ),
                    Span::styled(
                        format!("{:<9}", session.prompt_count),
                        Style::default().fg(theme::text_secondary()).bg(row_bg),
                    ),
                    Span::styled(
                        format!("${:<8.4}", session.total_cost),
                        Style::default().fg(theme::ORANGE()).bg(row_bg),
                    ),
                    Span::styled(
                        sid_display.to_string(),
                        Style::default().fg(theme::text_muted()).bg(row_bg),
                    ),
                ]));
        }

        if session_entries.is_empty() {
            lines.push(Line::from(Span::styled(
                "   No active sessions",
                Style::default().fg(theme::text_muted()),
            )));
        }

        // Footer hints
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled("  j/k", Style::default().fg(theme::GREEN()).add_modifier(Modifier::BOLD)),
            Span::styled(":navigate  ", Style::default().fg(theme::text_muted())),
        ]));

        Paragraph::new(lines)
            .style(Style::default().bg(theme::bg_primary()))
            .render(inner, buf);
    }

    fn on_event(&mut self, event: &HypertileEvent) -> EventOutcome {
        let HypertileEvent::Key(key) = event else {
            return EventOutcome::Ignored;
        };

        let session_count = sessions().try_lock().map(|s| s.len()).unwrap_or(0);

        match key.code {
            HtKeyCode::Char('j') => {
                if session_count > 0 && self.selected < session_count - 1 {
                    self.selected += 1;
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                EventOutcome::Consumed
            }
            _ => EventOutcome::Ignored,
        }
    }
}

// ── Theme Selector Plugin ──

struct ThemeMenuPlugin {
    selected: usize,
}

impl ThemeMenuPlugin {
    fn new() -> Self {
        Self {
            selected: theme::active_index(),
        }
    }
}

impl HypertilePlugin for ThemeMenuPlugin {
    fn render(&self, area: Rect, buf: &mut Buffer, is_focused: bool) {
        let t = theme::active();
        let themes = theme::all_themes();
        let current = theme::active_index();

        let title = format!("  Themes ({})  ", themes.len());
        let block = make_tile_block(title, t.magenta, is_focused);
        let inner = block.inner(area);
        block.render(area, buf);

        let visible_height = inner.height as usize;
        let mut lines: Vec<Line> = Vec::new();

        // Header
        lines.push(Line::from(vec![
            Span::styled(
                "     Theme Name              ",
                Style::default()
                    .fg(t.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "Preview",
                Style::default()
                    .fg(t.text_primary)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        lines.push(Line::from(Span::styled(
            " ─".to_string() + &"─".repeat(inner.width.saturating_sub(3) as usize),
            Style::default().fg(t.border_normal),
        )));

        // Scrolling: keep selected item visible
        let list_height = visible_height.saturating_sub(5); // header(2) + footer(3)
        let scroll = if self.selected >= list_height {
            self.selected - list_height + 1
        } else {
            0
        };

        for (idx, theme_entry) in themes.iter().enumerate().skip(scroll).take(list_height) {
            let is_selected = is_focused && idx == self.selected;
            let is_active = idx == current;

            let row_bg = if is_selected { t.bg_secondary } else { t.bg_primary };
            let indicator = if is_selected && is_active {
                " ▸●"
            } else if is_selected {
                " ▸ "
            } else if is_active {
                "  ●"
            } else {
                "   "
            };

            let indicator_color = if is_active { t.green } else { t.cyan };

            // Color preview swatches
            let preview = vec![
                Span::styled(
                    indicator,
                    Style::default().fg(indicator_color).bg(row_bg),
                ),
                Span::styled(
                    format!("{:<26}", theme_entry.name),
                    Style::default().fg(if is_active { t.green } else { t.text_primary }).bg(row_bg),
                ),
                Span::styled("██", Style::default().fg(theme_entry.blue).bg(row_bg)),
                Span::styled("██", Style::default().fg(theme_entry.green).bg(row_bg)),
                Span::styled("██", Style::default().fg(theme_entry.red).bg(row_bg)),
                Span::styled("██", Style::default().fg(theme_entry.yellow).bg(row_bg)),
                Span::styled("██", Style::default().fg(theme_entry.cyan).bg(row_bg)),
                Span::styled("██", Style::default().fg(theme_entry.magenta).bg(row_bg)),
                Span::styled("██", Style::default().fg(theme_entry.orange).bg(row_bg)),
            ];

            lines.push(Line::from(preview));
        }

        // Footer hints
        lines.push(Line::default());
        lines.push(Line::from(vec![
            Span::styled("  j/k", Style::default().fg(t.green).add_modifier(Modifier::BOLD)),
            Span::styled(":navigate  ", Style::default().fg(t.text_muted)),
            Span::styled("Enter", Style::default().fg(t.cyan).add_modifier(Modifier::BOLD)),
            Span::styled(":apply  ", Style::default().fg(t.text_muted)),
            Span::styled("●", Style::default().fg(t.green)),
            Span::styled("=active", Style::default().fg(t.text_muted)),
        ]));

        Paragraph::new(lines)
            .style(Style::default().bg(t.bg_primary))
            .render(inner, buf);
    }

    fn on_event(&mut self, event: &HypertileEvent) -> EventOutcome {
        let HypertileEvent::Key(key) = event else {
            return EventOutcome::Ignored;
        };

        let theme_count = theme::all_themes().len();

        match key.code {
            HtKeyCode::Char('j') => {
                if theme_count > 0 && self.selected < theme_count - 1 {
                    self.selected += 1;
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('k') => {
                if self.selected > 0 {
                    self.selected -= 1;
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Enter => {
                theme::set_active(self.selected);
                theme::save_current();
                debug_log(format!(
                    "[theme] Switched to: {} (saved)",
                    theme::all_themes()[self.selected].name
                ));
                EventOutcome::Consumed
            }
            _ => EventOutcome::Ignored,
        }
    }
}

// ── WebSocket Panel Plugin ──

struct WebSocketPlugin {
    log_scroll: usize,
    /// When something was copied: (timestamp, label like "Key" or "URL")
    copy_flash: Option<(Instant, String)>,
    /// Animation tick counter for the flash effect
    flash_tick: u64,
}

impl WebSocketPlugin {
    fn new() -> Self {
        debug_log("[ws] WebSocket panel opened");
        Self {
            log_scroll: 0,
            copy_flash: None,
            flash_tick: 0,
        }
    }

    /// Whether the copy flash is still active (within 2 seconds)
    fn flash_active(&self) -> bool {
        self.copy_flash
            .as_ref()
            .is_some_and(|(t, _)| t.elapsed().as_millis() < 2000)
    }

    /// Get the flash progress (0.0 = just copied, 1.0 = faded out)
    fn flash_progress(&self) -> f64 {
        self.copy_flash
            .as_ref()
            .map(|(t, _)| t.elapsed().as_millis() as f64 / 2000.0)
            .unwrap_or(1.0)
            .min(1.0)
    }
}

impl HypertilePlugin for WebSocketPlugin {
    fn render(&self, area: Rect, buf: &mut Buffer, is_focused: bool) {
        let Ok(state) = shared().try_lock() else {
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_NORMAL()))
                .title("  WebSocket  ")
                .style(Style::default().bg(theme::bg_primary()))
                .render(area, buf);
            return;
        };

        let title = format!("  WebSocket [{}]  ", state.ws_status);
        let block = make_tile_block(title, theme::CYAN(), is_focused);
        let inner = block.inner(area);
        block.render(area, buf);

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2), // mode selector
                Constraint::Length(2), // connection info
                Constraint::Length(2), // secret key
                Constraint::Length(3), // clients
                Constraint::Min(3),   // connection log
                Constraint::Length(1), // help bar
            ])
            .split(inner);

        // Mode selector
        let (local_style, cloud_style, off_style) = match &state.ws_mode {
            WsMode::Local { .. } => (
                Style::default().fg(theme::bg_primary()).bg(theme::GREEN()).add_modifier(Modifier::BOLD),
                Style::default().fg(theme::text_muted()),
                Style::default().fg(theme::text_muted()),
            ),
            WsMode::Cloud { .. } => (
                Style::default().fg(theme::text_muted()),
                Style::default().fg(theme::bg_primary()).bg(theme::CYAN()).add_modifier(Modifier::BOLD),
                Style::default().fg(theme::text_muted()),
            ),
            WsMode::Off => (
                Style::default().fg(theme::text_muted()),
                Style::default().fg(theme::text_muted()),
                Style::default().fg(theme::bg_primary()).bg(theme::RED()).add_modifier(Modifier::BOLD),
            ),
        };

        Paragraph::new(Line::from(vec![
            Span::styled("  Mode: ", Style::default().fg(theme::text_primary()).add_modifier(Modifier::BOLD)),
            Span::styled(" [1] Local ", local_style),
            Span::raw("  "),
            Span::styled(" [2] Cloud ", cloud_style),
            Span::raw("  "),
            Span::styled(" [3] Off ", off_style),
        ]))
        .style(Style::default().bg(theme::bg_primary()))
        .render(chunks[0], buf);

        // Connection info
        let info_line = match &state.ws_mode {
            WsMode::Local { port } => {
                format!("  Listening on ws://0.0.0.0:{}", port)
            }
            WsMode::Cloud { relay_url, room_id } => {
                format!("  Relay: {} | Room: {}", relay_url, room_id)
            }
            WsMode::Off => "  WebSocket server is off".into(),
        };
        let info_color = match &state.ws_mode {
            WsMode::Off => theme::text_muted(),
            _ => theme::GREEN(),
        };
        Paragraph::new(Line::from(Span::styled(info_line, Style::default().fg(info_color))))
            .style(Style::default().bg(theme::bg_primary()))
            .render(chunks[1], buf);

        // Secret key + copy flash
        let key_display = if state.ws_secret.len() > 16 {
            format!("{}...", &state.ws_secret[..16])
        } else {
            state.ws_secret.clone()
        };

        // Secret key row — with copy flash animation
        if self.flash_active() {
            let progress = self.flash_progress();
            let label = self.copy_flash.as_ref().map(|(_, l)| l.as_str()).unwrap_or("?");
            let tick = self.flash_tick as usize;

            // Braille spinner frames
            let spinner = [">>>", ">>>", ">> ", ">  ", "   ", "  <", " <<", "<<<", "<<<"];
            let frame = spinner[tick % spinner.len()];

            // Color pulses between green and cyan
            let fg = if tick % 2 == 0 { theme::GREEN() } else { theme::CYAN() };

            if progress < 0.5 {
                // Phase 1: Bright inverted banner
                let bar_len = ((1.0 - progress * 2.0) * 12.0) as usize;
                let bar: String = "=".repeat(bar_len);
                Paragraph::new(Line::from(vec![
                    Span::styled(
                        format!("  {} COPIED {} {} ", frame, label, frame),
                        Style::default().fg(theme::bg_primary()).bg(fg).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        bar,
                        Style::default().fg(fg),
                    ),
                ]))
                .style(Style::default().bg(theme::bg_primary()))
                .render(chunks[2], buf);
            } else {
                // Phase 2: Fade to key display with "copied" badge
                Paragraph::new(Line::from(vec![
                    Span::styled("  Key: ", Style::default().fg(theme::text_secondary())),
                    Span::styled(
                        &key_display,
                        Style::default().fg(fg).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  [{}] copied", label),
                        Style::default().fg(theme::GREEN()),
                    ),
                ]))
                .style(Style::default().bg(theme::bg_primary()))
                .render(chunks[2], buf);
            }
        } else {
            Paragraph::new(Line::from(vec![
                Span::styled("  Key: ", Style::default().fg(theme::text_secondary())),
                Span::styled(&key_display, Style::default().fg(theme::ORANGE()).add_modifier(Modifier::BOLD)),
                Span::styled("  (c:copy key, u:copy URL)", Style::default().fg(theme::text_muted())),
            ]))
            .style(Style::default().bg(theme::bg_primary()))
            .render(chunks[2], buf);
        }

        // Connected clients
        let client_count = state.ws_connections.len();
        let mut client_lines = vec![
            Line::from(Span::styled(
                format!("  Clients: {}", client_count),
                Style::default().fg(theme::text_primary()).add_modifier(Modifier::BOLD),
            )),
        ];
        for client in state.ws_connections.iter().take(3) {
            let elapsed = client.connected_at.elapsed().as_secs();
            client_lines.push(Line::from(Span::styled(
                format!("    {} ({}s ago)", client.addr, elapsed),
                Style::default().fg(theme::CYAN()),
            )));
        }
        Paragraph::new(client_lines)
            .style(Style::default().bg(theme::bg_primary()))
            .render(chunks[3], buf);

        // Connection log
        let log_height = chunks[4].height.saturating_sub(1) as usize;
        let log_entries: Vec<Line> = state
            .ws_log
            .iter()
            .rev()
            .skip(self.log_scroll)
            .take(log_height)
            .rev()
            .map(|entry| {
                let color = if entry.contains("error") || entry.contains("failed") {
                    theme::RED()
                } else if entry.contains("onnect") {
                    theme::GREEN()
                } else {
                    theme::text_secondary()
                };
                Line::from(Span::styled(format!("  {}", entry), Style::default().fg(color)))
            })
            .collect();
        Paragraph::new(log_entries)
            .block(
                Block::default()
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(theme::BORDER_NORMAL()))
                    .title(" Log ")
                    .title_style(Style::default().fg(theme::text_muted())),
            )
            .style(Style::default().bg(theme::bg_primary()))
            .render(chunks[4], buf);

        // Help bar
        if is_focused {
            Paragraph::new(Line::from(vec![
                Span::styled(" 1", Style::default().fg(theme::GREEN()).add_modifier(Modifier::BOLD)),
                Span::styled(":local ", Style::default().fg(theme::text_muted())),
                Span::styled("2", Style::default().fg(theme::CYAN()).add_modifier(Modifier::BOLD)),
                Span::styled(":cloud ", Style::default().fg(theme::text_muted())),
                Span::styled("3", Style::default().fg(theme::RED()).add_modifier(Modifier::BOLD)),
                Span::styled(":off ", Style::default().fg(theme::text_muted())),
                Span::styled("c", Style::default().fg(theme::ORANGE()).add_modifier(Modifier::BOLD)),
                Span::styled(":key ", Style::default().fg(theme::text_muted())),
                Span::styled("u", Style::default().fg(theme::ORANGE()).add_modifier(Modifier::BOLD)),
                Span::styled(":URL ", Style::default().fg(theme::text_muted())),
                Span::styled("j/k", Style::default().fg(theme::YELLOW()).add_modifier(Modifier::BOLD)),
                Span::styled(":scroll", Style::default().fg(theme::text_muted())),
            ]))
            .style(Style::default().bg(theme::bg_secondary()))
            .render(chunks[5], buf);
        }
    }

    fn on_event(&mut self, event: &HypertileEvent) -> EventOutcome {
        // Tick: advance flash animation
        if matches!(event, HypertileEvent::Tick) {
            if self.flash_active() {
                self.flash_tick += 1;
                return EventOutcome::Consumed;
            }
            return EventOutcome::Ignored;
        }

        let HypertileEvent::Key(key) = event else {
            return EventOutcome::Ignored;
        };

        match key.code {
            HtKeyCode::Char('1') => {
                // Switch to Local mode
                let mut state = shared().lock().unwrap();
                if matches!(state.ws_mode, WsMode::Local { .. }) {
                    return EventOutcome::Consumed;
                }
                // Shut down existing
                let _ = state.ws_shutdown.take();
                let port = 9753u16;
                let secret = state.ws_secret.clone();
                state.ws_mode = WsMode::Local { port };
                state.ws_status = "Starting...".into();
                state.ws_connections.clear();
                drop(state);

                let shutdown = ws::start_local_server(port, secret);
                let mut state = shared().lock().unwrap();
                state.ws_shutdown = Some(shutdown);
                EventOutcome::Consumed
            }
            HtKeyCode::Char('2') => {
                // Switch to Cloud mode
                let mut state = shared().lock().unwrap();
                if matches!(state.ws_mode, WsMode::Cloud { .. }) {
                    return EventOutcome::Consumed;
                }
                let _ = state.ws_shutdown.take();
                let secret = state.ws_secret.clone();
                let room_id = secret[..12].to_string();
                let relay_url = "wss://relay.example.com".to_string();
                state.ws_mode = WsMode::Cloud {
                    relay_url: relay_url.clone(),
                    room_id: room_id.clone(),
                };
                state.ws_status = "Connecting...".into();
                state.ws_connections.clear();
                drop(state);

                let shutdown = ws::start_cloud_client(relay_url, room_id, secret);
                let mut state = shared().lock().unwrap();
                state.ws_shutdown = Some(shutdown);
                EventOutcome::Consumed
            }
            HtKeyCode::Char('3') => {
                // Switch to Off
                let mut state = shared().lock().unwrap();
                let _ = state.ws_shutdown.take();
                state.ws_mode = WsMode::Off;
                state.ws_status = "Off".into();
                state.ws_connections.clear();
                EventOutcome::Consumed
            }
            HtKeyCode::Char('c') => {
                // Copy secret key
                let state = shared().lock().unwrap();
                let key = state.ws_secret.clone();
                drop(state);
                match copy_to_clipboard(&key) {
                    Ok(_) => {
                        debug_log("[ws] Secret key copied to clipboard");
                        self.copy_flash = Some((Instant::now(), "Key".into()));
                        self.flash_tick = 0;
                    }
                    Err(e) => debug_log(format!("[ws] clipboard copy failed: {}", e)),
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('u') => {
                // Copy connection URL
                let state = shared().lock().unwrap();
                let url = match &state.ws_mode {
                    WsMode::Local { port } => {
                        format!("ws://127.0.0.1:{}/ws?key={}", port, state.ws_secret)
                    }
                    WsMode::Cloud { relay_url, room_id } => {
                        format!("{}/join?room={}&key={}", relay_url, room_id, state.ws_secret)
                    }
                    WsMode::Off => "WebSocket is off".into(),
                };
                drop(state);
                match copy_to_clipboard(&url) {
                    Ok(_) => {
                        debug_log(format!("[ws] URL copied: {}", claude::truncate_chars(&url, 60)));
                        self.copy_flash = Some((Instant::now(), "URL".into()));
                        self.flash_tick = 0;
                    }
                    Err(e) => debug_log(format!("[ws] clipboard copy failed: {}", e)),
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('j') => {
                if self.log_scroll > 0 {
                    self.log_scroll -= 1;
                }
                EventOutcome::Consumed
            }
            HtKeyCode::Char('k') => {
                self.log_scroll += 1;
                EventOutcome::Consumed
            }
            _ => EventOutcome::Ignored,
        }
    }
}

// ── Main ──

fn build_runtime() -> HypertileRuntime {
    let mut rt = HypertileRuntime::builder()
        .with_split_behavior(SplitBehavior::DefaultPlugin)
        .with_default_split_plugin("claude")
        .with_animation_config(AnimationConfig {
            enabled: true,
            ..AnimationConfig::default()
        })
        .build();

    rt.register_plugin_type("claude", ClaudePlugin::new);
    rt.register_plugin_type("usage", || UsagePlugin);
    rt.register_plugin_type("debug", DebugPlugin::new);
    rt.register_plugin_type("sessions", SessionListPlugin::new);
    rt.register_plugin_type("themes", ThemeMenuPlugin::new);
    rt.register_plugin_type("websocket", WebSocketPlugin::new);

    rt
}

fn main() -> std::io::Result<()> {
    theme::load_saved();

    let mut terminal = ratatui::init();
    crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture, crossterm::event::EnableBracketedPaste)?;

    let mut workspace = WorkspaceRuntime::new(build_runtime);

    // Set up initial layout: 2 Claude sessions + Usage dashboard
    let rt = workspace.active_runtime_mut();
    let _ = rt.replace_focused_plugin("claude");
    let _ = rt.split_focused(Direction::Vertical, "usage");
    let _ = rt.focus_pane(PaneId::ROOT);
    let _ = rt.split_focused(Direction::Horizontal, "claude");

    let result = run(&mut terminal, &mut workspace);
    crossterm::execute!(std::io::stdout(), crossterm::event::DisableMouseCapture, crossterm::event::DisableBracketedPaste)?;
    ratatui::restore();
    result
}

fn run(
    terminal: &mut ratatui::DefaultTerminal,
    workspace: &mut WorkspaceRuntime,
) -> std::io::Result<()> {
    let tick_rate = Duration::from_millis(200);
    let mut last_tick = Instant::now();

    // Spawn a dedicated thread to read crossterm events and send them via
    // a channel. This avoids crossterm's internal event reader mutex which
    // deadlocks when a tokio runtime runs in a background thread.
    let (event_tx, event_rx) = std::sync::mpsc::channel::<Event>();
    std::thread::spawn(move || {
        loop {
            // Block on crossterm's read — this thread owns the event reader
            match event::read() {
                Ok(ev) => {
                    if event_tx.send(ev).is_err() {
                        break; // main thread dropped the receiver
                    }
                }
                Err(_) => break,
            }
        }
    });

    loop {
        // Check atomic quit flag BEFORE draw — ensures exit even if previous
        // render was slow due to mutex contention.
        trace_log("loop:top");

        if QUIT.load(Ordering::Relaxed) {
            return Ok(());
        }

        // Sync input mode flag for plugins to read during render (atomic — no lock needed)
        {
            let mode = workspace.active_runtime().mode();
            INPUT_MODE_ACTIVE.store(mode == InputMode::PluginInput, Ordering::Relaxed);
        }

        trace_log("loop:draw_start");
        terminal.draw(|frame| {
            let [tabs, body, footer] = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .areas(frame.area());

            render_tabs(workspace, tabs, frame.buffer_mut());
            workspace.render(body, frame.buffer_mut());

            // Footer
            let rt = workspace.active_runtime();
            let [mode_area, hint_area] =
                Layout::horizontal([Constraint::Length(10), Constraint::Min(0)]).areas(footer);
            ModeIndicator::new(rt.mode()).render(mode_area, frame.buffer_mut());

            Paragraph::new(Line::from(vec![
                Span::styled(
                    "  s/v",
                    Style::default()
                        .fg(theme::GREEN())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":split  ", Style::default().fg(theme::text_muted())),
                Span::styled(
                    "d",
                    Style::default()
                        .fg(theme::RED())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":close  ", Style::default().fg(theme::text_muted())),
                Span::styled(
                    "hjkl",
                    Style::default()
                        .fg(theme::YELLOW())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":nav  ", Style::default().fg(theme::text_muted())),
                Span::styled(
                    "i",
                    Style::default()
                        .fg(theme::CYAN())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":input  ", Style::default().fg(theme::text_muted())),
                Span::styled(
                    "p",
                    Style::default()
                        .fg(theme::MAGENTA())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":new_panel  ", Style::default().fg(theme::text_muted())),
                Span::styled(
                    "t",
                    Style::default()
                        .fg(theme::BLUE())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":themes  ", Style::default().fg(theme::text_muted())),
                Span::styled(
                    "Ctrl+t/w",
                    Style::default()
                        .fg(theme::ORANGE())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":tab  ", Style::default().fg(theme::text_muted())),
                Span::styled(
                    "Ctrl+c",
                    Style::default()
                        .fg(theme::RED())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":quit  ", Style::default().fg(theme::text_muted())),
                Span::styled(
                    "Esc",
                    Style::default()
                        .fg(theme::YELLOW())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(":normal", Style::default().fg(theme::text_muted())),
            ]))
            .style(Style::default().bg(theme::bg_panel()))
            .render(hint_area, frame.buffer_mut());

            // Paint selection highlight over the buffer
            if let Ok(state) = shared().try_lock() {
                let sel = &state.selection;
                if sel.has_selection() {
                    let (start, end) = sel.ordered();
                    let area = sel.pane_rect.unwrap_or(frame.area());
                    let mut selected_text = String::new();

                    let buf = frame.buffer_mut();
                    for row in start.1..=end.1 {
                        if row < area.y || row >= area.y + area.height {
                            continue;
                        }
                        let col_start = if row == start.1 { start.0 } else { area.x };
                        let col_end = if row == end.1 { end.0 } else { area.x + area.width - 1 };

                        let mut row_text = String::new();
                        for col in col_start..=col_end {
                            if col < area.x || col >= area.x + area.width {
                                continue;
                            }
                            if let Some(cell) = buf.cell_mut((col, row)) {
                                let _old_bg = cell.bg;
                                cell.set_fg(Color::White);
                                cell.set_bg(theme::BLUE());
                                row_text.push_str(cell.symbol());
                            }
                        }
                        if !selected_text.is_empty() {
                            selected_text.push('\n');
                        }
                        selected_text.push_str(row_text.trim_end());
                    }
                    drop(state);

                    // Store the extracted text for clipboard copy
                    if !selected_text.is_empty() {
                        if let Ok(mut state) = shared().try_lock() {
                            state.selection.selected_text = selected_text;
                        }
                    }
                }
            }
        })?;

        trace_log("loop:draw_done");

        let timeout = workspace.next_frame_in().map_or_else(
            || tick_rate.saturating_sub(last_tick.elapsed()),
            |frame| frame.min(tick_rate.saturating_sub(last_tick.elapsed())),
        );

        // Use try_recv + sleep to avoid any futex/condvar that might hang.
        // Both crossterm's event::poll and mpsc::recv_timeout were observed
        // hanging when a tokio runtime runs in a background thread.
        let poll_deadline = Instant::now() + timeout;
        let mut maybe_event = None;
        while Instant::now() < poll_deadline {
            match event_rx.try_recv() {
                Ok(ev) => { maybe_event = Some(ev); break; }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            }
            if QUIT.load(Ordering::Relaxed) { return Ok(()); }
        }
        trace_log("loop:after_poll");
        if let Some(ref ev) = maybe_event {
            let desc = match ev {
                Event::Key(k) => format!("Key({:?} mods={:?})", k.code, k.modifiers),
                Event::Mouse(m) => format!("Mouse({:?})", m.kind),
                Event::Resize(w, h) => format!("Resize({},{})", w, h),
                Event::FocusGained => "FocusGained".into(),
                Event::FocusLost => "FocusLost".into(),
                _ => "Other".into(),
            };
            trace_log(&format!("loop:event={}", desc));
        }
        if let Some(ev) = maybe_event {
            match ev {
                Event::Key(key) => {
                    // Ctrl+Shift+C or Ctrl+C with selection = copy
                    // Note: some terminals report Ctrl+Shift+C as Char('C') with CONTROL,
                    // others include SHIFT. We handle both.
                    let is_ctrl_shift_c = key.modifiers.contains(KeyModifiers::CONTROL)
                        && (key.code == KeyCode::Char('C')
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::SHIFT)));
                    let is_ctrl_c = key.code == KeyCode::Char('c')
                        && key.modifiers == KeyModifiers::CONTROL;

                    if is_ctrl_shift_c || is_ctrl_c {
                        let mut state = match shared().try_lock() {
                            Ok(s) => s,
                            Err(_) => {
                                // shared() contended — if Ctrl+C, force quit via atomic flag
                                if is_ctrl_c {
                                    QUIT.store(true, Ordering::Relaxed);
                                    return Ok(());
                                }
                                continue;
                            }
                        };
                        if !state.selection.selected_text.is_empty() {
                            let text = state.selection.selected_text.clone();
                            state.selection = TextSelection::default();
                            drop(state);
                            match copy_to_clipboard(&text) {
                                Ok(_) => debug_log(format!(
                                    "[copy] {} chars via {}",
                                    text.len(),
                                    if is_ctrl_shift_c { "Ctrl+Shift+C" } else { "Ctrl+C" }
                                )),
                                Err(e) => debug_log(format!("[copy] {e}")),
                            }
                            continue; // Don't forward copy key to workspace
                        } else if is_ctrl_c {
                            // No selection: Ctrl+C toggles mode or quits
                            drop(state);
                            let mode = workspace.active_runtime().mode();
                            if mode == InputMode::PluginInput {
                                workspace.active_runtime_mut().set_mode(InputMode::Layout);
                            } else {
                                QUIT.store(true, Ordering::Relaxed);
                                return Ok(());
                            }
                            continue; // Don't forward to workspace
                        } else {
                            // Ctrl+Shift+C with no selection: do nothing (don't quit)
                            debug_log(format!(
                                "[copy] Ctrl+Shift+C pressed but no selection text"
                            ));
                            continue; // Don't forward to workspace
                        }
                    }
                    // Intercept Escape before hypertile steals it from plugins
                    // when a permission overlay is open. Hypertile unconditionally
                    // consumes Escape in PluginInput mode to switch to Layout.
                    if key.code == KeyCode::Esc
                        && key.modifiers == KeyModifiers::NONE
                        && workspace.active_runtime().mode() == InputMode::PluginInput
                        && dismiss_any_awaiting_permission()
                    {
                        // Permission was dismissed, stay in PluginInput mode
                        continue;
                    }
                    // 't' in layout mode opens theme selector
                    if key.code == KeyCode::Char('t')
                        && key.modifiers == KeyModifiers::NONE
                        && workspace.active_runtime().mode() == InputMode::Layout
                    {
                        let rt = workspace.active_runtime_mut();
                        let _ = rt.split_focused(Direction::Vertical, "themes");
                        rt.set_mode(InputMode::PluginInput);
                    } else if let Some(ev) = event_from_crossterm(key) {
                        workspace.handle_event(ev);
                    }
                }
                Event::Mouse(mouse) => {
                    match mouse.kind {
                        MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                            // Find which pane was clicked and focus it
                            let rt = workspace.active_runtime_mut();
                            let panes = rt.panes();
                            let mut clicked_rect = None;
                            for pane in &panes {
                                if mouse.column >= pane.rect.x
                                    && mouse.column < pane.rect.x + pane.rect.width
                                    && mouse.row >= pane.rect.y
                                    && mouse.row < pane.rect.y + pane.rect.height
                                {
                                    clicked_rect = Some(pane.rect);
                                    let _ = rt.focus_pane(pane.id);
                                    rt.set_mode(InputMode::PluginInput);
                                    break;
                                }
                            }
                            // Start text selection constrained to the clicked pane
                            {
                                let mut state = shared().lock().unwrap();
                                state.selection = TextSelection {
                                    active: true,
                                    start: (mouse.column, mouse.row),
                                    end: (mouse.column, mouse.row),
                                    selected_text: String::new(),
                                    pane_rect: clicked_rect,
                                };
                            }
                        }
                        MouseEventKind::Drag(crossterm::event::MouseButton::Left) => {
                            let mut state = shared().lock().unwrap();
                            if state.selection.active {
                                let (col, row) = if let Some(r) = state.selection.pane_rect {
                                    (
                                        mouse.column.max(r.x).min(r.x + r.width.saturating_sub(1)),
                                        mouse.row.max(r.y).min(r.y + r.height.saturating_sub(1)),
                                    )
                                } else {
                                    (mouse.column, mouse.row)
                                };
                                state.selection.end = (col, row);
                            }
                        }
                        MouseEventKind::Up(crossterm::event::MouseButton::Left) => {
                            let mut state = shared().lock().unwrap();
                            if state.selection.active {
                                state.selection.end = (mouse.column, mouse.row);
                                state.selection.active = false;
                                // If start == end, it was just a click, clear selection
                                if state.selection.start == state.selection.end {
                                    state.selection.selected_text.clear();
                                }
                                // selected_text will be extracted during render
                            }
                        }
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                            let prev_mode = workspace.active_runtime().mode();
                            workspace.active_runtime_mut().set_mode(InputMode::PluginInput);
                            let key = if matches!(mouse.kind, MouseEventKind::ScrollUp) {
                                HtKeyCode::Up
                            } else {
                                HtKeyCode::Down
                            };
                            workspace.handle_event(HypertileEvent::Key(
                                KeyChord::new(key),
                            ));
                            workspace.active_runtime_mut().set_mode(prev_mode);
                        }
                        _ => {}
                    }
                }
                Event::Paste(text) => {
                    // Bracketed paste: send all chars at once to avoid per-char redraws
                    for ch in text.chars() {
                        workspace.handle_event(HypertileEvent::Key(
                            KeyChord::new(HtKeyCode::Char(ch)),
                        ));
                    }
                }
                Event::FocusGained => {
                    // Terminal was re-focused — force a full redraw since the
                    // diff-based renderer may be stale after being backgrounded.
                    terminal.clear()?;
                }
                Event::Resize(_, _) => {
                    // Terminal resized — force full redraw
                    terminal.clear()?;
                }
                _ => {}
            }
            trace_log("loop:event_done");
        }

        trace_log("loop:tick_check");
        if last_tick.elapsed() >= tick_rate {
            trace_log("loop:before_tick");
            workspace.handle_event(HypertileEvent::Tick);
            trace_log("loop:after_tick");
            last_tick = Instant::now();
        }
    }
}

fn render_tabs(workspace: &WorkspaceRuntime, area: Rect, buf: &mut Buffer) {
    let spans: Vec<Span> = workspace
        .tab_labels()
        .enumerate()
        .flat_map(|(i, (label, active))| {
            let sep = if i > 0 {
                vec![Span::raw(" ")]
            } else {
                vec![]
            };
            let tab = if active {
                Span::styled(
                    format!(" {} ", label),
                    Style::default()
                        .fg(theme::bg_primary())
                        .bg(theme::CYAN())
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    format!(" {} ", label),
                    Style::default()
                        .fg(theme::text_secondary())
                        .bg(theme::bg_panel()),
                )
            };
            sep.into_iter().chain(std::iter::once(tab))
        })
        .collect();
    Line::from(spans).render(area, buf);
}
