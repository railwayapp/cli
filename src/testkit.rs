//! Test-only scripted backboard for exercising the CLI's REAL GraphQL layer
//! (`client::post_graphql` and everything built on it) without a network.
//!
//! Same philosophy as `auth_sim`'s `MockEndpoint` (a raw `TcpListener` and a
//! scripted response table -- no mocking framework), but GraphQL-aware:
//! responses are routed by the request body's `operationName`, so one server
//! can back a multi-request flow (fetch config, fire mutation, poll status)
//! and each test reads like the conversation it scripts. Responses for an
//! operation are served in order; the last one repeats once exhausted --
//! which is what polling loops need.
//!
//! Every request body is recorded as parsed JSON so tests can assert what
//! actually went over the wire (operation, variables), not just how the
//! caller reacted.

use std::collections::{HashMap, VecDeque};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use crate::config::Configs;

/// Scripted `(http status, body)` responses for one operation, served in order.
type ResponseQueue = VecDeque<(u16, String)>;
type ResponseTable = Arc<Mutex<HashMap<String, ResponseQueue>>>;

/// A scripted local backboard. See module docs.
pub struct MockBackboard {
    base_url: String,
    requests: Arc<Mutex<Vec<Value>>>,
    responses: ResponseTable,
}

impl MockBackboard {
    pub fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let responses: ResponseTable = Arc::new(Mutex::new(HashMap::new()));

        let requests_for_thread = Arc::clone(&requests);
        let responses_for_thread = Arc::clone(&responses);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };

                let Some(body) = read_http_body(&mut stream) else {
                    continue;
                };
                let parsed: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let operation = parsed
                    .get("operationName")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                requests_for_thread.lock().unwrap().push(parsed);

                let (status, response_body) = {
                    let mut table = responses_for_thread.lock().unwrap();
                    match table.get_mut(&operation) {
                        Some(queue) if !queue.is_empty() => {
                            if queue.len() > 1 {
                                queue.pop_front().unwrap()
                            } else {
                                // Repeat the final scripted response so
                                // polling loops always have an answer.
                                queue.front().unwrap().clone()
                            }
                        }
                        _ => (
                            200,
                            json!({
                                "errors": [{
                                    "message": format!(
                                        "MockBackboard: no scripted response for operation {operation:?}"
                                    )
                                }],
                                "data": null,
                            })
                            .to_string(),
                        ),
                    }
                };

                let reason = match status {
                    200 => "OK",
                    400 => "Bad Request",
                    500 => "Internal Server Error",
                    502 => "Bad Gateway",
                    503 => "Service Unavailable",
                    _ => "Unknown",
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response_body}",
                    response_body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });

        Self {
            base_url: format!("http://127.0.0.1:{port}/graphql/v2"),
            requests,
            responses,
        }
    }

    pub fn url(&self) -> String {
        self.base_url.clone()
    }

    /// A `Configs` (backed by a throwaway path under `dir`) whose every
    /// GraphQL call resolves to this mock server.
    pub fn configs(&self, dir: &tempfile::TempDir) -> Configs {
        let mut configs = Configs::for_test(dir.path().join("config.json"));
        configs.override_backboard_url(self.url());
        configs
    }

    /// Queue a successful `{"data": ...}` response for `operation`.
    pub fn stub(&self, operation: &str, data: Value) {
        self.stub_raw(operation, 200, json!({ "data": data }).to_string());
    }

    /// Queue a GraphQL-level error (HTTP 200 with an `errors` array) for
    /// `operation` -- the shape backboard uses for resolver/UserError
    /// failures.
    pub fn stub_graphql_error(&self, operation: &str, message: &str) {
        self.stub_raw(
            operation,
            200,
            json!({ "errors": [{ "message": message }], "data": null }).to_string(),
        );
    }

    /// Queue an arbitrary HTTP response for `operation`.
    pub fn stub_raw(&self, operation: &str, status: u16, body: String) {
        self.responses
            .lock()
            .unwrap()
            .entry(operation.to_string())
            .or_default()
            .push_back((status, body));
    }

    /// Every request body received so far, as parsed JSON, in arrival order.
    pub fn requests(&self) -> Vec<Value> {
        self.requests.lock().unwrap().clone()
    }

    /// The `variables` of each received request for `operation`.
    pub fn variables_for(&self, operation: &str) -> Vec<Value> {
        self.requests()
            .into_iter()
            .filter(|r| r.get("operationName").and_then(Value::as_str) == Some(operation))
            .map(|r| r.get("variables").cloned().unwrap_or(Value::Null))
            .collect()
    }

    pub fn hits(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

/// Reads one HTTP request off `stream` and returns its body. `None` when the
/// stream closes before a full request arrives.
fn read_http_body(stream: &mut std::net::TcpStream) -> Option<Vec<u8>> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 1024];
    let mut content_length = 0usize;
    loop {
        let read = stream.read(&mut tmp).ok()?;
        if read == 0 {
            return None;
        }
        buf.extend_from_slice(&tmp[..read]);
        if let Some(pos) = find_headers_end(&buf) {
            let headers = String::from_utf8_lossy(&buf[..pos]).to_lowercase();
            for line in headers.lines() {
                if let Some(v) = line.strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            if buf.len() >= pos + 4 + content_length {
                let body_start = pos + 4;
                return Some(buf[body_start..body_start + content_length].to_vec());
            }
        }
    }
}

fn find_headers_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn routes_by_operation_name_and_records_variables() {
        let server = MockBackboard::spawn();
        server.stub("OpA", json!({ "a": 1 }));
        server.stub("OpB", json!({ "b": 2 }));

        let client = reqwest::Client::new();
        for (op, var) in [("OpA", "x"), ("OpB", "y"), ("OpA", "z")] {
            let response: Value = client
                .post(server.url())
                .json(&json!({ "operationName": op, "query": "{}", "variables": { "v": var } }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert!(response.get("data").is_some(), "unexpected: {response}");
        }

        assert_eq!(server.hits(), 3);
        assert_eq!(server.variables_for("OpA").len(), 2);
        assert_eq!(server.variables_for("OpB"), vec![json!({ "v": "y" })]);
    }

    #[tokio::test]
    async fn responses_are_served_in_order_and_the_last_repeats() {
        let server = MockBackboard::spawn();
        server.stub("Poll", json!({ "n": 1 }));
        server.stub("Poll", json!({ "n": 2 }));

        let client = reqwest::Client::new();
        let mut seen = Vec::new();
        for _ in 0..4 {
            let response: Value = client
                .post(server.url())
                .json(&json!({ "operationName": "Poll", "query": "{}" }))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            seen.push(response["data"]["n"].as_i64().unwrap());
        }
        assert_eq!(seen, vec![1, 2, 2, 2]);
    }

    #[tokio::test]
    async fn unscripted_operations_fail_loudly() {
        let server = MockBackboard::spawn();
        let client = reqwest::Client::new();
        let response: Value = client
            .post(server.url())
            .json(&json!({ "operationName": "Nope", "query": "{}" }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let message = response["errors"][0]["message"].as_str().unwrap();
        assert!(message.contains("no scripted response"));
        assert!(message.contains("Nope"));
    }
}
