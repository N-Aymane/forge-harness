//! Provider-independent conversation and event types.
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A conversation entry. Tool calls and their results retain provider identifiers.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub text: String,
    #[serde(default)]
    pub calls: Vec<ToolCall>,
    #[serde(default)]
    pub call_id: Option<String>,
}

impl Message {
    pub fn text(role: Role, text: impl Into<String>) -> Self {
        Self {
            role,
            text: text.into(),
            calls: Vec::new(),
            call_id: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Usage totals are cumulative within a response, never stream deltas.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cached_input: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    pub raw: Value,
}

#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub usage: Option<Usage>,
}

/// Public JSONL envelope. Every line is independently parseable and versioned.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    pub version: u8,
    pub session: String,
    #[serde(flatten)]
    pub kind: EventKind,
}

impl Event {
    pub fn new(session: &str, kind: EventKind) -> Self {
        Self {
            version: 1,
            session: session.into(),
            kind,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventKind {
    Started,
    Text {
        text: String,
    },
    Activity {
        text: String,
    },
    ApprovalRequired {
        id: String,
        title: String,
        preview: String,
    },
    Usage {
        request: String,
        reported: Option<Usage>,
        reserved_nusd: Option<u64>,
        cost_nusd: Option<u64>,
        warning: bool,
    },
    Compacted,
    Finished {
        status: String,
    },
    Error {
        message: String,
    },
}
