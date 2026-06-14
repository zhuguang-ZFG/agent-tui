//! Read-only Web observer (gastown-viewer style) for coordination state.

use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::claims;
use crate::config;
use crate::dead_letter::{self, DeadLetterRecord, RetryQueueEntry};
use crate::event_timeline;
use crate::mailbox::{self, MailboxEntry};
use crate::meta::{self, CoordEvent};
use crate::task_dag;
use crate::task_state::{self, TaskSnapshot};
use crate::terminal;
use crate::workflow_phase;


const DEFAULT_PORT: u16 = 8787;

pub fn observer_enabled() -> bool {
    std::env::var("AGENT_TUI_OBSERVER")
        .map(|v| v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

pub fn observer_port() -> u16 {
    std::env::var("AGENT_TUI_OBSERVER_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentSummary {
    pub name: String,
    pub role: String,
    pub active_tasks: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task: String,
    pub status: String,
    pub worker: Option<String>,
    pub lead: Option<String>,
    pub summary: Option<String>,
    pub updated: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowSummary {
    pub phase: String,
    pub title: String,
    pub detail: String,
    pub action: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObserverSnapshot {
    pub generated_at: String,
    pub project_dir: String,
    pub lead: String,
    pub workflow: WorkflowSummary,
    pub agents: Vec<AgentSummary>,
    pub tasks: Vec<TaskSummary>,
    pub mailbox: Vec<MailboxEntry>,
    pub events: Vec<CoordEvent>,
    pub dead_letters: Vec<DeadLetterRecord>,
    pub retry_queue: Vec<RetryQueueEntry>,
    pub timeline: Vec<String>,
    pub completed_tasks: Vec<String>,
    pub pending_plans: usize,
    pub conflict_tasks: Vec<String>,
}

pub fn build_snapshot(project_dir: &Path) -> Result<ObserverSnapshot> {
    let agents = config::load_agents(project_dir).unwrap_or_default();
    let lead = config::resolve_lead_agent(&agents);
    let claims = claims::load_claims_snapshot(project_dir);
    let states = task_state::load_snapshots(project_dir);
    let completed = task_dag::load_completed_tasks(project_dir);
    let pending = task_dag::load_pending_plans(project_dir);

    let agent_summaries: Vec<AgentSummary> = agents
        .iter()
        .map(|a| AgentSummary {
            name: a.name.clone(),
            role: a.role.clone(),
            active_tasks: claims
                .agent_tasks
                .get(&a.name)
                .cloned()
                .unwrap_or_default(),
        })
        .collect();

    let mut tasks: Vec<TaskSummary> = states
        .values()
        .map(task_to_summary)
        .collect();
    tasks.sort_by(|a, b| b.updated.cmp(&a.updated));

    let wf = workflow_phase::evaluate(project_dir);
    let workflow = WorkflowSummary {
        phase: workflow_phase::phase_id(&wf.phase).to_string(),
        title: wf.title,
        detail: wf.detail,
        action: wf.action,
        command: wf.command,
    };

    Ok(ObserverSnapshot {
        generated_at: meta::inbox_timestamp_iso(),
        project_dir: project_dir.display().to_string(),
        lead,
        workflow,
        agents: agent_summaries,
        mailbox: mailbox::load_entries(project_dir, 40),
        events: meta::load_coord_events(project_dir)
            .into_iter()
            .rev()
            .take(40)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect(),
        dead_letters: dead_letter::load_dead_letters(project_dir, 20),
        retry_queue: dead_letter::load_retry_queue(project_dir, 20),
        timeline: event_timeline::build_timeline_lines(project_dir, 30),
        completed_tasks: completed.into_iter().collect(),
        pending_plans: pending.len(),
        conflict_tasks: claims.conflict_tasks.into_iter().collect(),
        tasks,
    })
}

fn task_to_summary(s: &TaskSnapshot) -> TaskSummary {
    TaskSummary {
        task: s.task.clone(),
        status: s.status.clone(),
        worker: s.worker.clone(),
        lead: s.lead.clone(),
        summary: s.summary.clone(),
        updated: s.updated.clone(),
    }
}

pub fn serve_blocking(project_dir: PathBuf, bind: &str) -> Result<()> {
    let server = Server::http(bind).map_err(|e| anyhow::anyhow!("bind observer on {bind}: {e}"))?;
    let addr = server.server_addr();
    eprintln!("agent-tui observer: http://{addr}/  (Ctrl+C 停止)");
    eprintln!("  JSON API: http://{addr}/api/snapshot");
    eprintln!("  SSE:      http://{addr}/api/stream");
    run_server_loop(server, project_dir, None, None)
}

/// Background observer for TUI sessions (`AGENT_TUI_OBSERVER=1`).
pub fn spawn_background(project_dir: PathBuf) -> Result<()> {
    let port = observer_port();
    let bind = format!("127.0.0.1:{port}");
    let server = Server::http(&bind).map_err(|e| anyhow::anyhow!("bind observer on {bind}: {e}"))?;
    let addr = server.server_addr();
    terminal::log_message(
        &project_dir,
        "info",
        &format!("observer 已启动: http://{addr}/"),
    );
    thread::spawn(move || {
        let _ = run_server_loop(server, project_dir, None, None);
    });
    Ok(())
}

#[allow(clippy::explicit_counter_loop)]
fn run_server_loop(
    server: Server,
    project_dir: PathBuf,
    stop: Option<Arc<AtomicBool>>,
    max_requests: Option<usize>,
) -> Result<()> {
    let mut served = 0usize;
    for request in server.incoming_requests() {
        if stop.as_ref().is_some_and(|f| f.load(Ordering::SeqCst)) {
            break;
        }
        if let Err(e) = handle_request(&project_dir, request) {
            terminal::log_message(
                &project_dir,
                "warn",
                &format!("observer request error: {e:#}"),
            );
        }
        served += 1;
        if max_requests.is_some_and(|m| served >= m) {
            break;
        }
    }
    Ok(())
}

fn json_header() -> Header {
    Header::from_bytes("Content-Type", "application/json").expect("content-type")
}

fn html_header() -> Header {
    Header::from_bytes("Content-Type", "text/html; charset=utf-8").expect("content-type")
}

fn sse_header() -> Header {
    Header::from_bytes("Content-Type", "text/event-stream; charset=utf-8").expect("content-type")
}

fn handle_request(project_dir: &Path, request: Request) -> Result<()> {
    let path = request.url().split('?').next().unwrap_or("/").to_string();
    match (request.method(), path.as_str()) {
        (&Method::Get, "/") | (&Method::Get, "/index.html") => {
            let html = dashboard_html();
            let response = Response::from_string(html).with_header(html_header());
            request.respond(response)?;
        }
        (&Method::Get, "/api/health") => {
            let body = r#"{"ok":true,"service":"agent-tui-observer"}"#;
            let response = Response::from_string(body).with_header(json_header());
            request.respond(response)?;
        }
        (&Method::Get, "/api/snapshot") => {
            let snap = build_snapshot(project_dir)?;
            let body = serde_json::to_string_pretty(&snap)?;
            let response = Response::from_string(body).with_header(json_header());
            request.respond(response)?;
        }
        (&Method::Get, path) if path.starts_with("/api/stream") => {
            let once = request.url().contains("once=1");
            let reader = SseReader::new(
                project_dir.to_path_buf(),
                if once { Some(1) } else { None },
            );
            let response = Response::new(
                StatusCode(200),
                vec![
                    sse_header(),
                    Header::from_bytes("Cache-Control", "no-cache").expect("cache"),
                    Header::from_bytes("Connection", "keep-alive").expect("conn"),
                ],
                reader,
                None,
                None,
            );
            request.respond(response)?;
        }
        _ => {
            let response = Response::from_string("not found")
                .with_status_code(StatusCode(404));
            request.respond(response)?;
        }
    }
    Ok(())
}

fn dashboard_html() -> &'static str {
    include_str!("observer_dashboard.html")
}

fn coord_fingerprint(project_dir: &Path) -> String {
    fn line_count(path: PathBuf) -> usize {
        std::fs::read_to_string(path)
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count())
            .unwrap_or(0)
    }
    let shared = project_dir.join(".agents/shared");
    format!(
        "{}:{}:{}:{}:{}",
        line_count(shared.join("events.jsonl")),
        line_count(shared.join("mailbox.jsonl")),
        line_count(shared.join("task_state.jsonl")),
        line_count(shared.join("dead_letter.jsonl")),
        line_count(shared.join("lead_followup.jsonl")),
    )
}

fn sse_interval_ms() -> u64 {
    std::env::var("AGENT_TUI_OBSERVER_SSE_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2000)
        .clamp(500, 30_000)
}

struct SseReader {
    project_dir: PathBuf,
    last_fp: String,
    buffer: Vec<u8>,
    offset: usize,
    max_events: Option<u32>,
    events_sent: u32,
}

impl SseReader {
    fn new(project_dir: PathBuf, max_events: Option<u32>) -> Self {
        Self {
            project_dir,
            last_fp: String::new(),
            buffer: Vec::new(),
            offset: 0,
            max_events,
            events_sent: 0,
        }
    }

    fn push_snapshot(&mut self) -> io::Result<()> {
        let snap = build_snapshot(&self.project_dir)
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.last_fp = coord_fingerprint(&self.project_dir);
        let json = serde_json::to_string(&snap)
            .map_err(|e| io::Error::other(e.to_string()))?;
        self.buffer = format!("event: snapshot\ndata: {json}\n\n").into_bytes();
        self.offset = 0;
        self.events_sent += 1;
        Ok(())
    }
}

impl Read for SseReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.max_events.is_some_and(|m| self.events_sent >= m) {
            return Ok(0);
        }
        if self.offset >= self.buffer.len() {
            if self.events_sent == 0 {
                self.push_snapshot()?;
            } else {
                thread::sleep(Duration::from_millis(sse_interval_ms()));
                let fp = coord_fingerprint(&self.project_dir);
                if fp != self.last_fp {
                    self.push_snapshot()?;
                } else {
                    self.buffer = b": heartbeat\n\n".to_vec();
                    self.offset = 0;
                }
            }
        }
        if buf.is_empty() {
            return Ok(0);
        }
        let n = buf.len().min(self.buffer.len() - self.offset);
        buf[..n].copy_from_slice(&self.buffer[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}

/// Headless verify: GET /api/snapshot returns valid JSON with expected sections.
pub fn verify_http_snapshot(project_dir: &Path) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    let port = listener.local_addr()?.port();
    let server = Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!("create observer server: {e}"))?;
    let dir = project_dir.to_path_buf();
    let handle = thread::spawn(move || {
        let _ = run_server_loop(server, dir, None, Some(2));
    });
    thread::sleep(Duration::from_millis(80));

    let body = http_get_json(port, "/api/snapshot")?;
    let snap: ObserverSnapshot = serde_json::from_str(&body).context("parse snapshot JSON")?;
    if snap.agents.is_empty() {
        anyhow::bail!("observer snapshot: agents empty");
    }
    if snap.project_dir.is_empty() {
        anyhow::bail!("observer snapshot: missing project_dir");
    }
    if snap.workflow.title.is_empty() {
        anyhow::bail!("observer snapshot: missing workflow");
    }

    let health = http_get_json(port, "/api/health")?;
    if !health.contains("\"ok\":true") {
        anyhow::bail!("observer health unexpected: {health}");
    }

    let _ = handle.join();
    Ok(())
}

/// Headless verify: SSE /api/stream emits at least one snapshot event.
pub fn verify_sse_stream(project_dir: &Path) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    let port = listener.local_addr()?.port();
    let server = Server::from_listener(listener, None)
        .map_err(|e| anyhow::anyhow!("create observer server: {e}"))?;
    let dir = project_dir.to_path_buf();
    let handle = thread::spawn(move || {
        let _ = run_server_loop(server, dir, None, Some(1));
    });
    thread::sleep(Duration::from_millis(200));

    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{port}")).context("connect SSE")?;
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    write!(
        stream,
        "GET /api/stream?once=1 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).context("read SSE")?;
    let body = String::from_utf8_lossy(&buf);
    if !body.contains("event: snapshot") {
        anyhow::bail!("SSE missing event: snapshot (got {} bytes)", buf.len());
    }
    if !body.contains("\"agents\"") {
        anyhow::bail!("SSE payload missing agents JSON");
    }

    let _ = handle.join();
    Ok(())
}

fn http_get_json(port: u16, path: &str) -> Result<String> {
    let mut stream =
        TcpStream::connect(format!("127.0.0.1:{port}")).context("connect observer")?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf)?;
    let body = extract_http_body_bytes(&buf);
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn extract_http_body_bytes(buf: &[u8]) -> Vec<u8> {
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .or_else(|| {
            buf.windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| i + 2)
        })
        .unwrap_or(0);
    let headers = String::from_utf8_lossy(&buf[..header_end]).to_lowercase();
    let body = &buf[header_end..];
    if headers.contains("transfer-encoding: chunked") {
        decode_chunked_bytes(body)
    } else {
        body.to_vec()
    }
}

fn decode_chunked_bytes(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let Some(nl) = data.iter().position(|&b| b == b'\n') else {
            out.extend_from_slice(data);
            break;
        };
        let line = std::str::from_utf8(&data[..nl])
            .unwrap_or("")
            .trim_end_matches('\r');
        let hex = line.split(';').next().unwrap_or("").trim();
        let Ok(chunk_len) = usize::from_str_radix(hex, 16) else {
            out.extend_from_slice(data);
            break;
        };
        if chunk_len == 0 {
            break;
        }
        data = &data[nl + 1..];
        if data.len() < chunk_len {
            out.extend_from_slice(data);
            break;
        }
        out.extend_from_slice(&data[..chunk_len]);
        data = &data[chunk_len..];
        if data.starts_with(b"\r\n") {
            data = &data[2..];
        } else if data.starts_with(b"\n") {
            data = &data[1..];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_serializes() {
        let dir = std::env::temp_dir();
        let snap = build_snapshot(&dir).expect("snapshot");
        let json = serde_json::to_string(&snap).expect("json");
        assert!(json.contains("\"agents\""));
    }
}
