use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::env;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_tungstenite::tungstenite::Message;

enum ClientState {
    Idle,
    Running,
    AwaitingPermission,
}

fn shorten_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home_str = home.display().to_string();
        if let Some(rest) = path.strip_prefix(&home_str) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

fn print_prompt(state: &ClientState, cwd: &str) {
    match state {
        ClientState::Idle => {
            let display = shorten_path(cwd);
            print!("{display}> ");
        }
        ClientState::AwaitingPermission => print!("[y/n]> "),
        ClientState::Running => return,
    }
    let _ = std::io::stdout().flush();
}

#[tokio::main]
async fn main() {
    let url = match env::args().nth(1) {
        Some(u) => u,
        None => {
            eprintln!("Usage: ws-client <websocket-url>");
            eprintln!("  e.g. ws-client ws://127.0.0.1:9753/ws?key=abcdef1234567890");
            std::process::exit(1);
        }
    };

    println!("Connecting to {}...", url);

    let (ws_stream, _) = match tokio_tungstenite::connect_async(&url).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Connection failed: {e}");
            std::process::exit(1);
        }
    };

    let (mut write, mut read) = ws_stream.split();
    let mut stdin = BufReader::new(tokio::io::stdin()).lines();
    let mut state = ClientState::Idle;
    let mut cwd = String::new();

    loop {
        tokio::select! {
            line = stdin.next_line() => {
                match line {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() {
                            print_prompt(&state, &cwd);
                            continue;
                        }

                        match &state {
                            ClientState::AwaitingPermission => {
                                let allow = matches!(line.as_str(), "y" | "Y" | "yes" | "Yes");
                                let msg = serde_json::json!({"type": "permission", "allow": allow});
                                if send(&mut write, &msg.to_string()).await.is_err() {
                                    break;
                                }
                                state = ClientState::Running;
                            }
                            _ => {
                                if !handle_command(&line, &mut write, &mut state, &mut cwd).await {
                                    break;
                                }
                            }
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("stdin error: {e}");
                        break;
                    }
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<Value>(&text) {
                            Ok(val) => handle_server_message(&val, &mut state, &mut cwd),
                            Err(_) => println!("{text}"),
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        println!("Server closed connection.");
                        break;
                    }
                    Some(Err(e)) => {
                        eprintln!("WebSocket error: {e}");
                        break;
                    }
                    None => {
                        println!("Connection closed.");
                        break;
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Handle slash commands. Returns false if the connection should close.
async fn handle_command(
    line: &str,
    write: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    state: &mut ClientState,
    cwd: &mut String,
) -> bool {
    match line {
        "/quit" | "/exit" => {
            println!("Disconnecting...");
            let _ = write.close().await;
            std::process::exit(0);
        }
        "/kill" => {
            let msg = serde_json::json!({"type": "kill"});
            if send(write, &msg.to_string()).await.is_err() {
                return false;
            }
            println!("Kill sent.");
            *state = ClientState::Idle;
            print_prompt(state, cwd);
        }
        "/clear" => {
            // Clear terminal with ANSI escape codes
            print!("\x1b[2J\x1b[H");
            let _ = std::io::stdout().flush();
            print_prompt(state, cwd);
        }
        "/dangerously-skip-permissions" => {
            let msg = serde_json::json!({"type": "skip_permissions"});
            if send(write, &msg.to_string()).await.is_err() {
                return false;
            }
        }
        _ if line == "/cd" || line.starts_with("/cd ") => {
            let path = if line == "/cd" {
                String::new()
            } else {
                line[4..].trim().to_string()
            };
            let msg = serde_json::json!({"type": "cd", "path": path});
            if send(write, &msg.to_string()).await.is_err() {
                return false;
            }
        }
        _ if line == "/resume" || line.starts_with("/resume ") => {
            let sid = if line == "/resume" {
                String::new()
            } else {
                line[8..].trim().to_string()
            };
            if sid.is_empty() {
                println!("Usage: /resume <session-id>");
                print_prompt(state, cwd);
            } else {
                let msg = serde_json::json!({"type": "resume", "session_id": sid});
                if send(write, &msg.to_string()).await.is_err() {
                    return false;
                }
            }
        }
        _ => {
            // Send as prompt (includes /btw and anything else)
            let msg = serde_json::json!({"type": "prompt", "message": line});
            if send(write, &msg.to_string()).await.is_err() {
                return false;
            }
            *state = ClientState::Running;
        }
    }
    true
}

fn handle_server_message(val: &Value, state: &mut ClientState, cwd: &mut String) {
    match val.get("type").and_then(|t| t.as_str()) {
        Some("connected") => {
            let id = val.get("session_id").and_then(|v| v.as_u64()).unwrap_or(0);
            if let Some(c) = val.get("cwd").and_then(|v| v.as_str()) {
                *cwd = c.to_string();
            }
            println!("Connected! Session #{id} created.");
            println!("Type a prompt to start, /quit to exit.\n");
            *state = ClientState::Idle;
            print_prompt(state, cwd);
        }
        Some("output") => {
            let line = val.get("line").and_then(|v| v.as_str()).unwrap_or("");
            println!("{line}");
        }
        Some("state") => {
            let new_state = val.get("state").and_then(|v| v.as_str()).unwrap_or("");
            match new_state {
                "idle" => {
                    *state = ClientState::Idle;
                    println!();
                    print_prompt(state, cwd);
                }
                "running" => {
                    *state = ClientState::Running;
                }
                "permission" => {
                    *state = ClientState::AwaitingPermission;
                }
                _ => {}
            }
        }
        Some("permission") => {
            let tool = val.get("tool").and_then(|v| v.as_str()).unwrap_or("unknown");
            let command = val.get("command").and_then(|v| v.as_str()).unwrap_or("");
            println!("\n--- Permission Request ---");
            println!("Tool: {tool}");
            if !command.is_empty() {
                println!("{command}");
            }
            println!("--------------------------");
            *state = ClientState::AwaitingPermission;
            print_prompt(state, cwd);
        }
        Some("cd") => {
            if let Some(c) = val.get("cwd").and_then(|v| v.as_str()) {
                *cwd = c.to_string();
                println!("  [cd] -> {}", shorten_path(c));
            }
            print_prompt(state, cwd);
        }
        Some("cwd") => {
            if let Some(c) = val.get("path").and_then(|v| v.as_str()) {
                *cwd = c.to_string();
            }
        }
        Some("skip_permissions") => {
            let enabled = val.get("enabled").and_then(|v| v.as_bool()).unwrap_or(false);
            let status = if enabled { "ON" } else { "OFF" };
            println!("  [permissions] auto-accept is now {status}");
            print_prompt(state, cwd);
        }
        Some("error") => {
            let msg = val.get("message").and_then(|v| v.as_str()).unwrap_or("unknown error");
            eprintln!("Error: {msg}");
            print_prompt(state, cwd);
        }
        _ => {
            println!("{}", serde_json::to_string_pretty(val).unwrap_or_default());
        }
    }
}

async fn send(
    write: &mut futures::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    text: &str,
) -> Result<(), ()> {
    write
        .send(Message::Text(text.into()))
        .await
        .map_err(|e| eprintln!("Send error: {e}"))
}
