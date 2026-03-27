# Claude Commander

A terminal UI for running multiple Claude Code sessions side-by-side in a tiled layout. Built with [ratatui](https://github.com/ratatui/ratatui) and [ratatui-hypertile](https://github.com/ratatui/ratatui-hypertile) for BSP tiling.

<img width="1447" height="856" alt="image" src="https://github.com/user-attachments/assets/dba4a345-06a5-422e-9eca-fe99104ecae6" />


## Features

- **Tiled Claude Sessions** — Split your terminal into multiple panes, each running an independent Claude CLI session with full streaming output
- **Session Persistence** — Sessions are tracked by ID and can be resumed across restarts
- **Permission Handling** — Interactive approval/denial of tool use requests directly in the TUI, including AskUserQuestion support with selectable options
- **Token Usage Dashboard** — View daily activity charts, model breakdowns, and live API utilization gauges (5h window, 7d overall, Sonnet, Opus)
- **20 Built-in Themes** — Midnight Blue, Dracula, Monokai Pro, Solarized, Gruvbox, Nord, Catppuccin, Tokyo Night, One Dark, Cyberpunk, Synthwave, Matrix, Rose Pine, Everforest, Kanagawa, Ayu Dark, Palenight, and more. Theme preference is persisted.
- **Mouse Support** — Click-to-focus panes, scroll output, drag-to-select and copy text
- **Debug Panel** — Toggle a debug log showing raw stream events, tool calls, and session lifecycle

## Requirements

- Rust 1.88+ (edition 2024)
- [Claude CLI](https://docs.anthropic.com/en/docs/claude-code) installed and authenticated (`claude` on PATH)
- A terminal with 256-color or truecolor support

## Installation

```bash
git clone https://github.com/ethereumdegen/claude-commander.git
cd claude-commander
cargo build --release
./target/release/claude-commander
```

## Keybindings

| Key | Action |
|-----|--------|
| `s` | Split pane horizontally |
| `v` | Split pane vertically |
| `h` `j` `k` `l` | Navigate between panes |
| `d` | Close focused pane |
| `Enter` | Submit prompt to Claude |
| `Esc` | Cancel running request |
| `u` | Toggle usage dashboard |
| `t` | Cycle theme |
| `Tab` | Toggle debug panel |
| `y` / `a` / `n` | Allow / Allow with options / Deny permission requests |
| `Ctrl+c` | Quit |

## Architecture

```
src/
  main.rs    — Event loop, terminal setup, pane rendering, input handling
  claude.rs  — Claude CLI process spawning, stream-json parsing, session management
  usage.rs   — Token usage data from ~/.claude/stats-cache.json + live API
  theme.rs   — 20 color themes with persistence
```

The app spawns Claude CLI processes in `stream-json` mode with bidirectional stdin/stdout communication. Each pane maintains its own session with independent state, output buffer, and cost tracking.

## License

MIT
