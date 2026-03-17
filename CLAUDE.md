# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**lfg** (Looking For Group) is a WoW-inspired raid frames display for AI coding agents on a 64x64 iDotMatrix LED panel. Each agent (Claude Code, Cursor) gets a sprite and status icon — making idle/working/approval states visible in peripheral vision.

## Commands

```bash
# Build
cargo build --release

# Run (auto-discovers IDM-* BLE device)
cargo run -- [--no-ble] [--port 5555] [--device AA:BB:CC:DD:EE:FF]

# Run without hardware (HTTP-only mode for testing)
cargo run -- --no-ble

# Lint / check
cargo clippy
cargo check

# Tests (none currently, use --no-ble + curl for manual testing)
cargo test

# Enable debug logging
RUST_LOG=lfg=debug cargo run -- --no-ble
```

## Architecture

```
IDE hooks → POST /webhook → event.rs (state machine) → render.rs (GIF) → ble.rs → LED panel
```

### Modules

| File | Role |
|------|------|
| `main.rs` | CLI args, SQLite init, spawns async tasks (HTTP, BLE loop, render loop) |
| `state.rs` | Shared state types: `DisplayState`, `Agent`, `Column`, `Host`, `AgentState` |
| `event.rs` | State machine handling `PreToolUse`, `PostToolUse`, `PermissionRequest`, `Stop`, `SessionEnd`; also stale agent cleanup |
| `gateway.rs` | Host/column slot allocation — assigns agents to positions (5 columns × 2 rows) |
| `render.rs` | Canvas drawing, sprite/icon compositing, 6-frame animated GIF encoding |
| `sprites.rs` | 11 pixel art themes, ability icons, 4×5 font — large, rarely modified |
| `ble.rs` | BLE discovery, connection, GIF packet protocol (16-byte header + 4KB chunks), heartbeat, exponential backoff reconnection |
| `http.rs` | Axum routes: `/webhook`, `/status`, `/hosts`, `/theme`, `/reset`, `/debug/*` |
| `db.rs` | SQLite: persists cumulative `tool_calls`, `unique_agents`, `agent_minutes` across restarts |

### State Machine Rules (event.rs)

These are intentional design decisions — preserve them:
- **Requesting is sticky**: only cleared by `PostToolUse` (approval granted) or `Stop`
- **PreToolUse won't override Requesting**: prevents fire-icon flicker while agent is blocked
- **PostToolUse does NOT transition to Idle**: tools fire in rapid succession; only `Stop` means truly idle
- Stale agents (>5 min since last event) are cleaned up periodically

### Render Pipeline

1. Every 250ms: hash `DisplayState`, compare to last sent frame
2. On change: start 2-second debounce (rapid tool calls don't spam the display)
3. After 2s stability: render 6-frame animated GIF
4. Split into 4KB BLE packets with 16-byte header (length, type, flags, gif_size, CRC32)
5. Requesting state → 0.5s/frame (fast pulse); otherwise 1.25s/frame

### Webhook Format

```
POST /webhook?host=claude
Body: { "text": "PreToolUse|session-id|ToolName" }

Optional headers:
  X-Agent-Host: claude
  X-Agent-Profile: Slimes/0        (theme_name/sprite_index)
  X-Agent-Pixels: <base64 PNG>     (8×8 custom avatar)
```

Event types in `text` field: `PreToolUse`, `PostToolUse`, `PermissionRequest`, `Stop`, `SessionEnd`

## BLE Protocol

- Service UUIDs: `fa02` (write), `fa03` (notify)
- Packet header: 16 bytes — payload length, type=`0x0A`, flags, total gif size, CRC32
- Screen on: `0x05 0x00 0x07 0x01 0x01`; Brightness: `0x05 0x00 0x04 0x80 100`
- Heartbeat: 30-second keepalive ping
- Reconnection: reuses the same BLE adapter (do not recreate on reconnect — see `ble.rs`)

## Key Design Constraints

- Max 5 columns × 2 rows = 10 simultaneous agents
- Canvas is 64×64 pixels; sprites are 8×8; icons occupy a 4×4 quadrant of a sprite cell
- `sprites.rs` is ~24KB of pixel data — avoid large refactors; add new themes by following the existing `Theme` struct pattern
- The `SharedState` is an `Arc<Mutex<DisplayState>>` — keep lock durations short; render/BLE run in separate tasks
