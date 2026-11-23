//! An observation-only CDP connection. It never evaluates scripts, intercepts
//! requests or changes the control connection's lifetime.
#![forbid(unsafe_code)]

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    time::Duration,
};
use tokio::{sync::watch, task::JoinHandle};
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{protocol::WebSocketConfig, Message},
};

const MAX_EVENTS: usize = 256;
const MAX_PENDING: usize = 128;
const MAX_BATCH: usize = 20;
const MAX_BATCH_BYTES: usize = 6000;
const MAX_WAIT_MS: u64 = 3000;
const CAPTURE_LIFETIME: Duration = Duration::from_secs(600);
const FRAME_BYTES: usize = 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Request {
    Start {},
    Read {
        capture_id: String,
        #[serde(default)]
        after: u64,
        kind: Option<Kind>,
        limit: Option<usize>,
        #[serde(default)]
        wait_ms: u64,
    },
    Stop {
        capture_id: String,
    },
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Kind {
    Console,
    Network,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Console => "console",
            Self::Network => "network",
        }
    }
}

pub fn schema() -> Value {
    let capture_id = json!({"type":"string","minLength":32,"maxLength":32,"description":"Use the capture_id returned by start."});
    json!({"oneOf":[
        {"type":"object","additionalProperties":false,"properties":{"action":{"const":"start"}},"required":["action"]},
        {"type":"object","additionalProperties":false,"properties":{"action":{"const":"read"},"capture_id":capture_id,"after":{"type":"integer","minimum":0},"kind":{"type":"string","enum":["console","network"]},"limit":{"type":"integer","minimum":1,"maximum":MAX_BATCH},"wait_ms":{"type":"integer","minimum":0,"maximum":MAX_WAIT_MS}},"required":["action","capture_id"]},
        {"type":"object","additionalProperties":false,"properties":{"action":{"const":"stop"},"capture_id":capture_id},"required":["action","capture_id"]}
    ]})
}

struct Journal {
    state: &'static str,
    latest: u64,
    incomplete: bool,
    events: VecDeque<Value>,
}

impl Default for Journal {
    fn default() -> Self {
        Self {
            state: "recording",
            latest: 0,
            incomplete: false,
            events: VecDeque::new(),
        }
    }
}

impl Journal {
    fn push(&mut self, mut event: Value) -> u64 {
        self.latest += 1;
        event["seq"] = json!(self.latest);
        if self.events.len() == MAX_EVENTS {
            self.events.pop_front();
        }
        self.events.push_back(event);
        self.latest
    }

    fn page(&self, after: u64, kind: Option<Kind>, limit: usize) -> Result<Value, String> {
        if self.state == "stopped" {
            return Err("browser debug capture stopped".into());
        }
        if after > self.latest {
            return Err("browser debug cursor is ahead of the capture".into());
        }
        let mut next = after;
        let mut events = Vec::new();
        let mut bytes = 0;
        for event in &self.events {
            let seq = event["seq"]
                .as_u64()
                .ok_or("invalid browser debug sequence")?;
            if seq <= after {
                continue;
            }
            if kind.is_none_or(|kind| event["kind"] == kind.name()) {
                let size = event.to_string().len() + 1;
                if events.len() == limit || bytes + size > MAX_BATCH_BYTES {
                    break;
                }
                events.push(event.clone());
                bytes += size;
            }
            next = seq;
        }
        let overwritten = self
            .events
            .front()
            .and_then(|e| e["seq"].as_u64())
            .is_some_and(|first| first > after.saturating_add(1));
        Ok(
            json!({"ok":true,"state":self.state,"events":events,"next_cursor":next,
            "gap":overwritten || self.incomplete,"has_more":next < self.latest,"untrusted":true}),
        )
    }
}

pub(super) struct Capture {
    pub id: String,
    journal: watch::Sender<Journal>,
    worker: JoinHandle<()>,
}

impl Capture {
    pub async fn start(endpoint: &str) -> Result<Self, String> {
        let id = super::nonce()?;
        let (journal, _) = watch::channel(Journal::default());
        let (socket, projection) = tokio::time::timeout(Duration::from_secs(6), async {
            let config = WebSocketConfig::default()
                .max_frame_size(Some(FRAME_BYTES))
                .max_message_size(Some(FRAME_BYTES));
            let (mut socket, _) = connect_async_with_config(endpoint, Some(config), false)
                .await
                .map_err(|_| "browser debug connection unavailable")?;
            let mut projection = Projection::default();
            // Every notification received during enable must be routed too.
            // Only these two enable commands are sent on the debug connection.
            for (id, method) in [(1, "Runtime.enable"), (2, "Network.enable")] {
                socket
                    .send(Message::Text(
                        json!({"id":id,"method":method}).to_string().into(),
                    ))
                    .await
                    .map_err(|_| "browser debug activation failed")?;
                loop {
                    let Some(Ok(packet)) = socket.next().await else {
                        return Err("browser debug connection closed");
                    };
                    if let Message::Text(text) = packet {
                        let event: Value = serde_json::from_str(&text)
                            .map_err(|_| "invalid browser debug frame")?;
                        if event["id"] == id {
                            if !event["result"].is_object() {
                                return Err("browser rejected debug subscription");
                            }
                            break;
                        }
                        record(&journal, &mut projection, &event);
                    } else if packet.is_close() {
                        return Err("browser debug connection closed");
                    } else if packet.is_ping() {
                        // tungstenite queues the Pong while reading.
                        socket
                            .flush()
                            .await
                            .map_err(|_| "browser debug connection closed")?;
                    }
                }
            }
            Ok((socket, projection))
        })
        .await
        .map_err(|_| "browser debug activation timed out")??;

        let mut socket = socket;
        let mut projection = projection;
        let end = CaptureEnd(journal.clone());
        let worker = tokio::spawn(async move {
            let _end = end;
            let expiry = tokio::time::sleep(CAPTURE_LIFETIME);
            tokio::pin!(expiry);
            loop {
                let packet = tokio::select! {
                    biased;
                    _ = &mut expiry => {
                        _end.0.send_modify(|journal| {
                            if journal.state == "recording" {
                                journal.state = "expired";
                                journal.incomplete |= !projection.pending.is_empty();
                            }
                        });
                        break;
                    },
                    next = socket.next() => next,
                };
                let Some(Ok(packet)) = packet else {
                    break;
                };
                if let Message::Text(text) = packet {
                    let Ok(value) = serde_json::from_str(&text) else {
                        break;
                    };
                    record(&_end.0, &mut projection, &value);
                } else if packet.is_close()
                    || (packet.is_ping()
                        && !matches!(
                            tokio::time::timeout(Duration::from_secs(1), socket.flush()).await,
                            Ok(Ok(()))
                        ))
                {
                    break;
                }
            }
        });
        Ok(Self {
            id,
            journal,
            worker,
        })
    }

    pub fn active(&self) -> bool {
        self.journal.borrow().state == "recording"
    }

    pub fn reader(&self, capture_id: &str) -> Result<Reader, String> {
        if self.id != capture_id {
            return Err("browser debug capture is stale; use the current capture_id".into());
        }
        Ok(Reader {
            journal: self.journal.subscribe(),
            capture_id: self.id.clone(),
        })
    }
}

fn record(journal: &watch::Sender<Journal>, projection: &mut Projection, event: &Value) {
    journal.send_if_modified(|journal| {
        let prior = (journal.latest, journal.incomplete);
        projection.handle(event, journal);
        prior != (journal.latest, journal.incomplete)
    });
}

struct CaptureEnd(watch::Sender<Journal>);
impl Drop for CaptureEnd {
    fn drop(&mut self) {
        self.0.send_modify(|journal| {
            if journal.state == "recording" {
                journal.state = "disconnected";
                journal.incomplete = true;
            }
        });
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.journal.send_modify(|journal| {
            journal.state = "stopped";
            journal.events.clear();
        });
        self.worker.abort();
    }
}

pub(super) struct Reader {
    journal: watch::Receiver<Journal>,
    capture_id: String,
}

impl Reader {
    pub async fn read(
        mut self,
        mut after: u64,
        kind: Option<Kind>,
        limit: Option<usize>,
        wait_ms: u64,
    ) -> Result<Value, String> {
        let limit = limit.unwrap_or(MAX_BATCH);
        if !(1..=MAX_BATCH).contains(&limit) || wait_ms > MAX_WAIT_MS {
            return Err("browser debug read exceeds its limit or wait budget".into());
        }
        let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let mut page = self.journal.borrow_and_update().page(after, kind, limit)?;
            // A filtered read advances over non-matching events. Preserve an
            // overflow signal instead of waiting past and losing it.
            if page["gap"] == true
                || page["state"] != "recording"
                || page["events"]
                    .as_array()
                    .is_some_and(|events| !events.is_empty())
                || tokio::time::Instant::now() >= deadline
            {
                page["capture_id"] = json!(self.capture_id);
                return Ok(page);
            }
            after = page["next_cursor"].as_u64().ok_or("invalid debug cursor")?;
            if let Ok(Err(_)) = tokio::time::timeout_at(deadline, self.journal.changed()).await {
                return Err("browser debug capture stopped".into());
            }
        }
    }
}

#[derive(Default)]
struct Projection {
    pending: HashMap<String, RequestSummary>,
}

struct RequestSummary {
    request: u64,
    url: String,
    method: &'static str,
    started: Option<f64>,
    status: Option<u64>,
}

impl Projection {
    fn handle(&mut self, event: &Value, journal: &mut Journal) {
        let data = &event["params"];
        match event["method"].as_str() {
            Some("Runtime.consoleAPICalled") => {
                let args = data["args"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(6)
                    .map(console_arg)
                    .collect::<Vec<_>>()
                    .join(" ");
                let level = match data["type"].as_str() {
                    Some("error" | "assert") => "error",
                    Some("warning") => "warn",
                    Some("debug") => "debug",
                    Some("info") => "info",
                    _ => "log",
                };
                journal.push(json!({"kind":"console","level":level,"text":bounded(&args, 600)}));
            }
            Some("Runtime.exceptionThrown") => {
                let details = &data["exceptionDetails"];
                let text = details["exception"]["description"]
                    .as_str()
                    .or(details["text"].as_str())
                    .unwrap_or("Uncaught exception")
                    .lines()
                    .next()
                    .unwrap_or("Uncaught exception");
                journal.push(json!({"kind":"console","level":"error","text":bounded(text, 600),"uncaught":true}));
            }
            Some("Network.requestWillBeSent") => self.begin(data, journal),
            Some("Network.responseReceived") => {
                if let Some(request) = data["requestId"]
                    .as_str()
                    .and_then(|id| self.pending.get_mut(id))
                {
                    request.status = http_status(&data["response"]["status"]);
                }
            }
            Some("Network.loadingFinished" | "Network.loadingFailed") => {
                if let Some(request) = data["requestId"]
                    .as_str()
                    .and_then(|id| self.pending.remove(id))
                {
                    let failed = event["method"] == "Network.loadingFailed";
                    // CDP may embed a URL or page text in other error fields.
                    let reason = if !failed {
                        None
                    } else if data["canceled"] == true {
                        Some("cancelled")
                    } else if !data["blockedReason"].is_null() {
                        Some("blocked")
                    } else if !data["corsErrorStatus"].is_null() {
                        Some("cors")
                    } else {
                        Some("transport")
                    };
                    request.complete(
                        journal,
                        if failed { "failed" } else { "finished" },
                        data,
                        reason,
                    );
                }
            }
            _ => {} // Raw headers, bodies, chunks and unrelated domains never enter the journal.
        }
    }

    fn begin(&mut self, data: &Value, journal: &mut Journal) {
        let Some(id) = data["requestId"].as_str().filter(|id| id.len() <= 128) else {
            return;
        };
        if let Some(mut prior) = self.pending.remove(id) {
            prior.status = http_status(&data["redirectResponse"]["status"]);
            let phase = if prior.status.is_some() {
                "redirect"
            } else {
                "unknown"
            };
            journal.incomplete |= phase == "unknown";
            prior.complete(journal, phase, data, None);
        }
        let Some(url) = data["request"]["url"].as_str().and_then(safe_url) else {
            return;
        };
        if self.pending.len() == MAX_PENDING {
            // Keep the bounded active set; any omitted request is explicit.
            journal.incomplete = true;
            return;
        }
        let method = match data["request"]["method"].as_str() {
            Some("GET") => "GET",
            Some("POST") => "POST",
            Some("PUT") => "PUT",
            Some("DELETE") => "DELETE",
            Some("PATCH") => "PATCH",
            Some("HEAD") => "HEAD",
            Some("OPTIONS") => "OPTIONS",
            _ => "OTHER",
        };
        let seq =
            journal.push(json!({"kind":"network","phase":"started","method":method,"url":url}));
        self.pending.insert(
            id.to_owned(),
            RequestSummary {
                request: seq,
                url,
                method,
                started: data["timestamp"].as_f64(),
                status: None,
            },
        );
    }
}

impl RequestSummary {
    fn complete(self, journal: &mut Journal, phase: &str, data: &Value, reason: Option<&str>) {
        let duration = self
            .started
            .zip(data["timestamp"].as_f64())
            .map(|(start, end)| (end - start) * 1000.0)
            .filter(|ms| ms.is_finite() && *ms >= 0.0);
        journal.push(json!({"kind":"network","phase":phase,"request_seq":self.request,
            "method":self.method,"url":self.url,"status":self.status,"duration_ms":duration,"reason":reason}));
    }
}

fn http_status(v: &Value) -> Option<u64> {
    v.as_u64()
        .or_else(|| v.as_f64().filter(|n| n.fract() == 0.0).map(|n| n as u64))
        .filter(|n| (100..=599).contains(n))
}

fn safe_url(raw: &str) -> Option<String> {
    if raw.len() > 8192 {
        return None;
    }
    let mut url = url::Url::parse(raw).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    url.set_username("").ok()?;
    url.set_password(None).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    Some(bounded(url.as_str(), 480))
}

fn console_arg(arg: &Value) -> String {
    let value = &arg["value"];
    match (arg["type"].as_str(), value) {
        (Some("string"), Value::String(s)) => bounded(s, 256),
        (_, Value::Bool(_) | Value::Number(_)) => value.to_string(),
        (Some("undefined"), _) => "undefined".into(),
        (_, _) if arg["subtype"] == "null" => "null".into(),
        // Never expand objects, access getters, or retain CDP handles.
        _ => "[object]".into(),
    }
}

fn bounded(s: &str, bytes: usize) -> String {
    let mut text = String::new();
    for c in s.chars() {
        if text.len() + c.len_utf8() > bytes {
            break;
        }
        text.push(if c.is_control() { ' ' } else { c });
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(method: &str, params: Value) -> Value {
        json!({"method":method,"params":params})
    }

    #[test]
    fn arguments_do_not_accept_targets_or_stale_shapes() {
        for args in [
            json!({"action":"start","endpoint":"ws://fixture"}),
            json!({"action":"read","capture_id":"fixture","owner":"other"}),
            json!({"action":"stop"}),
            json!({"action":"read","capture_id":"fixture","after":-1}),
        ] {
            assert!(serde_json::from_value::<Request>(args).is_err());
        }
    }

    #[test]
    fn cursors_are_non_destructive_and_overflow_is_explicit() {
        let mut journal = Journal::default();
        for index in 0..MAX_EVENTS + 3 {
            let kind = if index % 2 == 0 { "console" } else { "network" };
            journal.push(json!({"kind":kind}));
        }
        let first = journal.page(0, Some(Kind::Console), 1).unwrap();
        assert_eq!(first["gap"], true);
        assert_eq!(first["events"][0]["seq"], 5);
        assert_eq!(first["has_more"], true);
        assert_eq!(journal.page(0, Some(Kind::Console), 1).unwrap(), first);
        let next = first["next_cursor"].as_u64().unwrap();
        let second = journal.page(next, Some(Kind::Console), 1).unwrap();
        assert_eq!(second["gap"], false);
        assert_eq!(second["events"][0]["seq"], 7);
        assert!(journal.page(journal.latest + 1, None, 1).is_err());
    }

    #[test]
    fn large_text_pages_respect_wire_budget_and_make_progress() {
        let mut j = Journal::default();
        for _ in 0..MAX_BATCH {
            j.push(json!({"kind":"console","text":"\\".repeat(600)}));
        }
        let first = j.page(0, None, MAX_BATCH).unwrap();
        assert!(first.to_string().len() < 6500);
        assert_eq!(first["has_more"], true);
        let next = first["next_cursor"].as_u64().unwrap();
        assert!(next > 0);
        assert_eq!(
            j.page(next, None, MAX_BATCH).unwrap()["events"][0]["seq"],
            next + 1
        );
    }

    #[test]
    fn requests_correlate_redirect_completion_and_transport_failure_without_secrets() {
        let mut p = Projection::default();
        let mut j = Journal::default();
        let begin = |url: &str, redirect, timestamp| {
            event(
                "Network.requestWillBeSent",
                json!({
                    "requestId":"1","request":{"url":url,"method":"GET","headers":{"Authorization":"private-auth"},"postData":"private-body"},
                    "redirectResponse":redirect,"timestamp":timestamp,
                }),
            )
        };
        p.handle(
            &begin(
                "https://user:pwd@example.test/one?q=private-query#private-fragment",
                Value::Null,
                1.0,
            ),
            &mut j,
        );
        p.handle(
            &begin("https://example.test/two", json!({"status":307}), 1.25),
            &mut j,
        );
        p.handle(&event("Network.responseReceived", json!({"requestId":"1","response":{"status":500,"headers":{"Set-Cookie":"private-cookie"}}})), &mut j);
        p.handle(
            &event(
                "Network.loadingFinished",
                json!({"requestId":"1","timestamp":1.5}),
            ),
            &mut j,
        );
        p.handle(
            &begin("https://example.test/three", Value::Null, 2.0),
            &mut j,
        );
        p.handle(&event("Network.loadingFailed", json!({"requestId":"1","timestamp":2.5,"canceled":false,"errorText":"private-error"})), &mut j);
        let page = j.page(0, None, MAX_BATCH).unwrap();
        let events = page["events"].as_array().unwrap();
        assert_eq!(events[0]["url"], "https://example.test/one");
        assert_eq!(events[1]["phase"], "redirect");
        assert_eq!(events[1]["request_seq"], events[0]["seq"]);
        assert_eq!(events[1]["duration_ms"], 250.0);
        assert_eq!(events[3]["phase"], "finished");
        assert_eq!(events[3]["status"], 500);
        assert_eq!(events[3]["request_seq"], events[2]["seq"]);
        assert_eq!(events[5]["phase"], "failed");
        assert_eq!(events[5]["reason"], "transport");
        assert!(!page.to_string().contains("private"));
    }

    #[test]
    fn pending_request_overflow_and_console_string_budget_are_bounded() {
        let mut p = Projection::default();
        let mut j = Journal::default();
        for id in 0..=MAX_PENDING {
            p.handle(&event("Network.requestWillBeSent", json!({
                "requestId":id.to_string(),"request":{"url":"http://fixture.test","method":"GET"}
            })), &mut j);
        }
        assert_eq!(p.pending.len(), MAX_PENDING);
        assert_eq!(j.page(0, None, 1).unwrap()["gap"], true);
        p.handle(
            &event(
                "Runtime.consoleAPICalled",
                json!({"type":"error","args":[
                    {"type":"object","objectId":"never-visible","description":"not expanded"},
                    {"type":"string","value":"中".repeat(300)}
                ]}),
            ),
            &mut j,
        );
        let log = j.events.back().unwrap();
        assert!(!log.to_string().contains("never-visible"));
        assert!(log["text"].as_str().unwrap().len() <= 600);
    }

    #[tokio::test]
    async fn capture_keeps_events_during_enable_and_reports_disconnect() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("ws://{}", listener.local_addr().unwrap());
        let (disconnect, signal) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_async(stream).await.unwrap();
            for (id, method) in [(1, "Runtime.enable"), (2, "Network.enable")] {
                let packet = socket.next().await.unwrap().unwrap();
                let packet: Value = serde_json::from_str(packet.to_text().unwrap()).unwrap();
                assert_eq!(packet, json!({"id":id,"method":method}));
                if id == 1 {
                    let log = event(
                        "Runtime.consoleAPICalled",
                        json!({"type":"info","args":[{"type":"string","value":"while enabling"}]}),
                    );
                    socket
                        .send(Message::Text(log.to_string().into()))
                        .await
                        .unwrap();
                }
                socket
                    .send(Message::Text(
                        json!({"id":id,"result":{}}).to_string().into(),
                    ))
                    .await
                    .unwrap();
            }
            signal.await.unwrap();
            socket.close(None).await.unwrap();
        });
        let capture = Capture::start(&endpoint).await.unwrap();
        assert!(capture.reader("another-capture").is_err());
        let page = capture
            .reader(&capture.id)
            .unwrap()
            .read(0, None, None, 0)
            .await
            .unwrap();
        assert_eq!(page["events"][0]["text"], "while enabling");
        assert_eq!(page["next_cursor"], 1);
        disconnect.send(()).unwrap();
        let page = capture
            .reader(&capture.id)
            .unwrap()
            .read(1, None, None, 1000)
            .await
            .unwrap();
        assert_eq!(page["state"], "disconnected");
        assert_eq!(page["gap"], true);
        assert!(page["events"].as_array().unwrap().is_empty());
        let reader = capture.reader(&capture.id).unwrap();
        drop(capture);
        assert!(reader.read(0, None, None, 0).await.is_err());
        server.await.unwrap();
    }
}
