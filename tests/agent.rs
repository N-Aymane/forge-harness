//! Complete runtime tests with no real keys or paid API calls.
use anyhow::Result;
use async_trait::async_trait;
use forge_harness::{
    accounting::Pricing,
    config::{ModelConfig, ProviderKind},
    model::{Completion, Event, Message, Role, ToolCall, Usage},
    providers::Provider,
    runtime::{Agent, Frontend},
    storage::Store,
    tools::Tools,
};
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct Mock {
    responses: Mutex<VecDeque<Completion>>,
}
#[async_trait]
impl Provider for Mock {
    async fn complete(
        &self,
        _messages: &[Message],
        _tools: bool,
        text: mpsc::Sender<String>,
        _cancel: &CancellationToken,
    ) -> Result<Completion> {
        let r = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected model request");
        if !r.text.is_empty() {
            text.send(r.text.clone()).await?;
        }
        Ok(r)
    }
}
struct Display {
    decision: Option<bool>,
    events: Mutex<Vec<Event>>,
}
#[async_trait]
impl Frontend for Display {
    async fn event(&self, e: Event) -> Result<()> {
        self.events.lock().unwrap().push(e);
        Ok(())
    }
    async fn approve(&self, _id: &str, _title: &str, _preview: &str) -> Result<Option<bool>> {
        Ok(self.decision)
    }
}
fn model() -> ModelConfig {
    ModelConfig {
        provider: ProviderKind::Compatible,
        model: "mock".into(),
        base_url: "http://127.0.0.1:1/v1".into(),
        key_env: None,
        context_tokens: 100000,
        max_output_tokens: 1000,
        pricing: Some(Pricing {
            input_nusd_per_token: 1,
            output_nusd_per_token: 2,
            cached_input_nusd_per_token: 1,
            cache_write_nusd_per_token: Some(1),
        }),
    }
}
fn response(calls: Vec<ToolCall>) -> Completion {
    Completion {
        text: if calls.is_empty() {
            "Done.".into()
        } else {
            "I propose a change.".into()
        },
        calls,
        usage: Some(Usage {
            input: 100,
            output: 10,
            ..Default::default()
        }),
    }
}
fn call() -> ToolCall {
    ToolCall {
        id: "write-1".into(),
        name: "write_file".into(),
        arguments: json!({"path":"hello.txt","content":"hello\n"}),
    }
}
fn agent(dir: &std::path::Path, decision: Option<bool>, responses: Vec<Completion>) -> Agent {
    let store = Store::open(&dir.join("state.db")).unwrap();
    let tools = Tools::new(dir, None).unwrap();
    let session = store
        .create_session(&tools.root().to_string_lossy(), "mock")
        .unwrap();
    Agent {
        store,
        session,
        model: model(),
        provider: Arc::new(Mock {
            responses: Mutex::new(responses.into()),
        }),
        tools,
        frontend: Arc::new(Display {
            decision,
            events: Mutex::new(vec![]),
        }),
        cancel: CancellationToken::new(),
    }
}

#[tokio::test]
async fn context_compaction_is_paid_and_keeps_history() {
    let d = tempfile::tempdir().unwrap();
    let mut a = agent(
        d.path(),
        Some(true),
        vec![response(vec![]), response(vec![])],
    );
    a.initialize(Some("Keep my preferences")).await.unwrap();
    a.store
        .append(
            &a.session,
            &Message::text(Role::User, "older discussion ".repeat(1000)),
        )
        .unwrap();
    a.store
        .append(
            &a.session,
            &Message::text(Role::Assistant, "earlier result"),
        )
        .unwrap();
    a.model.context_tokens = 22000;
    assert_eq!(a.turn("new task").await.unwrap(), "complete");
    assert_eq!(a.store.ledger().unwrap().len(), 2);
    let context = a.store.context(&a.session).unwrap();
    assert!(
        context
            .iter()
            .any(|m| m.text.contains("Summary of earlier conversation"))
    );
    assert!(
        context
            .iter()
            .any(|m| m.role == Role::System && m.text.contains("Keep my preferences"))
    );
    assert!(context.iter().any(|m| m.text == "new task"));
}

#[tokio::test]
async fn oversized_context_declined_never_calls_provider() {
    let d = tempfile::tempdir().unwrap();
    let mut a = agent(d.path(), Some(false), vec![]);
    a.initialize(None).await.unwrap();
    a.model.context_tokens = 6000;
    assert!(
        a.turn(&"large context ".repeat(1000))
            .await
            .unwrap_err()
            .to_string()
            .contains("context exceeds")
    );
    assert!(a.store.ledger().unwrap().is_empty());
}

#[tokio::test]
async fn request_limit_stops_a_runaway_tool_loop() {
    let d = tempfile::tempdir().unwrap();
    let responses = (0..25)
        .map(|i| {
            response(vec![ToolCall {
                id: format!("list-{i}"),
                name: "list_files".into(),
                arguments: json!({}),
            }])
        })
        .collect();
    let mut a = agent(d.path(), Some(true), responses);
    a.model.context_tokens = 1_000_000;
    a.initialize(None).await.unwrap();
    assert!(
        a.turn("inspect")
            .await
            .unwrap_err()
            .to_string()
            .contains("25-request limit")
    );
    assert_eq!(a.store.ledger().unwrap().len(), 25);
}

#[tokio::test]
async fn coding_flow_approval_execution_followup_and_ledger() {
    let d = tempfile::tempdir().unwrap();
    let a = agent(
        d.path(),
        Some(true),
        vec![response(vec![call()]), response(vec![])],
    );
    a.initialize(None).await.unwrap();
    assert_eq!(a.turn("create hello").await.unwrap(), "complete");
    assert_eq!(
        std::fs::read_to_string(d.path().join("hello.txt")).unwrap(),
        "hello\n"
    );
    let context = a.store.context(&a.session).unwrap();
    assert!(
        context
            .iter()
            .any(|m| m.role == Role::Tool && m.text.contains("updated"))
    );
    let ledger = a.store.ledger().unwrap();
    assert_eq!(ledger.len(), 2);
    assert!(
        ledger
            .iter()
            .all(|r| r.state == "complete" && r.cost_nusd == Some(120))
    );
}
#[tokio::test]
async fn denied_change_is_not_applied() {
    let d = tempfile::tempdir().unwrap();
    let a = agent(
        d.path(),
        Some(false),
        vec![response(vec![call()]), response(vec![])],
    );
    a.initialize(None).await.unwrap();
    a.turn("create hello").await.unwrap();
    assert!(!d.path().join("hello.txt").exists());
    assert!(
        a.store
            .context(&a.session)
            .unwrap()
            .iter()
            .any(|m| m.text.contains("User denied"))
    );
}
#[tokio::test]
async fn unattended_stop_then_resume_never_replays_call() {
    let d = tempfile::tempdir().unwrap();
    let mut a = agent(d.path(), None, vec![response(vec![call()])]);
    a.initialize(None).await.unwrap();
    assert_eq!(a.turn("create hello").await.unwrap(), "approval_required");
    assert!(!d.path().join("hello.txt").exists());
    a.provider = Arc::new(Mock {
        responses: Mutex::new(vec![response(vec![])].into()),
    });
    a.frontend = Arc::new(Display {
        decision: Some(true),
        events: Mutex::new(vec![]),
    });
    assert_eq!(a.turn("report current status").await.unwrap(), "complete");
    assert!(!d.path().join("hello.txt").exists());
    assert!(
        a.store
            .context(&a.session)
            .unwrap()
            .iter()
            .any(|m| m.role == Role::Tool && m.text.contains("status is unknown"))
    );
}
struct Interrupted;
#[async_trait]
impl Provider for Interrupted {
    async fn complete(
        &self,
        _m: &[Message],
        _t: bool,
        _s: mpsc::Sender<String>,
        _c: &CancellationToken,
    ) -> Result<Completion> {
        anyhow::bail!("stream interrupted")
    }
}
#[tokio::test]
async fn interrupted_stream_keeps_reservation() {
    let d = tempfile::tempdir().unwrap();
    let mut a = agent(d.path(), Some(true), vec![]);
    a.provider = Arc::new(Interrupted);
    a.initialize(None).await.unwrap();
    assert!(a.turn("hello").await.is_err());
    let rows = a.store.ledger().unwrap();
    assert_eq!(rows[0].state, "uncertain");
    assert!(rows[0].reserved_nusd.unwrap() > 0);
}
#[tokio::test]
async fn missing_usage_is_unknown_not_free() {
    let d = tempfile::tempdir().unwrap();
    let a = agent(
        d.path(),
        Some(true),
        vec![Completion {
            text: "done".into(),
            ..Default::default()
        }],
    );
    a.initialize(None).await.unwrap();
    a.turn("hello").await.unwrap();
    let rows = a.store.ledger().unwrap();
    assert_eq!(rows[0].state, "uncertain");
    assert!(rows[0].cost_nusd.is_none());
    assert!(rows[0].reserved_nusd.is_some());
}
