//! One chat turn: persist, optionally search the web, stream the reply from Ollama, flush the draft, finalize.
//! All of it runs in Rust; the webview only receives events and can never cause data loss by going away.

use crate::{
    db::{now_ms, Db, HistTurn, NewAttachment, Turn},
    ollama::{self, Ollama},
    search::{self, Hit},
};
use base64::Engine;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::Notify;

pub struct Active {
    pub message_id: i64,
    pub conversation_id: i64,
    pub cancel: Arc<Notify>,
}

pub struct Shared {
    pub db: Db,
    pub ollama: Ollama,
    pub active: Mutex<Option<Active>>,
    pub flush_every: Duration,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct SendArgs {
    pub conversation_id: Option<i64>,
    pub model: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<NewAttachment>,
    pub num_ctx: u32,
    #[serde(default)]
    pub web: bool,
}

#[derive(Serialize, Clone, Debug)]
#[serde(tag = "event", rename_all = "camelCase")]
pub enum ChatEvent {
    Searching,
    #[serde(rename_all = "camelCase")]
    Sources {
        sources: Vec<SourceLink>,
    },
    #[serde(rename_all = "camelCase")]
    SearchError {
        message: String,
    },
    #[serde(rename_all = "camelCase")]
    Delta {
        content: String,
        thinking: String,
    },
    /// status: "complete" | "stopped"
    #[serde(rename_all = "camelCase")]
    Done {
        status: String,
        stats: Value,
    },
    #[serde(rename_all = "camelCase")]
    Error {
        message: String,
        stats: Value,
    },
}

#[derive(Serialize, Clone, Debug)]
pub struct SourceLink {
    pub title: String,
    pub url: String,
}

pub type Emit = Arc<dyn Fn(ChatEvent) + Send + Sync>;

// ---------- prompt building ----------

/// Remove `<think>…</think>` blocks (and an unterminated trailing `<think>`) so a model never re-reads its own reasoning.
pub fn strip_think(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("<think>") {
        out.push_str(&rest[..i]);
        match rest[i..].find("</think>") {
            Some(j) => rest = &rest[i + j + "</think>".len()..],
            None => return out.trim().to_string(),
        }
    }
    out.push_str(rest);
    out.trim().to_string()
}

pub fn iso_date(ms: i64) -> String {
    // civil-from-days (Howard Hinnant)
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + (m <= 2) as i64;
    format!("{y:04}-{m:02}-{d:02}")
}

/// Text injected in front of the question for this one turn. Never stored.
pub fn web_block(q: &str, hits: &[Hit], date: &str) -> String {
    let q: String = q.chars().take(200).collect();
    let items = hits.iter().enumerate().map(|(i, h)| format!("[{}] {} ({})\n{}", i + 1, h.title, h.url, h.snippet)).collect::<Vec<_>>().join("\n\n");
    format!("Web search results for \"{q}\" (fetched {date}):\n{items}\n\nAnswer using these results and cite sources like [1]. If they do not contain the answer, say so.\n\n")
}

pub fn build_messages(hist: &[HistTurn], vision: bool, web: &str) -> Vec<Value> {
    let last_user = hist.iter().rposition(|h| h.role == "user");
    let mut out = Vec::new();
    for (i, h) in hist.iter().enumerate() {
        if h.role == "user" {
            let docs = h.docs.iter().map(|(n, t)| format!("Document \"{n}\":\n{t}")).collect::<Vec<_>>().join("\n\n");
            let q = if !h.content.trim().is_empty() {
                h.content.trim().to_string()
            } else if !h.docs.is_empty() {
                "Summarize this document.".to_string()
            } else {
                "Describe this image.".to_string()
            };
            let mut content = if docs.is_empty() { q } else { format!("{docs}\n\n{q}") };
            if Some(i) == last_user && !web.is_empty() {
                content = format!("{web}{content}");
            }
            let mut m = json!({ "role": "user", "content": content });
            if vision && !h.images.is_empty() {
                m["images"] = Value::Array(h.images.iter().map(|b| Value::String(base64::engine::general_purpose::STANDARD.encode(b))).collect());
            }
            out.push(m);
        } else {
            let c = strip_think(&h.content);
            if !c.is_empty() {
                out.push(json!({ "role": "assistant", "content": c }));
            }
        }
    }
    out
}

// ---------- streaming ----------

#[derive(Deserialize, Default)]
struct Line {
    message: Option<LineMsg>,
    #[serde(default)]
    done: bool,
    error: Option<String>,
    eval_count: Option<u64>,
    eval_duration: Option<u64>,
    prompt_eval_count: Option<u64>,
}
#[derive(Deserialize, Default)]
struct LineMsg {
    #[serde(default)]
    content: String,
    thinking: Option<String>,
}

enum Outcome {
    Done(Value),
    Stopped,
    Failed(String),
}

fn http_error(status: u16, body: &str) -> String {
    let detail =
        serde_json::from_str::<Value>(body).ok().and_then(|j| j["error"].as_str().map(String::from)).unwrap_or_else(|| body.trim().chars().take(300).collect());
    if detail.is_empty() {
        format!("Ollama returned HTTP {status}")
    } else {
        detail
    }
}

#[allow(clippy::too_many_arguments)]
async fn stream_loop(
    sh: &Shared,
    emit: &Emit,
    port: u16,
    body: Value,
    aid: i64,
    cancel: &Notify,
    content: &mut String,
    thinking: &mut String,
    started: Instant,
    base_stats: &Value,
) -> Outcome {
    let url = format!("{}/api/chat", ollama::base(port));
    let sent = tokio::select! { biased; _ = cancel.notified() => return Outcome::Stopped, r = sh.ollama.client.post(&url).json(&body).send() => r };
    let resp = match sent {
        Ok(r) => r,
        Err(_) => return Outcome::Failed(format!("Cannot reach Ollama on port {port}. Is it running?")),
    };
    if !resp.status().is_success() {
        let code = resp.status().as_u16();
        return Outcome::Failed(http_error(code, &resp.text().await.unwrap_or_default()));
    }
    let mut stream = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut tick = tokio::time::interval(sh.flush_every);
    tick.tick().await;
    let mut dirty = false;
    loop {
        tokio::select! {
            biased;
            _ = cancel.notified() => return Outcome::Stopped,
            _ = tick.tick() => {
                if dirty {
                    let _ = sh.db.update_draft(aid, content, thinking);
                    dirty = false;
                }
            }
            chunk = stream.next() => {
                let bytes = match chunk {
                    None => return Outcome::Failed("The connection to Ollama closed before the reply finished.".into()),
                    Some(Err(e)) => return Outcome::Failed(format!("Stream interrupted: {e}")),
                    Some(Ok(b)) => b,
                };
                buf.extend_from_slice(&bytes);
                let (mut dc, mut dt) = (String::new(), String::new());
                let mut finished: Option<Value> = None;
                let mut failed: Option<String> = None;
                while let Some(pos) = buf.iter().position(|b| *b == b'\n') {
                    let raw: Vec<u8> = buf.drain(..=pos).collect();
                    let Ok(line) = serde_json::from_slice::<Line>(&raw) else { continue };
                    if let Some(e) = line.error { failed = Some(e); break; }
                    if let Some(m) = line.message {
                        dc.push_str(&m.content);
                        if let Some(t) = m.thinking { dt.push_str(&t); }
                    }
                    if line.done {
                        let mut s = base_stats.clone();
                        s["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
                        if let (Some(n), Some(d)) = (line.eval_count, line.eval_duration) {
                            if d > 0 { s["tokS"] = json!(n as f64 / (d as f64 / 1e9)); }
                            s["evalCount"] = json!(n);
                        }
                        if let Some(p) = line.prompt_eval_count { s["promptEvalCount"] = json!(p); }
                        finished = Some(s);
                        break;
                    }
                }
                if !dc.is_empty() || !dt.is_empty() {
                    content.push_str(&dc);
                    thinking.push_str(&dt);
                    dirty = true;
                    emit(ChatEvent::Delta { content: dc, thinking: dt });
                }
                if let Some(e) = failed { return Outcome::Failed(e); }
                if let Some(s) = finished { return Outcome::Done(s); }
            }
        }
    }
}

pub async fn run_turn(sh: Arc<Shared>, emit: Emit, turn: Turn, a: SendArgs, port: u16, cancel: Arc<Notify>) {
    let (conversation_id, aid) = (turn.conversation_id, turn.assistant_message_id);
    let started = Instant::now();
    let mut base_stats = json!({ "model": a.model, "numCtx": a.num_ctx });
    let mut content = String::new();
    let mut thinking = String::new();

    let outcome = 'turn: {
        // 1. web search (optional, this turn only)
        let mut web = String::new();
        if a.web && !a.text.trim().is_empty() {
            emit(ChatEvent::Searching);
            let r = tokio::select! { biased; _ = cancel.notified() => break 'turn Outcome::Stopped, r = search::search(&sh.ollama.client, &a.text, 5) => r };
            let err = match r {
                Ok(hits) if !hits.is_empty() => {
                    web = web_block(&a.text, &hits, &iso_date(now_ms()));
                    let links: Vec<SourceLink> = hits.iter().map(|h| SourceLink { title: h.title.clone(), url: h.url.clone() }).collect();
                    let _ = sh.db.set_sources(aid, &serde_json::to_value(&links).unwrap_or(Value::Null));
                    emit(ChatEvent::Sources { sources: links });
                    None
                }
                Ok(_) => Some("Web search returned no results.".to_string()),
                Err(e) => Some(format!("Web search failed: {e}")),
            };
            if let Some(m) = err {
                base_stats["searchError"] = json!(m);
                emit(ChatEvent::SearchError { message: m });
            }
        }

        // 2. prompt = stored history (no web text) + this turn's web block
        let vision = sh.ollama.has_vision(port, &a.model).await;
        let hist = match sh.db.history_before(conversation_id, aid) {
            Ok(h) => h,
            Err(e) => break 'turn Outcome::Failed(format!("Could not read chat history: {e}")),
        };
        let body = json!({
            "model": a.model,
            "messages": build_messages(&hist, vision, &web),
            "stream": true,
            "options": { "num_ctx": a.num_ctx },
        });

        // 3. stream
        stream_loop(&sh, &emit, port, body, aid, &cancel, &mut content, &mut thinking, started, &base_stats).await
    };

    let (status, stats, event) = match outcome {
        Outcome::Done(s) => ("complete", s.clone(), ChatEvent::Done { status: "complete".into(), stats: s }),
        Outcome::Stopped => {
            let mut s = base_stats.clone();
            s["elapsedMs"] = json!(started.elapsed().as_millis() as u64);
            ("stopped", s.clone(), ChatEvent::Done { status: "stopped".into(), stats: s })
        }
        Outcome::Failed(msg) => {
            let mut s = base_stats.clone();
            s["error"] = json!(msg);
            ("error", s.clone(), ChatEvent::Error { message: msg, stats: s })
        }
    };
    // Persist first, then tell the UI: if the webview is gone the reply is still safe.
    if let Err(e) = sh.db.finalize(aid, &content, &thinking, status, &stats) {
        eprintln!("chat: could not finalize message {aid}: {e}");
    }
    *sh.active.lock().unwrap_or_else(|e| e.into_inner()) = None;
    emit(event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    #[test]
    fn strips_think_blocks() {
        assert_eq!(strip_think("<think>plan</think>\n\nAnswer"), "Answer");
        assert_eq!(strip_think("a <think>x</think> b <think>y</think> c"), "a  b  c");
        assert_eq!(strip_think("before <think>never closed"), "before");
        assert_eq!(strip_think("plain"), "plain");
    }

    #[test]
    fn dates() {
        assert_eq!(iso_date(0), "1970-01-01");
        assert_eq!(iso_date(1_709_164_800_000), "2024-02-29");
        assert_eq!(iso_date(1_798_675_200_000 + 86_399_000), "2026-12-31");
    }

    fn turn(role: &str, content: &str) -> HistTurn {
        HistTurn { role: role.into(), content: content.into(), docs: vec![], images: vec![] }
    }

    #[test]
    fn web_text_goes_only_into_the_latest_user_message() {
        let hits = vec![Hit { title: "T".into(), url: "https://u".into(), snippet: "S".into() }];
        let w = web_block("q", &hits, "2026-01-02");
        assert!(w.starts_with("Web search results for \"q\" (fetched 2026-01-02):\n[1] T (https://u)\nS\n\n"));
        let hist = vec![turn("user", "old"), turn("assistant", "<think>t</think>reply"), turn("user", "new")];
        let m = build_messages(&hist, true, &w);
        assert_eq!(m[0]["content"], "old");
        assert_eq!(m[1]["content"], "reply");
        assert!(m[2]["content"].as_str().unwrap().starts_with("Web search results"));
        assert!(m[2]["content"].as_str().unwrap().ends_with("new"));
        let again = build_messages(&hist, true, "");
        assert_eq!(again[2]["content"], "new", "next turn is rebuilt from storage without the injected text");
    }

    #[test]
    fn documents_images_and_vision_gating() {
        let mut u = turn("user", "");
        u.docs = vec![("a.pdf".into(), "TEXT".into())];
        u.images = vec![vec![1, 2, 3]];
        let with = build_messages(std::slice::from_ref(&u), true, "");
        assert_eq!(with[0]["content"], "Document \"a.pdf\":\nTEXT\n\nSummarize this document.");
        assert_eq!(with[0]["images"][0], "AQID");
        let without = build_messages(&[u], false, "");
        assert!(without[0].get("images").is_none());
        let img_only = HistTurn { role: "user".into(), content: "".into(), docs: vec![], images: vec![vec![1]] };
        assert_eq!(build_messages(&[img_only], true, "")[0]["content"], "Describe this image.");
    }

    #[test]
    fn event_wire_format() {
        let v = serde_json::to_value(ChatEvent::Delta { content: "a".into(), thinking: "".into() }).unwrap();
        assert_eq!(v, json!({"event": "delta", "content": "a", "thinking": ""}));
        let v = serde_json::to_value(ChatEvent::SearchError { message: "x".into() }).unwrap();
        assert_eq!(v["event"], "searchError");
    }

    // ----- end-to-end against a mock Ollama -----

    /// Minimal HTTP server: answers /api/show and streams `lines` for /api/chat, with `gap` between lines.
    async fn mock_ollama(lines: Vec<String>, gap: Duration, hang_after: bool) -> u16 {
        mock_ollama_rec(lines, gap, hang_after, Arc::new(Mutex::new(Vec::new()))).await
    }

    /// Same, and every request body the mock sees is appended to `rec`.
    async fn mock_ollama_rec(lines: Vec<String>, gap: Duration, hang_after: bool, rec: Arc<Mutex<Vec<String>>>) -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = l.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = l.accept().await else { return };
                let lines = lines.clone();
                let rec = rec.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let mut got = Vec::new();
                    loop {
                        let n = s.read(&mut buf).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        got.extend_from_slice(&buf[..n]);
                        if let Some(h) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&got[..h]).to_lowercase();
                            let cl = head.lines().find_map(|l| l.strip_prefix("content-length: ")).and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                            if got.len() >= h + 4 + cl {
                                break;
                            }
                        }
                    }
                    let req = String::from_utf8_lossy(&got).to_string();
                    if let Some(i) = req.find("\r\n\r\n") {
                        rec.lock().unwrap().push(format!("{}\n{}", req.lines().next().unwrap_or(""), &req[i + 4..]));
                    }
                    if req.starts_with("POST /api/show") {
                        let b = r#"{"capabilities":["completion"]}"#;
                        let _ = s
                            .write_all(
                                format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", b.len(), b)
                                    .as_bytes(),
                            )
                            .await;
                        return;
                    }
                    let _ =
                        s.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n").await;
                    for line in lines {
                        let chunk = format!("{line}\n");
                        if s.write_all(format!("{:x}\r\n{}\r\n", chunk.len(), chunk).as_bytes()).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(gap).await;
                    }
                    if hang_after {
                        tokio::time::sleep(Duration::from_secs(30)).await;
                    } else {
                        let _ = s.write_all(b"0\r\n\r\n").await;
                    }
                });
            }
        });
        port
    }

    fn shared() -> Arc<Shared> {
        Arc::new(Shared { db: Db::open_memory().unwrap(), ollama: Ollama::new(), active: Mutex::new(None), flush_every: Duration::from_millis(40) })
    }

    fn collect() -> (Emit, Arc<Mutex<Vec<ChatEvent>>>) {
        let ev = Arc::new(Mutex::new(Vec::new()));
        let e2 = ev.clone();
        (Arc::new(move |e| e2.lock().unwrap().push(e)), ev)
    }

    fn args(text: &str) -> SendArgs {
        SendArgs { conversation_id: None, model: "m".into(), text: text.into(), attachments: vec![], num_ctx: 8192, web: false }
    }

    fn l(content: &str, thinking: &str) -> String {
        json!({"message": {"role": "assistant", "content": content, "thinking": thinking}, "done": false}).to_string()
    }
    fn done() -> String {
        json!({"message": {"role": "assistant", "content": ""}, "done": true, "eval_count": 20, "eval_duration": 2_000_000_000u64, "prompt_eval_count": 7})
            .to_string()
    }

    #[tokio::test]
    async fn streams_persists_and_reports_stats() {
        let port = mock_ollama(vec![l("", "hmm "), l("Hel", ""), l("lo ✓", ""), done()], Duration::from_millis(5), false).await;
        let sh = shared();
        let t = sh.db.begin_turn(None, "m", "hi", &[]).unwrap();
        let (emit, ev) = collect();
        run_turn(sh.clone(), emit, t.clone(), args("hi"), port, Arc::new(Notify::new())).await;

        let m = sh.db.messages(t.conversation_id).unwrap();
        assert_eq!((m[1].content.as_str(), m[1].thinking.as_str(), m[1].status.as_str()), ("Hello ✓", "hmm ", "complete"));
        assert_eq!(m[1].stats.as_ref().unwrap()["tokS"], 10.0);
        assert_eq!(m[1].stats.as_ref().unwrap()["promptEvalCount"], 7);
        let ev = ev.lock().unwrap();
        assert!(matches!(ev.last(), Some(ChatEvent::Done { status, .. }) if status == "complete"));
        let text: String = ev.iter().filter_map(|e| if let ChatEvent::Delta { content, .. } = e { Some(content.as_str()) } else { None }).collect();
        assert_eq!(text, "Hello ✓");
    }

    #[tokio::test]
    async fn request_carries_model_ctx_history_and_this_turns_web_block_only() {
        let rec = Arc::new(Mutex::new(Vec::new()));
        let port = mock_ollama_rec(vec![l("ok", ""), done()], Duration::from_millis(1), false, rec.clone()).await;
        let sh = shared();
        // first turn, then a second turn in the same conversation
        let t1 = sh.db.begin_turn(None, "m", "first question", &[]).unwrap();
        sh.db.finalize(t1.assistant_message_id, "<think>x</think>first answer", "", "complete", &json!({})).unwrap();
        let t2 = sh.db.begin_turn(Some(t1.conversation_id), "m", "second question", &[]).unwrap();
        let (emit, _) = collect();
        let mut a = args("second question");
        a.num_ctx = 16384;
        run_turn(sh.clone(), emit, t2, a, port, Arc::new(Notify::new())).await;

        let recs = rec.lock().unwrap();
        let chat = recs.iter().find(|r| r.starts_with("POST /api/chat")).expect("chat request");
        let body: Value = serde_json::from_str(chat.split_once('\n').unwrap().1).unwrap();
        assert_eq!(body["model"], "m");
        assert_eq!(body["stream"], true);
        assert_eq!(body["options"]["num_ctx"], 16384);
        let msgs = body["messages"].as_array().unwrap();
        let roles: Vec<&str> = msgs.iter().map(|m| m["role"].as_str().unwrap()).collect();
        assert_eq!(roles, ["user", "assistant", "user"]);
        assert_eq!(msgs[1]["content"], "first answer", "stored <think> is not replayed");
        assert_eq!(msgs[2]["content"], "second question", "web off: no injected text");
    }

    #[tokio::test]
    async fn draft_is_flushed_while_streaming_and_cancel_keeps_it() {
        // slow stream that never finishes; we cancel after a few flush intervals
        let lines: Vec<String> = (0..5).map(|i| l(&format!("w{i} "), "")).collect();
        let port = mock_ollama(lines, Duration::from_millis(30), true).await;
        let sh = shared();
        let t = sh.db.begin_turn(None, "m", "hi", &[]).unwrap();
        let (emit, ev) = collect();
        let cancel = Arc::new(Notify::new());
        let (sh2, c2, t2) = (sh.clone(), cancel.clone(), t.clone());
        let task = tokio::spawn(async move { run_turn(sh2, emit, t2, args("hi"), port, c2).await });

        // mid-stream the DB already holds partial text
        let seen = Arc::new(AtomicBool::new(false));
        for _ in 0..100 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let m = sh.db.messages(t.conversation_id).unwrap();
            if m[1].status == "streaming" && m[1].content.starts_with("w0") {
                seen.store(true, Ordering::SeqCst);
                break;
            }
        }
        assert!(seen.load(Ordering::SeqCst), "draft flushed to SQLite during generation");
        tokio::time::sleep(Duration::from_millis(200)).await;
        cancel.notify_one();
        task.await.unwrap();

        let m = sh.db.messages(t.conversation_id).unwrap();
        assert_eq!(m[1].status, "stopped");
        assert_eq!(m[1].content, "w0 w1 w2 w3 w4 ");
        assert!(matches!(ev.lock().unwrap().last(), Some(ChatEvent::Done { status, .. }) if status == "stopped"));
        assert!(sh.active.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn mid_stream_error_keeps_partial_text() {
        let port = mock_ollama(vec![l("partial", ""), json!({"error": "model runner crashed"}).to_string()], Duration::from_millis(5), false).await;
        let sh = shared();
        let t = sh.db.begin_turn(None, "m", "hi", &[]).unwrap();
        let (emit, ev) = collect();
        run_turn(sh.clone(), emit, t.clone(), args("hi"), port, Arc::new(Notify::new())).await;
        let m = sh.db.messages(t.conversation_id).unwrap();
        assert_eq!((m[1].content.as_str(), m[1].status.as_str()), ("partial", "error"));
        assert_eq!(m[1].stats.as_ref().unwrap()["error"], "model runner crashed");
        assert!(matches!(ev.lock().unwrap().last(), Some(ChatEvent::Error { message, .. }) if message == "model runner crashed"));
    }

    #[tokio::test]
    async fn dropped_connection_is_an_error_not_a_silent_success() {
        let port = mock_ollama(vec![l("cut ", "")], Duration::from_millis(5), false).await; // ends without done:true
        let sh = shared();
        let t = sh.db.begin_turn(None, "m", "hi", &[]).unwrap();
        let (emit, _) = collect();
        run_turn(sh.clone(), emit, t.clone(), args("hi"), port, Arc::new(Notify::new())).await;
        let m = sh.db.messages(t.conversation_id).unwrap();
        assert_eq!((m[1].content.as_str(), m[1].status.as_str()), ("cut ", "error"));
    }

    #[tokio::test]
    async fn unreachable_ollama_reports_clearly_and_keeps_the_user_message() {
        let sh = shared();
        let t = sh.db.begin_turn(None, "m", "hi", &[]).unwrap();
        let (emit, ev) = collect();
        run_turn(sh.clone(), emit, t.clone(), args("hi"), 1, Arc::new(Notify::new())).await;
        assert!(matches!(ev.lock().unwrap().last(), Some(ChatEvent::Error { message, .. }) if message.contains("Cannot reach Ollama")));
        let m = sh.db.messages(t.conversation_id).unwrap();
        assert_eq!(m[0].content, "hi");
        assert_eq!(m[1].status, "error");
    }
}
