use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::mpsc;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, RwLock};
use std::thread::{self, JoinHandle};

use anyhow::{Context, Result};
use portable_pty::{native_pty_system, MasterPty, PtySize};

use crate::config::{apply_agent_tui_env, apply_pty_size, build_command, normalize_windows_path, AgentSpec};
use crate::conpty;

/// Scrollback lines kept in vt100 parser (for UI + orchestration transcript).
const SCROLLBACK_LINES: usize = 5000;
const TRANSCRIPT_MAX_BYTES: usize = 400_000;

pub struct AgentPane {
    pub spec: AgentSpec,
    parser: Arc<RwLock<vt100::Parser>>,
    /// Rolling PTY text for lead orchestration (survives screen scroll).
    transcript: Arc<Mutex<String>>,
    master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    pty_rows: Arc<AtomicU16>,
    pty_cols: Arc<AtomicU16>,
    dirty: Arc<AtomicBool>,
    input_tx: Sender<Vec<u8>>,
    _reader: JoinHandle<()>,
    _writer: JoinHandle<()>,
    _waiter: JoinHandle<()>,
}

impl AgentPane {
    pub fn spawn(
        spec: AgentSpec,
        rows: u16,
        cols: u16,
        project_dir: &Path,
        lead_agent: &str,
    ) -> Result<Self> {
        let mut cmd = build_command(&spec.command)?;
        apply_pty_size(&mut cmd, rows, cols);
        apply_agent_tui_env(&mut cmd, project_dir, &spec.name, &spec.role, lead_agent);
        let workdir = if spec.worktree.is_dir() {
            normalize_windows_path(spec.worktree.clone())
        } else {
            normalize_windows_path(project_dir.to_path_buf())
        };
        cmd.cwd(&workdir);

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("openpty")?;

        let parser = Arc::new(RwLock::new(vt100::Parser::new(rows, cols, SCROLLBACK_LINES)));
        let transcript = Arc::new(Mutex::new(String::new()));
        let dirty = Arc::new(AtomicBool::new(true));
        let pty_rows = Arc::new(AtomicU16::new(rows));
        let pty_cols = Arc::new(AtomicU16::new(cols));

        let master: Arc<Mutex<Box<dyn MasterPty + Send>>> =
            Arc::new(Mutex::new(pair.master));

        let (input_tx, input_rx) = mpsc::channel::<Vec<u8>>();

        let master_writer = master.clone();
        let writer_handle = thread::spawn(move || {
            let writer = master_writer
                .lock()
                .expect("master lock")
                .take_writer()
                .expect("pty writer");
            let mut writer = writer;
            while let Ok(bytes) = input_rx.recv() {
                let _ = writer.write_all(&bytes);
                let _ = writer.flush();
            }
        });

        let master_reader = master.clone();
        let parser_reader = parser.clone();
        let transcript_reader = transcript.clone();
        let dirty_reader = dirty.clone();
        let response_tx = input_tx.clone();
        let pty_rows_r = pty_rows.clone();
        let pty_cols_r = pty_cols.clone();

        let reader_handle = thread::spawn(move || {
            let reader = match master_reader.lock() {
                Ok(m) => m.try_clone_reader(),
                Err(_) => return,
            };
            let mut reader = match reader {
                Ok(r) => r,
                Err(_) => return,
            };
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let row = pty_rows_r.load(Ordering::Relaxed).max(1);
                        let col = pty_cols_r.load(Ordering::Relaxed).max(1);
                        for reply in conpty::query_responses(&buf[..n], row, col) {
                            let _ = response_tx.send(reply);
                        }
                        if let Ok(mut p) = parser_reader.write() {
                            p.process(&buf[..n]);
                            let snapshot = p.screen().contents();
                            drop(p);
                            if let Ok(mut t) = transcript_reader.lock() {
                                append_transcript_lines(&mut t, &snapshot);
                            }
                        }
                        dirty_reader.store(true, Ordering::Relaxed);
                    }
                    Err(_) => break,
                }
            }
        });

        let agent_name = spec.name.clone();
        let log_dir = workdir.clone();
        let slave = pair.slave;
        let waiter = thread::spawn(move || {
            match slave.spawn_command(cmd) {
                Ok(mut child) => {
                    let _ = child.wait();
                }
                Err(e) => {
                    crate::terminal::log_message(
                        &log_dir,
                        "error",
                        &format!("spawn {agent_name} failed: {e:#}"),
                    );
                }
            }
        });

        Ok(Self {
            spec,
            parser,
            transcript,
            master,
            pty_rows,
            pty_cols,
            dirty,
            input_tx,
            _reader: reader_handle,
            _writer: writer_handle,
            _waiter: waiter,
        })
    }

    pub fn with_screen<R>(&self, f: impl FnOnce(&vt100::Screen) -> R) -> R {
        let parser = self.parser.read().expect("parser lock");
        f(parser.screen())
    }

    pub fn dimensions(&self) -> (u16, u16) {
        (
            self.pty_rows.load(Ordering::Relaxed),
            self.pty_cols.load(Ordering::Relaxed),
        )
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.pty_rows.store(rows, Ordering::Relaxed);
        self.pty_cols.store(cols, Ordering::Relaxed);
        let size = PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(size);
        }
        if let Ok(mut p) = self.parser.write() {
            p.screen_mut().set_size(rows, cols);
        }
        self.dirty.store(true, Ordering::Relaxed);
    }

    pub fn mark_clean(&mut self) {
        self.dirty.store(false, Ordering::Relaxed);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty.load(Ordering::Relaxed)
    }

    pub fn visible_text(&self) -> String {
        self.with_screen(|screen| screen.contents())
    }

    /// Full rolling transcript (preferred for orchestration plan detection).
    pub fn transcript_text(&self) -> String {
        self.transcript.lock().expect("transcript lock").clone()
    }

    /// Append synthetic text (tests / probes).
    #[allow(dead_code)]
    pub fn append_transcript(&self, text: &str) {
        if let Ok(mut t) = self.transcript.lock() {
            append_transcript_lines(&mut t, text);
        }
    }

    pub fn send_input(&self, data: &[u8]) {
        let _ = self.input_tx.send(data.to_vec());
    }

    /// Inject a coordination line into the agent prompt (as if user typed + Enter).
    pub fn inject_line(&self, line: &str) {
        let mut data = line.as_bytes().to_vec();
        #[cfg(windows)]
        {
            data.extend_from_slice(b"\r\n");
        }
        #[cfg(not(windows))]
        data.push(b'\n');
        self.send_input(&data);
    }

    /// Nudge the CLI to accept/submit pending input (e.g. after delegate inject).
    pub fn wake_prompt(&self) {
        #[cfg(windows)]
        self.send_input(b"\r");
        #[cfg(not(windows))]
        self.send_input(b"\n");
    }
}

pub fn spawn_demo_shell(
    name: &str,
    rows: u16,
    cols: u16,
    project_dir: &Path,
    lead_agent: &str,
) -> Result<AgentPane> {
    let spec = AgentSpec {
        name: name.to_string(),
        command: if cfg!(windows) {
            "cmd.exe /K".into()
        } else {
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into())
        },
        role: "demo".into(),
        worktree: project_dir.to_path_buf(),
    };
    AgentPane::spawn(spec, rows, cols, project_dir, lead_agent)
}

fn append_transcript_lines(buf: &mut String, chunk: &str) {
    for line in chunk.lines() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        if buf.ends_with(line) {
            continue;
        }
        if let Some(pos) = buf.rfind('\n') {
            if buf[pos + 1..].trim() == line.trim() {
                continue;
            }
        } else if buf.trim() == line.trim() {
            continue;
        }
        if !buf.is_empty() && !buf.ends_with('\n') {
            buf.push('\n');
        }
        buf.push_str(line);
        buf.push('\n');
    }
    truncate_transcript(buf, TRANSCRIPT_MAX_BYTES);
}

/// Truncate transcript without splitting UTF-8 codepoints (avoids drain panic).
fn truncate_transcript(buf: &mut String, max_bytes: usize) {
    if buf.len() <= max_bytes {
        return;
    }
    let drain = buf.len().saturating_sub(max_bytes);
    let mut start = drain;
    while start < buf.len() && !buf.is_char_boundary(start) {
        start += 1;
    }
    buf.drain(..start);
    if let Some(pos) = buf.find('\n') {
        buf.drain(..pos + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcript_keeps_scrolled_lines() {
        let mut buf = String::new();
        append_transcript_lines(&mut buf, "line1\nline2\n");
        append_transcript_lines(&mut buf, "line2\nline3\n");
        assert!(buf.contains("line1"));
        assert!(buf.contains("line3"));
    }

    #[test]
    fn transcript_truncates_on_char_boundary() {
        let mut buf = "中文".repeat(250_000);
        buf.push('\n');
        truncate_transcript(&mut buf, 400_000);
        assert!(buf.len() <= 400_000);
        assert!(std::str::from_utf8(buf.as_bytes()).is_ok());
    }
}
