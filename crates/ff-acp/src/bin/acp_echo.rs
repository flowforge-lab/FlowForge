use serde_json::{json, Value};
use std::io::Write;
use tokio::io::{self, AsyncBufReadExt, BufReader};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut session_counter = 0u64;
    let mut lines = BufReader::new(io::stdin()).lines();

    while let Some(line) = lines.next_line().await? {
        let trimmed = line.trim().to_owned();
        if trimmed.is_empty() {
            continue;
        }

        let request: Value = match serde_json::from_str(&trimmed) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("parse error: {e}");
                continue;
            }
        };

        let method = request["method"].as_str().unwrap_or("").to_string();
        let id = request.get("id").cloned();

        match method.as_str() {
            "initialize" => reply(id, None, Some(initialize())),
            "session/new" => {
                session_counter += 1;
                reply(id, None, Some(session_new(session_counter)));
            }
            "session/prompt" => {
                let session_id = request["params"]["sessionId"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                send_update(&session_id, "Hello from mock agent (1/2)");
                send_update(&session_id, "Hello from mock agent (2/2)");
                reply(id, None, Some(session_prompt()));
            }
            "session/cancel" => {}
            "session/set_mode" => reply(id, None, Some(session_set_mode())),
            _ => {
                if id.is_some() {
                    reply(id, Some(unknown_method()), None);
                }
            }
        }
    }

    Ok(())
}

fn initialize() -> Value {
    json!({
        "protocolVersion": 1,
        "agentCapabilities": {}
    })
}

fn session_new(counter: u64) -> Value {
    json!({
        "sessionId": format!("sess-{counter}")
    })
}

fn session_prompt() -> Value {
    json!({
        "stopReason": "end_turn"
    })
}

fn session_set_mode() -> Value {
    json!({
        "success": true
    })
}

fn unknown_method() -> Value {
    json!({
        "code": -32601,
        "message": "method not found"
    })
}

fn reply(id: Option<Value>, error: Option<Value>, result: Option<Value>) {
    let id = match id {
        Some(id) => id,
        None => return,
    };

    let mut resp = json!({
        "jsonrpc": "2.0",
        "id": id,
    });

    if let Some(err) = error {
        resp["error"] = err;
    } else if let Some(res) = result {
        resp["result"] = res;
    }

    println!("{}", serde_json::to_string(&resp).unwrap());
    let _ = std::io::stdout().flush();
}

fn send_update(session_id: &str, text: &str) {
    let notification = json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": {
                    "type": "text",
                    "text": text
                }
            }
        }
    });
    println!("{}", serde_json::to_string(&notification).unwrap());
    let _ = std::io::stdout().flush();
}
