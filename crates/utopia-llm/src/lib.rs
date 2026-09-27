//! utopia-llm: a thin client for the OpenAI-compatible protocol.
//! One set of code covers DeepSeek / Qwen(DashScope compatibility mode) / GLM / OpenAI /
//! Ollama / vLLM.

use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

/// A tool call in the OpenAI protocol (carried on an assistant turn).
#[derive(Debug, Clone)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// Arguments as a JSON string (passed through from the protocol verbatim)
    pub arguments: String,
}

/// One assistant turn of a tool conversation: text and tool calls, at least one of the two.
#[derive(Debug)]
pub struct AssistantTurn {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

impl AssistantTurn {
    /// Rebuilt as an OpenAI-protocol assistant message (for feeding the conversation history
    /// back in).
    pub fn to_message(&self) -> serde_json::Value {
        let mut msg = json!({ "role": "assistant", "content": self.content });
        if !self.tool_calls.is_empty() {
            msg["tool_calls"] = json!(self
                .tool_calls
                .iter()
                .map(|c| json!({
                    "id": c.id,
                    "type": "function",
                    "function": { "name": c.name, "arguments": c.arguments },
                }))
                .collect::<Vec<_>>());
        }
        msg
    }
}

/// A tool-result message (role=tool).
pub fn tool_result_message(tool_call_id: &str, content: &str) -> serde_json::Value {
    json!({ "role": "tool", "tool_call_id": tool_call_id, "content": content })
}

/// Streaming turn events for a conversation with tools.
#[derive(Debug)]
pub enum ToolStreamItem {
    /// A body delta (forwarded to the frontend immediately)
    Delta(String),
    /// End of stream: the complete turn (accumulated body + merged tool calls)
    Turn(AssistantTurn),
}

/// **Could not get a parseable answer out of the endpoint.** Two kinds: we never got through
/// (DNS, connection, TLS, timeout), or we did get through but what came back is not this API at
/// all (the response body does not parse as JSON).
///
/// Merging them into one class is deliberate: from the user's side these are the same thing --
/// "the address you configured is not a model API" -- and the thing to do is the same too: go look
/// at the URL, look at the proxy. The first version only caught transport-layer failures, and the
/// result was that the most common failure of all (URL misconfigured, a proxy in the middle
/// returning HTML) produced not a single alert, which is "silent failure" itself.
///
/// **Does not cover** an endpoint that cleanly returned 4xx/5xx: that means it really is the model
/// API, just with the wrong key, quota or model name -- a different class of problem, and a
/// different person to go find.
///
/// A type rather than matching on error text: any layer of the call chain adding one line of
/// context rewrites the text, while `anyhow`'s source chain keeps `downcast_ref` recognising it
/// all the way up.
#[derive(Debug, thiserror::Error)]
#[error("LLM endpoint gave no usable answer: {0}")]
pub struct Unreachable(#[from] pub reqwest::Error);

/// Whether the anyhow error chain contains an [`Unreachable`].
pub fn is_unreachable(err: &anyhow::Error) -> bool {
    err.chain().any(|e| e.is::<Unreachable>())
}

/// The endpoint is rate limiting. **A type, just like [`Unreachable`]**, for the same reason: the
/// caller has to use it to decide "come back in a bit" rather than "this chunk is a write-off",
/// and the error text is being rewritten by context all the way up.
///
/// What sets rate limiting apart from the other 4xx is that **it gets better by itself**. A wrong
/// key is still wrong after ten thousand retries, while a full quota clears in a minute -- mix the
/// two together and the retry budget gets spent on the class that will never get better.
#[derive(Debug, thiserror::Error)]
#[error("LLM endpoint is rate limiting ({status}): {detail}")]
pub struct RateLimited {
    pub status: u16,
    /// **Often `None`.** Most vendors' 429s carry no `Retry-After` (measured: SiliconFlow does
    /// not), so the caller must bring its own backoff and treat this as a "more precise when
    /// present" extra rather than as the deciding signal.
    pub retry_after: Option<Duration>,
    pub detail: String,
}

/// The [`RateLimited`] in an anyhow error chain, seen through the context layers.
pub fn rate_limited(err: &anyhow::Error) -> Option<&RateLimited> {
    err.chain().find_map(|e| e.downcast_ref::<RateLimited>())
}

/// The account cannot pay for this request: unpaid balance, or the plan's quota is used up.
///
/// **Kept apart from [`RateLimited`], because it does not get better by itself.** Rate limiting
/// clears after a minute; an unpaid balance is still unpaid at daybreak -- retrying just says the
/// same error three times over, while the thing that actually needs to happen (somebody tops the
/// account up) does not happen because of a retry.
///
/// Measured: in one test run 14 documents failed in their entirety because of this, and at the
/// time it went down the same path as any ordinary failure -- the only way to learn the reason was
/// to dig `graph_error` out of the database.
#[derive(Debug, thiserror::Error)]
#[error("LLM account cannot pay for this request ({status}): {detail}")]
pub struct OutOfCredit {
    pub status: u16,
    pub detail: String,
}

/// The [`OutOfCredit`] in an anyhow error chain, seen through the context layers.
pub fn out_of_credit(err: &anyhow::Error) -> Option<&OutOfCredit> {
    err.chain().find_map(|e| e.downcast_ref::<OutOfCredit>())
}

/// The integer-seconds form of `Retry-After`.
///
/// The spec also allows an HTTP-date, which we **do not** parse: pulling in a date library for a
/// header hardly anyone sends is not worth it, and treating a parse failure as absent is exactly
/// right -- the caller has to have backoff anyway, and one more guessed number only makes things
/// harder to track down.
fn retry_after_of(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Every non-2xx takes shape here, sorted into three classes: out of credit, rate limited, other.
///
/// **503 does not count as rate limiting**: it may be the endpoint genuinely down, or it may be a
/// proxy in the middle, and counting it would apply "come back in a bit" where nothing is coming
/// back. Keep the test narrow; falling back to "other" is the lesser evil.
fn failure(
    kind: &str,
    status: reqwest::StatusCode,
    retry_after: Option<Duration>,
    body: &serde_json::Value,
) -> anyhow::Error {
    let detail = err_detail(body);
    // **Out of credit is judged first, and it cannot go by status code alone.**
    //
    // 402 is the textbook answer (SiliconFlow uses it), but OpenAI signals an exhausted balance
    // with **429**, told apart by `insufficient_quota` in the body. Classify by status code alone
    // and an OpenAI account with no money gets taken for rate limiting, then backs off and retries
    // forever against something that will never get better -- and the longer the backoff, the more
    // the symptom looks like "the endpoint is slow", the less anyone finds the root.
    if status == reqwest::StatusCode::PAYMENT_REQUIRED || says_out_of_credit(body) {
        return anyhow::Error::new(OutOfCredit {
            status: status.as_u16(),
            detail,
        });
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return anyhow::Error::new(RateLimited {
            status: status.as_u16(),
            retry_after,
            detail,
        });
    }
    anyhow::anyhow!("{kind} request failed ({status}): {detail}")
}

#[derive(Clone)]
pub struct LlmClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
    pub model: String,
}

/// How long establishing a connection may take before it counts as a failure.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// **How long without a new byte before this request counts as dead.**
///
/// `Client::timeout` (total request duration) will not do: `chat_tools_stream` is genuinely
/// streaming, one long conversation legitimately runs for minutes, and a cap on total duration
/// would cut it off mid-body. `read_timeout` measures **silence** instead -- under streaming,
/// tokens keep arriving and it is never reached; under non-streaming it catches exactly the case
/// of "the request went out and sank without a trace".
///
/// Why 300 seconds and not 60: the first byte of a non-streaming call waits for the model to
/// finish generating the whole passage, extraction with a big prompt normally takes 60-120
/// seconds, and longer when the server is queueing. Set it too low and normal requests get
/// declared dead, and on this path "killing a healthy request" is far more expensive than
/// "noticing five minutes late" -- it makes extractions fail that would otherwise have succeeded.
///
/// **The cost of not having it has been measured**: `reqwest::Client::new()` sets no timeout at
/// all, and with 32 concurrent lanes hitting the same account every request hung, all 32 worker
/// slots were occupied permanently, and the pipeline stalled while reporting **not a single
/// error** (the orphan reclaim for jobs only runs once at process start, so while the process is
/// alive nothing ever gets buried). One ingest of 7459 chunks died on chunk 55.
const READ_TIMEOUT: Duration = Duration::from_secs(300);

impl LlmClient {
    pub fn new(base_url: &str, api_key: Option<&str>, model: &str) -> Self {
        Self::with_timeouts(base_url, api_key, model, CONNECT_TIMEOUT, READ_TIMEOUT)
    }

    /// Timeouts are injectable only to make this **testable** -- production goes through
    /// [`LlmClient::new`]. Testing a hang with 300 seconds takes 5 minutes; nobody keeps that test.
    pub fn with_timeouts(
        base_url: &str,
        api_key: Option<&str>,
        model: &str,
        connect: Duration,
        read: Duration,
    ) -> Self {
        Self {
            http: reqwest::Client::builder()
                .connect_timeout(connect)
                .read_timeout(read)
                .build()
                // Only fails when the TLS backend cannot come up, and in that case falling back
                // to the default client is meaningless too -- but the whole process should not
                // die here either
                .unwrap_or_else(|e| {
                    tracing::error!(error = %e, "could not build the HTTP client, falling back to the default with no timeouts");
                    reqwest::Client::new()
                }),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(String::from),
            model: model.to_string(),
        }
    }

    fn request(&self, path: &str) -> reqwest::RequestBuilder {
        let mut req = self.http.post(format!("{}{path}", self.base_url));
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        req
    }

    /// Non-streaming chat (lightweight cases such as connectivity tests).
    pub async fn chat(&self, messages: &[ChatMessage]) -> anyhow::Result<String> {
        let resp = self
            .request("/chat/completions")
            .json(&json!({ "model": self.model, "messages": messages, "stream": false }))
            .send()
            .await
            .map_err(Unreachable)?;
        let status = resp.status();
        let retry_after = retry_after_of(resp.headers());
        let body: serde_json::Value = resp.json().await.map_err(Unreachable)?;
        if !status.is_success() {
            return Err(failure("LLM", status, retry_after, &body));
        }
        log_usage(&self.model, &body);
        body["choices"][0]["message"]["content"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| anyhow::anyhow!("Unexpected LLM response shape: {body}"))
    }

    /// Tool conversation (non-streaming): messages is raw OpenAI-protocol JSON (assistant
    /// .tool_calls and role=tool turns are supported), tools is an array of function definitions.
    pub async fn chat_tools(
        &self,
        messages: &[serde_json::Value],
        tools: &serde_json::Value,
    ) -> anyhow::Result<AssistantTurn> {
        let resp = self
            .request("/chat/completions")
            .json(&json!({
                "model": self.model,
                "messages": messages,
                "tools": tools,
                "stream": false,
            }))
            .send()
            .await
            .map_err(Unreachable)?;
        let status = resp.status();
        let retry_after = retry_after_of(resp.headers());
        let body: serde_json::Value = resp.json().await.map_err(Unreachable)?;
        if !status.is_success() {
            return Err(failure("LLM", status, retry_after, &body));
        }
        let msg = &body["choices"][0]["message"];
        if msg.is_null() {
            anyhow::bail!("Unexpected LLM response shape: {body}");
        }
        let content = msg["content"]
            .as_str()
            .map(String::from)
            .filter(|s| !s.is_empty());
        let tool_calls = msg["tool_calls"]
            .as_array()
            .map(|calls| {
                calls
                    .iter()
                    .filter_map(|c| {
                        Some(ToolCall {
                            id: c["id"].as_str()?.to_string(),
                            name: c["function"]["name"].as_str()?.to_string(),
                            arguments: c["function"]["arguments"]
                                .as_str()
                                .unwrap_or("{}")
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(AssistantTurn {
            content,
            tool_calls,
        })
    }

    /// Tool conversation (streaming): body deltas are produced immediately, tool calls are merged
    /// by the OpenAI protocol's index shards (id/name arrive in the first frame, arguments continue
    /// frame by frame), and the complete turn is given at the end of the stream.
    pub async fn chat_tools_stream(
        &self,
        messages: &[serde_json::Value],
        tools: &serde_json::Value,
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<ToolStreamItem>> + Send + use<>> {
        let resp = self
            .request("/chat/completions")
            .json(&json!({
                "model": self.model,
                "messages": messages,
                "tools": tools,
                "stream": true,
            }))
            .send()
            .await
            .map_err(Unreachable)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let retry_after = retry_after_of(resp.headers());
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            return Err(failure("LLM", status, retry_after, &body));
        }

        let mut bytes = resp.bytes_stream();
        let stream = async_stream::try_stream! {
            let mut buf = String::new();
            let mut content = String::new();
            let mut calls: Vec<ToolCall> = Vec::new();
            let mut done = false;
            'outer: while let Some(part) = bytes.next().await {
                let part = part?;
                buf.push_str(&String::from_utf8_lossy(&part));
                while let Some(pos) = buf.find("\n\n") {
                    let frame = buf[..pos].to_string();
                    buf.drain(..pos + 2);
                    for line in frame.lines() {
                        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                            continue;
                        };
                        if data == "[DONE]" {
                            done = true;
                            break 'outer;
                        }
                        // TEMP DEBUG(remove): log every frame verbatim, to see
                        // whether tool-call deltas are sent more than once
                        tracing::debug!(%data, "llm sse frame");
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                            continue;
                        };
                        let delta = &v["choices"][0]["delta"];
                        if let Some(text) = delta["content"].as_str() {
                            if !text.is_empty() {
                                content.push_str(text);
                                yield ToolStreamItem::Delta(text.to_string());
                            }
                        }
                        if let Some(tcs) = delta["tool_calls"].as_array() {
                            for tc in tcs {
                                let idx = tc["index"].as_u64().unwrap_or(0) as usize;
                                while calls.len() <= idx {
                                    calls.push(ToolCall {
                                        id: String::new(),
                                        name: String::new(),
                                        arguments: String::new(),
                                    });
                                }
                                let slot = &mut calls[idx];
                                if let Some(id) = tc["id"].as_str() {
                                    slot.id.push_str(id);
                                }
                                if let Some(n) = tc["function"]["name"].as_str() {
                                    slot.name.push_str(n);
                                }
                                if let Some(a) = tc["function"]["arguments"].as_str() {
                                    slot.arguments.push_str(a);
                                }
                            }
                        }
                    }
                }
            }
            let _ = done;
            calls.retain(|c| !c.name.is_empty());
            let content = if content.is_empty() { None } else { Some(content) };
            yield ToolStreamItem::Turn(AssistantTurn { content, tool_calls: calls });
        };
        Ok(stream)
    }

    /// Streaming chat: produces incremental text fragments.
    pub async fn chat_stream(
        &self,
        messages: &[ChatMessage],
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<String>> + Send + use<>> {
        let messages = messages
            .iter()
            .map(|m| serde_json::to_value(m).unwrap_or_default())
            .collect::<Vec<_>>();
        self.chat_stream_raw(&messages).await
    }

    /// Streaming chat (raw JSON messages, may carry tool-turn context).
    pub async fn chat_stream_raw(
        &self,
        messages: &[serde_json::Value],
    ) -> anyhow::Result<impl Stream<Item = anyhow::Result<String>> + Send + use<>> {
        let resp = self
            .request("/chat/completions")
            .json(&json!({ "model": self.model, "messages": messages, "stream": true }))
            .send()
            .await
            .map_err(Unreachable)?;
        if !resp.status().is_success() {
            let status = resp.status();
            let retry_after = retry_after_of(resp.headers());
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            return Err(failure("LLM", status, retry_after, &body));
        }

        let mut bytes = resp.bytes_stream();
        let stream = async_stream::try_stream! {
            let mut buf = String::new();
            while let Some(part) = bytes.next().await {
                let part = part?;
                buf.push_str(&String::from_utf8_lossy(&part));
                // SSE frames are separated by blank lines; take out frame by frame whatever has
                // fully arrived
                while let Some(pos) = buf.find("\n\n") {
                    let frame = buf[..pos].to_string();
                    buf.drain(..pos + 2);
                    for line in frame.lines() {
                        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                            continue;
                        };
                        if data == "[DONE]" {
                            return;
                        }
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(data) {
                            if let Some(delta) = v["choices"][0]["delta"]["content"].as_str() {
                                if !delta.is_empty() {
                                    yield delta.to_string();
                                }
                            }
                        }
                    }
                }
            }
        };
        Ok(stream)
    }

    /// Batch embedding.
    pub async fn embed(&self, texts: &[String]) -> anyhow::Result<Vec<Vec<f32>>> {
        let resp = self
            .request("/embeddings")
            .json(&json!({ "model": self.model, "input": texts }))
            .send()
            .await
            .map_err(Unreachable)?;
        let status = resp.status();
        let retry_after = retry_after_of(resp.headers());
        let body: serde_json::Value = resp.json().await.map_err(Unreachable)?;
        if !status.is_success() {
            return Err(failure("Embedding", status, retry_after, &body));
        }
        let data = body["data"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("Unexpected embedding response shape"))?;
        let mut out = Vec::with_capacity(data.len());
        for item in data {
            let v = item["embedding"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Embedding response has no vector"))?
                .iter()
                .filter_map(|x| x.as_f64().map(|f| f as f32))
                .collect();
            out.push(v);
        }
        Ok(out)
    }
}

/// Record the token cost of one call. **The cache-hit count is the most important column here**:
/// extraction feeds on the vendors' prefix caching by way of "the system message is byte-identical
/// across every chunk within one document", and stuffing content that varies per chunk into system
/// quietly drops it to zero -- this number is the only place that shows.
///
/// Field names differ by vendor: OpenAI uses prompt_tokens_details.cached_tokens, DeepSeek uses
/// prompt_cache_hit_tokens. Read both, whichever one is there.
fn log_usage(model: &str, body: &serde_json::Value) {
    let u = &body["usage"];
    if u.is_null() {
        return;
    }
    let n = |k: &str| u[k].as_u64();
    let cached = u["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .or_else(|| n("prompt_cache_hit_tokens"));
    tracing::info!(
        model,
        prompt = n("prompt_tokens"),
        completion = n("completion_tokens"),
        cached,
        "llm usage"
    );
}

fn err_detail(body: &serde_json::Value) -> String {
    body["error"]["message"]
        .as_str()
        .or_else(|| body["message"].as_str())
        .unwrap_or("unknown error")
        .to_string()
}

/// Whether the response body says this is a balance problem.
///
/// **This is for the 429s**: OpenAI uses one status code for both "too fast" and "out of money",
/// and `insufficient_quota` in `error.code` or `error.type` is the only dividing line.
///
/// Only this one marker is recognised, with no matching against the free text of message: wording
/// changes and gets localised, while `code` is part of the interface contract. When it is not
/// recognised we fall back to judging by status code, which is the safe side (backing off a few
/// times as rate limiting is gentler than giving up outright as out of credit).
fn says_out_of_credit(body: &serde_json::Value) -> bool {
    ["code", "type"]
        .iter()
        .filter_map(|k| body["error"][k].as_str())
        .any(|v| v == "insufficient_quota")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Actually send a request doomed to fail, to get a genuine `reqwest::Error`.
    /// Nothing will be listening on port 1, and 127.0.0.1 does not go through a proxy.
    async fn a_real_transport_error() -> reqwest::Error {
        reqwest::Client::builder()
            .no_proxy()
            .build()
            .unwrap()
            .get("http://127.0.0.1:1/")
            .send()
            .await
            .expect_err("port 1 should not be connectable")
    }

    /// A server that **accepts the connection and then answers with not one byte**.
    ///
    /// This is the shape that actually happens in production, and the hardest kind to spot: TCP
    /// connects, TLS shakes hands, the request goes out, and then nothing. A connection error is
    /// reported right away, this one is not -- without a timeout `send().await` just stops there
    /// forever, and the worker slot that called it is never released again.
    async fn a_server_that_never_answers() -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            // Take the connection and never let go. **The socket must be held**: dropping it is
            // a FIN, and then what is being tested is a closed connection again, not silence
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });
        addr
    }

    /// When a request hangs it must **return an error**, not wait forever.
    ///
    /// Without this test standing guard, the regression looks like this: an ingest dies on chunk
    /// 55, all 32 worker slots are occupied permanently, the jobs table is nothing but running,
    /// and there is not one word in the logs or the UI.
    #[tokio::test]
    async fn a_silent_server_ends_in_an_error_not_a_hang() {
        let addr = a_server_that_never_answers().await;
        let client = LlmClient::with_timeouts(
            &format!("http://{addr}"),
            None,
            "m",
            Duration::from_secs(5),
            Duration::from_millis(300),
        );
        let started = tokio::time::Instant::now();
        let out = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            client.chat(&[ChatMessage {
                role: "user".into(),
                content: "hi".into(),
            }]),
        )
        .await;

        // Outer timeout firing = the client never cut it off itself, exactly the bug being fixed
        let inner = out.expect("the client did not time out, the request hung forever");
        assert!(inner.is_err(), "a silent server must not count as success");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "read_timeout did not take effect: waited {:?}",
            started.elapsed()
        );
    }

    /// **The check must see through the context layers.**
    ///
    /// This is the link in the whole chain most likely to break quietly: every
    /// `.context("extraction failed")` a caller adds swaps the error text out, and a check that
    /// matches on text is dead the same day -- while the symptom is that the alert never appears
    /// again, no test goes red, and no user comes to report "I did not get an alert".
    #[tokio::test]
    async fn unreachable_survives_context_layers() {
        let raw = a_real_transport_error().await;
        let err = anyhow::Error::new(Unreachable(raw))
            .context("embedding failed")
            .context("process_document failed");
        assert!(is_unreachable(&err));
        // Also pins down "the text changes" itself: the outermost layer no longer mentions the
        // endpoint at all
        assert!(!err.to_string().contains("endpoint"));
    }

    /// The flip side: an ordinary error must not be taken for an endpoint problem, or the alert
    /// would light up on every failure.
    #[tokio::test]
    async fn an_ordinary_failure_is_not_the_endpoint() {
        let e = anyhow::anyhow!("Embedding response has no vector").context("extraction failed");
        assert!(!is_unreachable(&e));
    }

    /// **A rate limit has to be recognised through the context layers.**
    ///
    /// Same reason as [`unreachable_survives_context_layers`], with heavier consequences: not
    /// recognising it falls back to "this chunk is a write-off", when the rate limit would have
    /// cleared a minute later. Measured: in one ingest of 1884 chunks, 55 of 60 documents failed
    /// in their entirety because of this.
    #[tokio::test]
    async fn a_rate_limit_survives_context_layers() {
        let err = anyhow::Error::new(RateLimited {
            status: 429,
            retry_after: None,
            detail: "TPM limit reached".into(),
        })
        .context("extraction failed")
        .context("process_document failed");
        let hit = rate_limited(&err).expect("the rate limit was not recognised");
        assert_eq!(hit.status, 429);
        assert!(!err.to_string().contains("rate limiting"));
    }

    /// **Having no `Retry-After` is the normal case, not the exception.**
    ///
    /// Most vendors' 429s do not carry the header. The check must go by type alone, with the
    /// backoff supplied by the caller -- take `retry_after.is_some()` as the deciding signal and
    /// not one of these vendors gets recognised.
    #[tokio::test]
    async fn a_rate_limit_without_retry_after_is_still_a_rate_limit() {
        let err = anyhow::Error::new(RateLimited {
            status: 429,
            retry_after: None,
            detail: "TPM limit reached".into(),
        });
        assert!(rate_limited(&err).is_some());
        assert!(rate_limited(&err).unwrap().retry_after.is_none());
    }

    /// The flip side: a rate limit must not be taken for an unreachable endpoint -- the two are
    /// handled in completely different ways.
    #[tokio::test]
    async fn a_rate_limit_is_not_an_unreachable_endpoint() {
        let err = anyhow::Error::new(RateLimited {
            status: 429,
            retry_after: Some(Duration::from_secs(7)),
            detail: "slow down".into(),
        });
        assert!(!is_unreachable(&err));
        assert_eq!(
            rate_limited(&err).unwrap().retry_after,
            Some(Duration::from_secs(7))
        );
    }

    /// **A 429 is not necessarily a rate limit.** OpenAI uses one status code for both "too fast"
    /// and "out of money", and the dividing line is in `error.code`. Classify by status code alone
    /// and an account with no money gets retried with unbounded backoff -- and the longer the
    /// retrying goes on, the more the symptom looks like "the endpoint is slow", the less anyone
    /// finds the root.
    #[test]
    fn a_429_that_says_insufficient_quota_is_a_billing_problem() {
        let body = serde_json::json!({
            "error": { "message": "You exceeded your current quota", "code": "insufficient_quota" }
        });
        let e = failure("LLM", reqwest::StatusCode::TOO_MANY_REQUESTS, None, &body);
        assert!(out_of_credit(&e).is_some(), "should be out of credit");
        assert!(rate_limited(&e).is_none(), "should not be a rate limit");
    }

    /// 402 is the textbook answer, and it is exactly what SiliconFlow uses.
    #[test]
    fn a_402_is_a_billing_problem() {
        let body = serde_json::json!({ "message": "Sorry, your account balance is insufficient" });
        let e = failure("LLM", reqwest::StatusCode::PAYMENT_REQUIRED, None, &body);
        assert!(out_of_credit(&e).is_some());
    }

    /// The flip side: a 429 without that marker is still a rate limit -- do not judge something
    /// that gets better by itself as something needing a human.
    #[test]
    fn a_plain_429_is_still_a_rate_limit() {
        let body = serde_json::json!({ "error": { "message": "TPM limit reached" } });
        let e = failure("LLM", reqwest::StatusCode::TOO_MANY_REQUESTS, None, &body);
        assert!(rate_limited(&e).is_some());
        assert!(out_of_credit(&e).is_none());
    }

    /// The flip side: other 4xx are not rate limits. A wrong key is still wrong after ten
    /// thousand retries.
    #[tokio::test]
    async fn an_auth_failure_is_not_a_rate_limit() {
        let e = anyhow::anyhow!("LLM request failed (401 Unauthorized): bad key");
        assert!(rate_limited(&e).is_none());
    }

    /// An endpoint that cleanly returned a 4xx does not count -- that means it really is the model
    /// API, just with the wrong key or model name, and both the person to find and the thing to do
    /// are different.
    #[tokio::test]
    async fn a_clean_api_error_is_a_different_problem() {
        let e = anyhow::anyhow!("LLM request failed (401 Unauthorized): bad key");
        assert!(!is_unreachable(&e));
    }
}
