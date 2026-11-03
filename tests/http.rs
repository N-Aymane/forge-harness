//! Local HTTP fixtures verify request shapes and streaming for every adapter.
use anyhow::Result;
use forge_harness::{
    config::{ModelConfig, ProviderKind},
    credentials::Secret,
    model::{Message, Role},
    providers::{HttpProvider, Provider},
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn adapters_send_valid_tools_and_redact_fragmented_keys() -> Result<()> {
    for kind in [
        ProviderKind::Openai,
        ProviderKind::Anthropic,
        ProviderKind::Compatible,
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let frames = match kind {
            ProviderKind::Openai => vec![
                json!({"type":"response.output_text.delta","delta":"hello sk-"}),
                json!({"type":"response.output_text.delta","delta":"private"}),
                json!({"type":"response.completed","response":{"usage":{"input_tokens":10,"output_tokens":3}}}),
            ],
            ProviderKind::Anthropic => vec![
                json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":0}}}),
                json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"hello sk-"}}),
                json!({"type":"content_block_delta","delta":{"type":"text_delta","text":"private"}}),
                json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":3}}),
                json!({"type":"message_stop"}),
            ],
            ProviderKind::Compatible => vec![
                json!({"choices":[{"delta":{"content":"hello sk-"}}]}),
                json!({"choices":[{"delta":{"content":"private"},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":3}}),
            ],
        };
        let mut body = frames
            .iter()
            .map(|v| format!("data: {v}\n\n"))
            .collect::<String>();
        if kind == ProviderKind::Compatible {
            body.push_str("data: [DONE]\n\n");
        }
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await?;
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            let boundary = loop {
                let n = socket.read(&mut buffer).await?;
                anyhow::ensure!(n > 0, "request disconnected");
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
                anyhow::ensure!(n > 0, "request disconnected");
                bytes.extend_from_slice(&buffer[..n]);
            }
            let request = serde_json::from_slice(&bytes[boundary..boundary + length])?;
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await?;
            for chunk in body.as_bytes().chunks(7) {
                socket.write_all(chunk).await?;
                tokio::task::yield_now().await;
            }
            Ok::<Value, anyhow::Error>(request)
        });
        let model = ModelConfig {
            provider: kind,
            model: "fixture".into(),
            base_url: format!("http://{addr}/v1"),
            key_env: None,
            context_tokens: 50000,
            max_output_tokens: 100,
            pricing: None,
        };
        let provider = HttpProvider::new(model, Secret::new("sk-private".into())?)?;
        let (tx, mut rx) = mpsc::channel(64);
        let result = provider
            .complete(
                &[Message::text(Role::User, "hello")],
                true,
                tx,
                &CancellationToken::new(),
            )
            .await?;
        let mut streamed = String::new();
        while let Some(s) = rx.recv().await {
            streamed.push_str(&s);
        }
        assert_eq!(streamed, "hello [REDACTED]");
        assert_eq!(result.text, streamed);
        assert_eq!(result.usage.unwrap().output, 3);
        let body = server.await??;
        assert!(body["tools"].as_array().unwrap().len() >= 5);
        match kind {
            ProviderKind::Openai => assert!(body["input"].is_array()),
            _ => assert!(body["messages"].is_array()),
        }
    }
    Ok(())
}
