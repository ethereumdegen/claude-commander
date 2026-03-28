# Past Bugs

## TUI Freeze on Linux: arboard clipboard `.wait()` blocks indefinitely

**Date**: 2026-03-27

**Symptoms**:
- Entire TUI freezes and becomes unresponsive
- Ctrl+C doesn't work
- Appeared to be related to WebSocket connections, but was actually triggered by pressing 'c' (copy key) while WS panel was focused, or Ctrl+C when text was selected
- Happened "sometimes" — only when a key event triggered clipboard access

**Root Cause**:
`copy_to_clipboard()` used arboard's `.wait()` method on Linux. On Linux/Wayland/X11, `.wait()` blocks the calling thread **indefinitely** until another application reads the clipboard. Since this ran on the TUI thread, the entire UI froze.

```rust
// BAD - blocks TUI thread forever on Linux
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    clipboard.set().wait().text(text).map_err(|e| e.to_string())
}
```

**Fix**:
Moved the clipboard operation to a background thread so it can't block the TUI:

```rust
// GOOD - non-blocking
fn copy_to_clipboard(text: &str) -> Result<(), String> {
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
```

**Debugging journey**:
This was extremely hard to diagnose because:
1. The freeze looked like a mutex deadlock (many shared Mutex locks between TUI and WS threads)
2. Crossterm's event system uses internal mutexes, another red herring
3. File-based trace logging (`/tmp/claude-commander-trace.log`) finally revealed the last event before every freeze was `Key(Char('c'))`, pointing to the clipboard code

**Lesson**: Never call blocking/indefinite operations on the TUI thread. On Linux, arboard's `.wait()` is essentially a "block until someone else pastes" call.

**Collateral improvements made during investigation**:
- All render-path mutex locks converted to `try_lock()` (prevents future deadlocks)
- `output_lines` capped at 10,000 (prevents unbounded memory growth)
- WS sessions cleaned up on disconnect (prevents HashMap leak)
- Dedicated event reader thread for crossterm (avoids crossterm's internal mutex)
- Atomic `QUIT` flag as Ctrl+C fallback
