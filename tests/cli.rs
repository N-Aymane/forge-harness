//! Exercise the binary boundary, JSONL envelopes, and unattended approval exit code.
use anyhow::Result;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

#[tokio::test]
async fn json_run_stops_for_approval_without_writing() -> Result<()> {
    let home = tempfile::tempdir()?;
    let workspace = tempfile::tempdir()?;
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let config = format!(
        "default_model = 'fixture'\n[models.fixture]\nprovider = 'compatible'\nmodel = 'fixture'\nbase_url = 'http://127.0.0.1:{port}/v1'\nkey_env = 'FORGE_FIXTURE_KEY'\ncontext_tokens = 100000\nmax_output_tokens = 100\n"
    );
    std::fs::write(home.path().join("config.toml"), config)?;
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 4096];
        let boundary = loop {
            let n = socket.read(&mut buffer).await?;
            anyhow::ensure!(n > 0, "disconnected");
            bytes.extend_from_slice(&buffer[..n]);
            if let Some(i) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                break i + 4;
            }
        };
        let header = String::from_utf8_lossy(&bytes[..boundary]);
        let length: usize = header
            .lines()
            .find_map(|s| {
                s.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>())
            })
            .transpose()?
            .unwrap();
        while bytes.len() < boundary + length {
            let n = socket.read(&mut buffer).await?;
            anyhow::ensure!(n > 0, "disconnected");
            bytes.extend_from_slice(&buffer[..n]);
        }
        let delta = json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call-1","function":{"name":"write_file","arguments":json!({"path":"never.txt","content":"must need approval"}).to_string()}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":10}});
        let body = format!("data: {delta}\n\ndata: [DONE]\n\n");
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await?;
        Ok::<_, anyhow::Error>(())
    });
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_forge"))
        .arg("--home")
        .arg(home.path())
        .arg("-C")
        .arg(workspace.path())
        .args(["run", "create a file", "--json"])
        .env("FORGE_FIXTURE_KEY", "test-private-key")
        .output()
        .await?;
    server.await??;
    assert_eq!(
        output.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!workspace.path().join("never.txt").exists());
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("test-private-key"));
    let events: Vec<Value> = stdout
        .lines()
        .map(serde_json::from_str)
        .collect::<serde_json::Result<_>>()?;
    assert!(
        events
            .iter()
            .all(|e| e["version"] == 1 && e["session"].is_string())
    );
    assert!(events.iter().any(|e| e["type"] == "approval_required"
        && e["preview"].as_str().unwrap().contains("never.txt")));
    assert_eq!(events.last().unwrap()["status"], "approval_required");
    Ok(())
}

#[test]
fn doctor_works_without_keys_or_network() {
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_forge"))
        .arg("--home")
        .arg(home.path())
        .arg("doctor")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("No models configured"));
}
