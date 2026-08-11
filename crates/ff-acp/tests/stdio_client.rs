//! Integration tests: the client handshakes a real ACP agent over child-process
//! stdio, creates a session, sends a prompt, and receives updates. The agent is
//! `acp_echo`, a crate bin, so the whole path runs in CI with no network or
//! external server.

use std::time::Duration;

use ff_acp::client::AcpClient;
use ff_acp::config::AcpAgentConfig;

fn echo_config() -> AcpAgentConfig {
    AcpAgentConfig {
        id: "echo".into(),
        command: env!("CARGO_BIN_EXE_acp_echo").to_string(),
        args: vec![],
        env: std::collections::BTreeMap::new(),
        disabled: false,
    }
}

fn sdk_config(cfg: &AcpAgentConfig) -> agent_client_protocol::AcpAgentConfig {
    let mut acp = agent_client_protocol::AcpAgentConfig::new(cfg.command.clone());
    for arg in &cfg.args {
        acp = acp.arg(arg.clone());
    }
    for (k, v) in &cfg.env {
        acp = acp.env(k.clone(), v.clone());
    }
    acp
}

#[tokio::test]
async fn connect_initialize_handshake_over_stdio() {
    let client = AcpClient::connect(sdk_config(&echo_config()))
        .await
        .expect("connect + initialize");
    client.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn session_new_and_prompt_receives_updates() {
    let client = AcpClient::connect(sdk_config(&echo_config()))
        .await
        .expect("connect + initialize");

    let session_id = client
        .session_new(std::path::PathBuf::from("/tmp"))
        .await
        .expect("session/new");

    eprintln!("DEBUG: session_id={:?}", session_id);

    let mut rx = client
        .prompt(session_id, "Hello".into())
        .await
        .expect("session/prompt");

    // Should receive at least one update before Done.
    let mut got_content = false;
    let mut got_done = false;
    while let Some(inbound) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout waiting for stream event")
    {
        match inbound {
            ff_acp::content::Inbound::Agent(event) => match event {
                ff_agent::AgentEvent::Done { .. } => {
                    got_done = true;
                    break;
                }
                _ => {
                    got_content = true;
                }
            },
            ff_acp::content::Inbound::ModeChanged { .. } | ff_acp::content::Inbound::Ignored => {}
        }
    }
    assert!(
        got_content,
        "should have received at least one content update"
    );
    assert!(got_done, "stream should end with Done");
    client.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn cancel_stops_the_prompt_stream() {
    let client = AcpClient::connect(sdk_config(&echo_config()))
        .await
        .expect("connect + initialize");

    let session_id = client
        .session_new(std::path::PathBuf::from("/tmp"))
        .await
        .expect("session/new");

    let mut rx = client
        .prompt(session_id.clone(), "Hello".into())
        .await
        .expect("session/prompt");

    // Cancel the current turn. Since the mock agent does not support cancellation
    // (session/cancel is a notification that the agent may ignore), this test
    // verifies that cancel does not error and the stream eventually closes.
    client.cancel(session_id).await.expect("session/cancel");

    // The stream should end (either with Done or by the channel closing).
    let mut stream_ended = false;
    while let Some(inbound) = tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timeout waiting for stream event")
    {
        if let ff_acp::content::Inbound::Agent(event) = inbound {
            if matches!(event, ff_agent::AgentEvent::Done { .. }) {
                stream_ended = true;
                break;
            }
        }
    }
    assert!(stream_ended, "stream should end with Done after cancel");
    client.shutdown().await.expect("shutdown");
}

/// A non-existent binary should fail the handshake.
#[tokio::test]
async fn nonexistent_binary_fails_handshake() {
    let cfg = AcpAgentConfig {
        id: "nonexistent".into(),
        command: "does-not-exist-hopefully-12345".into(),
        args: vec![],
        env: std::collections::BTreeMap::new(),
        disabled: false,
    };
    let err = AcpClient::connect(sdk_config(&cfg)).await;
    assert!(
        err.is_err(),
        "connecting to a non-existent binary should fail"
    );
}

/// Shutdown handles a clean agent gracefully.
#[tokio::test]
async fn clean_shutdown_completes() {
    let client = AcpClient::connect(sdk_config(&echo_config()))
        .await
        .expect("connect + initialize");
    client
        .shutdown()
        .await
        .expect("shutdown should complete cleanly");
}

/// Reconnecting with a new client after shutdown works.
#[tokio::test]
async fn reconnect_after_shutdown() {
    let client = AcpClient::connect(sdk_config(&echo_config()))
        .await
        .expect("first connect");
    client.shutdown().await.expect("first shutdown");

    let client2 = AcpClient::connect(sdk_config(&echo_config()))
        .await
        .expect("second connect");
    client2.shutdown().await.expect("second shutdown");
}
