# Degen Studio TUI - Implementation Plan

## Overview
A ratatui-based TUI app using ratatui-hypertile for BSP tiling layout. Run multiple Claude sessions in split tiles + view token usage with colorful bar graphs.

## Architecture

```
src/
  main.rs          - Entry point, event loop, terminal setup
  app.rs           - App state, mode management, keybindings
  claude.rs        - Claude CLI session spawning & management
  usage.rs         - Token usage data fetching & bar graph rendering
  theme.rs         - Color palette (dark theme from degen-studio)
  tiles.rs         - Hypertile integration, per-pane content routing
```

## Features
1. **Tiled Claude Sessions** - Each tile runs an independent Claude REPL
   - Split h/v, close, navigate with vim keys (via hypertile-extras)
   - Input prompt at bottom of each tile, streaming output above
   - Session persistence via --session-id / --resume

2. **Usage Dashboard** - Toggle with `u` key
   - Fetch from ~/.claude/stats-cache.json + live API
   - Colorful bar graphs: daily activity, utilization buckets
   - Model breakdown with token counts

3. **Keybindings** (hypertile defaults + custom)
   - s/v: split horizontal/vertical
   - hjkl: navigate panes
   - d: close pane
   - u: toggle usage view
   - Enter: submit prompt in focused pane
   - Ctrl+c: quit

## Dependencies
- ratatui 0.30
- ratatui-hypertile 0.3
- ratatui-hypertile-extras 0.3
- crossterm 0.29
- serde + serde_json
- tokio (async Claude process management)
- chrono (timestamps)
- uuid (session IDs)

## Data Flow
1. Each tile = ClaudeSession { session_id, output_lines, input_buf, running, cost }
2. On Enter: spawn `claude -p --output-format json --session-id {id} "{prompt}"`
3. Stream stdout line-by-line into output_lines
4. Parse JSON response for session_id, cost_usd, result
5. Usage view: read ~/.claude/.credentials.json for OAuth token, fetch /api/oauth/usage
