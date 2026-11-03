//! Streaming HTTP adapters with normalized usage and tool calls.
use crate::{
    config::{ModelConfig, ProviderKind},
    credentials::Secret,
    model::{Completion, Message, Role, ToolCall, Usage},
    tools,
};
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(
        &self,
        messages: &[Message],
        tools_enabled: bool,
        text: mpsc::Sender<String>,
        cancel: &CancellationToken,
    ) -> Result<Completion>;
    fn redact(&self, text: &str) -> String {
        text.into()
    }
}

pub struct HttpProvider {
    client: reqwest::Client,
    model: ModelConfig,
    key: Secret,
}
impl HttpProvider {
    pub fn new(model: ModelConfig, key: Secret) -> Result<Self> {
        model.validate()?;
        Ok(Self {
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(20))
                .timeout(Duration::from_secs(600))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            model,
            key,
        })
    }
    fn request(&self, messages: &[Message], enabled: bool) -> Result<reqwest::RequestBuilder> {
        let definitions = if enabled {
            tools::definitions()
        } else {
            Vec::new()
        };
        let base = self.model.base_url.trim_end_matches('/');
        let (endpoint, mut body) = match self.model.provider {
            ProviderKind::Openai => {
                let mut input = Vec::new();
                for m in messages {
                    if m.role == Role::Tool {
                        input.push(json!({"type":"function_call_output","call_id":m.call_id,"output":m.text}));
                        continue;
                    }
                    if !m.text.is_empty() {
                        let role = match m.role {
                            Role::System => "developer",
                            Role::User => "user",
                            _ => "assistant",
                        };
                        input.push(json!({"role":role,"content":m.text}));
                    }
                    for c in &m.calls {
                        input.push(json!({"type":"function_call","call_id":c.id,"name":c.name,"arguments":serde_json::to_string(&c.arguments)?}));
                    }
                }
                let ts:Vec<Value>=definitions.iter().map(|d|json!({"type":"function","name":d["name"],"description":d["description"],"parameters":d["parameters"],"strict":false})).collect();
                (
                    "responses",
                    json!({"model":self.model.model,"input":input,"tools":ts,"stream":true,"store":false,"max_output_tokens":self.model.max_output_tokens}),
                )
            }
            ProviderKind::Compatible => {
                let mut list = Vec::new();
                for m in messages {
                    let mut v = json!({"role":match m.role{Role::System=>"system",Role::User=>"user",Role::Assistant=>"assistant",Role::Tool=>"tool"},"content":m.text});
                    if m.role == Role::Tool {
                        v["tool_call_id"] = json!(m.call_id);
                    }
                    if !m.calls.is_empty() {
                        v["tool_calls"]=json!(m.calls.iter().map(|c|json!({"id":c.id,"type":"function","function":{"name":c.name,"arguments":c.arguments.to_string()}})).collect::<Vec<_>>());
                    }
                    list.push(v);
                }
                let ts: Vec<Value> = definitions
                    .iter()
                    .map(|d| json!({"type":"function","function":d}))
                    .collect();
                (
                    "chat/completions",
                    json!({"model":self.model.model,"messages":list,"tools":ts,"stream":true,"stream_options":{"include_usage":true},"max_tokens":self.model.max_output_tokens}),
                )
            }
            ProviderKind::Anthropic => {
                let system = messages
                    .iter()
                    .filter(|m| m.role == Role::System)
                    .map(|m| m.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                let mut list: Vec<Value> = Vec::new();
                for m in messages.iter().filter(|m| m.role != Role::System) {
                    let role = if m.role == Role::Assistant {
                        "assistant"
                    } else {
                        "user"
                    };
                    let mut content = Vec::new();
                    if m.role == Role::Tool {
                        content.push(
                            json!({"type":"tool_result","tool_use_id":m.call_id,"content":m.text}),
                        );
                    } else if !m.text.is_empty() {
                        content.push(json!({"type":"text","text":m.text}));
                    }
                    for c in &m.calls {
                        content.push(
                            json!({"type":"tool_use","id":c.id,"name":c.name,"input":c.arguments}),
                        );
                    }
                    if let Some(last) = list.last_mut().filter(|v| v["role"] == role) {
                        last["content"]
                            .as_array_mut()
                            .context("invalid message content")?
                            .extend(content);
                    } else {
                        list.push(json!({"role":role,"content":content}));
                    }
                }
                let ts:Vec<Value>=definitions.iter().map(|d|json!({"name":d["name"],"description":d["description"],"input_schema":d["parameters"]})).collect();
                (
                    "messages",
                    json!({"model":self.model.model,"system":system,"messages":list,"tools":ts,"stream":true,"max_tokens":self.model.max_output_tokens}),
                )
            }
        };
        if !enabled {
            body.as_object_mut()
                .context("invalid request body")?
                .remove("tools");
        }
        let req = self.client.post(format!("{base}/{endpoint}")).json(&body);
        Ok(if self.model.provider == ProviderKind::Anthropic {
            req.header("x-api-key", self.key.expose())
                .header("anthropic-version", "2023-06-01")
        } else {
            req.bearer_auth(self.key.expose())
        })
    }
}

#[async_trait]
impl Provider for HttpProvider {
    fn redact(&self, text: &str) -> String {
        self.key.redact(text)
    }
    async fn complete(
        &self,
        messages: &[Message],
        enabled: bool,
        text: mpsc::Sender<String>,
        cancel: &CancellationToken,
    ) -> Result<Completion> {
        let request = self.request(messages, enabled)?;
        let response = tokio::select! {r=request.send()=>r.context("provider request failed (no automatic retry; inspect usage before retrying)")?,_=cancel.cancelled()=>bail!("cancelled")};
        if !response.status().is_success() {
            bail!(
                "provider returned HTTP {}; response body omitted to protect secrets",
                response.status().as_u16()
            );
        }
        let mut bytes = response.bytes_stream();
        let mut sse = Sse::default();
        let mut parser = StreamParser::new(self.model.provider);
        let mut redactor = StreamingRedactor::new(self.key.expose());
        loop {
            let chunk =
                tokio::select! {chunk=bytes.next()=>chunk,_=cancel.cancelled()=>bail!("cancelled")};
            let Some(chunk) = chunk else {
                break;
            };
            for frame in sse.push(
                &chunk.context("provider stream disconnected; request may have been billed")?,
            )? {
                let delta = parser.frame(&frame)?;
                let safe = redactor.push(&delta, false);
                if !safe.is_empty() {
                    tokio::select! {r=text.send(safe)=>{r.context("frontend closed")?;},_=cancel.cancelled()=>bail!("cancelled")};
                }
            }
            if parser.done {
                break;
            }
        }
        let tail = redactor.push("", true);
        if !tail.is_empty() {
            let _ = text.send(tail).await;
        }
        if !parser.done {
            bail!("provider stream ended without a completion marker; usage is uncertain");
        }
        let mut result = parser.finish()?;
        result.text = self.redact(&result.text);
        for call in &mut result.calls {
            call.id = self.redact(&call.id);
            call.name = self.redact(&call.name);
            call.arguments = serde_json::from_str(&self.redact(&call.arguments.to_string()))?;
        }
        if let Some(usage) = result.usage.as_mut() {
            usage.raw = serde_json::from_str(&self.redact(&usage.raw.to_string()))?;
        }
        Ok(result)
    }
}

/// Incremental byte framing keeps split UTF-8 sequences intact.
#[derive(Default)]
struct Sse {
    pending: Vec<u8>,
    data: Vec<String>,
}
impl Sse {
    fn push(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        self.pending.extend_from_slice(bytes);
        if self.pending.len() > 2 * 1024 * 1024 {
            bail!("provider SSE frame exceeds 2 MiB");
        }
        let mut frames = Vec::new();
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            let line = std::str::from_utf8(&line)?.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    frames.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data
                    .push(data.strip_prefix(' ').unwrap_or(data).into());
            }
            if self.data.iter().map(String::len).sum::<usize>() > 2 * 1024 * 1024 {
                bail!("provider SSE frame exceeds 2 MiB");
            }
        }
        Ok(frames)
    }
}

#[derive(Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}
struct StreamParser {
    kind: ProviderKind,
    result: Completion,
    calls: BTreeMap<String, PartialCall>,
    done: bool,
    finish_reason: Option<String>,
}
impl StreamParser {
    fn new(kind: ProviderKind) -> Self {
        Self {
            kind,
            result: Completion::default(),
            calls: BTreeMap::new(),
            done: false,
            finish_reason: None,
        }
    }
    fn frame(&mut self, data: &str) -> Result<String> {
        if data == "[DONE]" {
            if self.kind != ProviderKind::Compatible || self.finish_reason.is_none() {
                bail!("unexpected stream termination marker");
            }
            self.done = true;
            return Ok(String::new());
        }
        let v: Value = serde_json::from_str(data).context("invalid provider event JSON")?;
        if v.get("error").is_some() || v["type"] == "error" {
            bail!("provider reported a streaming error; details omitted");
        }
        let mut text = String::new();
        match self.kind {
            ProviderKind::Openai => match v["type"].as_str().unwrap_or("") {
                "response.output_text.delta" => text = string(&v["delta"]),
                "response.output_item.added" | "response.output_item.done" => {
                    let item = &v["item"];
                    if item["type"] == "function_call" {
                        let call = self.calls.entry(string(&item["id"])).or_default();
                        call.id = string(&item["call_id"]);
                        call.name = string(&item["name"]);
                        if let Some(a) = item["arguments"].as_str().filter(|a| !a.is_empty()) {
                            call.arguments = a.into();
                        }
                    }
                }
                "response.function_call_arguments.delta" => self
                    .calls
                    .entry(string(&v["item_id"]))
                    .or_default()
                    .arguments
                    .push_str(v["delta"].as_str().unwrap_or("")),
                "response.function_call_arguments.done" => {
                    self.calls
                        .entry(string(&v["item_id"]))
                        .or_default()
                        .arguments = string(&v["arguments"])
                }
                "response.completed" => {
                    self.result.usage = parse_usage(&v["response"]["usage"], self.kind);
                    self.done = true;
                }
                "response.failed" | "response.incomplete" => {
                    bail!("provider response incomplete or failed; request may have been billed")
                }
                _ => {}
            },
            ProviderKind::Compatible => {
                if let Some(u) = parse_usage(&v["usage"], self.kind) {
                    self.result.usage = Some(u);
                }
                if let Some(choice) = v["choices"].as_array().and_then(|a| a.first()) {
                    if let Some(reason) = choice["finish_reason"].as_str() {
                        if !matches!(reason, "stop" | "tool_calls") {
                            bail!("provider stopped with {reason}; response is incomplete");
                        }
                        self.finish_reason = Some(reason.into());
                    }
                    let delta = &choice["delta"];
                    text = string(&delta["content"]);
                    if let Some(calls) = delta["tool_calls"].as_array() {
                        for c in calls {
                            let call = self.calls.entry(c["index"].to_string()).or_default();
                            if let Some(id) = c["id"].as_str() {
                                call.id.push_str(id);
                            }
                            if let Some(name) = c["function"]["name"].as_str() {
                                call.name.push_str(name);
                            }
                            if let Some(args) = c["function"]["arguments"].as_str() {
                                call.arguments.push_str(args);
                            }
                        }
                    }
                }
            }
            ProviderKind::Anthropic => match v["type"].as_str().unwrap_or("") {
                "message_start" => {
                    self.result.usage = parse_usage(&v["message"]["usage"], self.kind)
                }
                "content_block_start" => {
                    let b = &v["content_block"];
                    if b["type"] == "tool_use" {
                        self.calls.insert(
                            v["index"].to_string(),
                            PartialCall {
                                id: string(&b["id"]),
                                name: string(&b["name"]),
                                arguments: if b["input"].as_object().is_some_and(|a| !a.is_empty())
                                {
                                    b["input"].to_string()
                                } else {
                                    String::new()
                                },
                            },
                        );
                    }
                    if b["type"] == "text" {
                        text = string(&b["text"]);
                    }
                }
                "content_block_delta" => {
                    if v["delta"]["type"] == "text_delta" {
                        text = string(&v["delta"]["text"]);
                    }
                    if v["delta"]["type"] == "input_json_delta" {
                        self.calls
                            .entry(v["index"].to_string())
                            .or_default()
                            .arguments
                            .push_str(v["delta"]["partial_json"].as_str().unwrap_or(""));
                    }
                }
                "message_delta" => {
                    if let Some(u) = self.result.usage.as_mut() {
                        if let Some(n) = v["usage"]["output_tokens"].as_u64() {
                            u.output = n;
                        }
                        if let Some(raw) = u.raw.as_object_mut()
                            && let Some(delta) = v["usage"].as_object()
                        {
                            raw.extend(delta.clone());
                        }
                    }
                    if let Some(reason) = v["delta"]["stop_reason"].as_str() {
                        if !matches!(reason, "end_turn" | "tool_use" | "stop_sequence") {
                            bail!("provider stopped with {reason}; response is incomplete");
                        }
                        self.finish_reason = Some(reason.into());
                    }
                }
                "message_stop" => {
                    if self.finish_reason.is_none() {
                        bail!("missing Anthropic stop reason");
                    }
                    self.done = true;
                }
                _ => {}
            },
        }
        self.result.text.push_str(&text);
        if self.result.text.len() > 2 * 1024 * 1024
            || self.calls.len() > 64
            || self.calls.values().any(|c| c.arguments.len() > 512 * 1024)
        {
            bail!("provider response exceeds local size limits");
        }
        Ok(text)
    }
    fn finish(mut self) -> Result<Completion> {
        let mut ids = std::collections::HashSet::new();
        for (_, call) in self.calls {
            if call.id.is_empty() || call.name.is_empty() || !ids.insert(call.id.clone()) {
                bail!("invalid or duplicate tool call identifier");
            }
            let args = if call.arguments.is_empty() {
                json!({})
            } else {
                serde_json::from_str(&call.arguments).context("invalid tool call arguments")?
            };
            self.result.calls.push(ToolCall {
                id: call.id,
                name: call.name,
                arguments: args,
            });
        }
        Ok(self.result)
    }
}
fn string(v: &Value) -> String {
    v.as_str().unwrap_or("").into()
}
fn parse_usage(v: &Value, kind: ProviderKind) -> Option<Usage> {
    if !v.is_object() {
        return None;
    }
    let (input, output) = if kind == ProviderKind::Compatible {
        ("prompt_tokens", "completion_tokens")
    } else {
        ("input_tokens", "output_tokens")
    };
    if v[input].as_u64().is_none() || v[output].as_u64().is_none() {
        return None;
    }
    let n = |name: &str| v[name].as_u64().unwrap_or(0);
    Some(match kind {
        ProviderKind::Openai => Usage {
            input: n("input_tokens"),
            output: n("output_tokens"),
            cached_input: v["input_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
            reasoning: v["output_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0),
            raw: v.clone(),
            ..Default::default()
        },
        ProviderKind::Compatible => Usage {
            input: n("prompt_tokens"),
            output: n("completion_tokens"),
            cached_input: v["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0),
            reasoning: v["completion_tokens_details"]["reasoning_tokens"]
                .as_u64()
                .unwrap_or(0),
            raw: v.clone(),
            ..Default::default()
        },
        ProviderKind::Anthropic => Usage {
            input: n("input_tokens").saturating_add(n("cache_read_input_tokens")),
            output: n("output_tokens"),
            cached_input: n("cache_read_input_tokens"),
            cache_write: n("cache_creation_input_tokens"),
            raw: v.clone(),
            ..Default::default()
        },
    })
}

/// Hold only a possible secret prefix, so secrets split across deltas stay private.
struct StreamingRedactor {
    secret: String,
    pending: String,
}
impl StreamingRedactor {
    fn new(secret: &str) -> Self {
        Self {
            secret: secret.into(),
            pending: String::new(),
        }
    }
    fn push(&mut self, text: &str, flush: bool) -> String {
        self.pending.push_str(text);
        if self.secret.is_empty() {
            return std::mem::take(&mut self.pending);
        }
        self.pending = self.pending.replace(&self.secret, "[REDACTED]");
        let keep = if flush {
            0
        } else {
            self.pending
                .char_indices()
                .map(|(i, _)| &self.pending[i..])
                .filter(|s| self.secret.starts_with(s))
                .map(str::len)
                .max()
                .unwrap_or(0)
        };
        let rest = self.pending.split_off(self.pending.len() - keep);
        std::mem::replace(&mut self.pending, rest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn empty_usage_is_not_zero_usage() {
        for kind in [
            ProviderKind::Openai,
            ProviderKind::Anthropic,
            ProviderKind::Compatible,
        ] {
            assert!(parse_usage(&json!({}), kind).is_none());
        }
    }
    #[test]
    fn fragmented_utf8_and_multiline_sse() {
        let mut s = Sse::default();
        let data = "data: {\"text\":\"é\"}\r\n\r\n".as_bytes();
        let mut frames = Vec::new();
        for b in data {
            frames.extend(s.push(&[*b]).unwrap());
        }
        assert_eq!(frames, vec!["{\"text\":\"é\"}"]);
    }
    #[test]
    fn anthropic_usage_is_cumulative() {
        let mut p = StreamParser::new(ProviderKind::Anthropic);
        for v in [
            json!({"type":"message_start","message":{"usage":{"input_tokens":10,"output_tokens":1,"cache_read_input_tokens":5}}}),
            json!({"type":"message_delta","usage":{"output_tokens":3}}),
            json!({"type":"message_delta","usage":{"output_tokens":7},"delta":{"stop_reason":"end_turn"}}),
            json!({"type":"message_stop"}),
        ] {
            p.frame(&v.to_string()).unwrap();
        }
        let u = p.finish().unwrap().usage.unwrap();
        assert_eq!(u.output, 7);
        assert_eq!(u.input, 15);
        assert_eq!(u.cached_input, 5);
    }
    #[test]
    fn redaction_across_fragments() {
        let mut r = StreamingRedactor::new("sk-secret");
        let mut out = String::new();
        for s in ["value sk-", "sec", "ret end"] {
            out.push_str(&r.push(s, false));
        }
        out.push_str(&r.push("", true));
        assert_eq!(out, "value [REDACTED] end");
    }
    #[test]
    fn fragmented_tool_arguments() {
        let mut p = StreamParser::new(ProviderKind::Compatible);
        for v in [
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}),
            json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a\"}"}}]},"finish_reason":"tool_calls"}]}),
        ] {
            p.frame(&v.to_string()).unwrap();
        }
        p.frame("[DONE]").unwrap();
        assert_eq!(p.finish().unwrap().calls[0].arguments, json!({"path":"a"}));
    }
}
