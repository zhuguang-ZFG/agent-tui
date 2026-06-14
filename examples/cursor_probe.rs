use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::RwLock;
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use vt100::Parser;

fn query_responses(data: &[u8], row: u16, col: u16) -> Vec<Vec<u8>> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();
    if text.contains("\x1b[6n") {
        out.push(format!("\x1b[{row};{col}R").into_bytes());
    }
    if text.contains("\x1b[5n") {
        out.push(b"\x1b[0n".to_vec());
    }
    if text.contains("\x1b[c") {
        out.push(b"\x1b[?1;0c".to_vec());
    }
    out
}

fn probe(label: &str, mut cmd: CommandBuilder) {
    let tag = label.to_string();
    cmd.cwd(r"D:\mem0\.agents\cursor\worktree");
    cmd.env("TERM", "xterm-256color");

    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 30,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let mut writer = pair.master.take_writer().unwrap();
    thread::spawn(move || {
        while let Ok(b) = rx.recv() {
            let _ = writer.write_all(&b);
            let _ = writer.flush();
        }
    });

    let parser = Arc::new(RwLock::new(Parser::new(30, 100, 0)));
    let parser_t = parser.clone();
    let response_tx = tx.clone();
    let total_bytes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let total_reader = total_bytes.clone();
    let tag_r = tag.clone();
    let mut reader = pair.master.try_clone_reader().unwrap();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total_reader.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                    eprintln!(
                        "{tag_r} raw({n}): {:?}",
                        String::from_utf8_lossy(&buf[..n.min(120)])
                    );
                    for r in query_responses(&buf[..n], 30, 100) {
                        let _ = response_tx.send(r);
                    }
                    if let Ok(mut p) = parser_t.write() {
                        p.process(&buf[..n]);
                    }
                }
                Err(_) => break,
            }
        }
    });

    match pair.slave.spawn_command(cmd) {
        Ok(mut child) => {
            thread::spawn(move || {
                let _ = child.wait();
            });
        }
        Err(e) => {
            println!("{tag}: spawn err {e:#}");
            return;
        }
    }

    thread::sleep(Duration::from_secs(10));
    let guard = parser.read().unwrap();
    let screen = guard.screen();
    let mut lines = 0;
    for row in 0..screen.size().0 {
        let mut line = String::new();
        for col in 0..screen.size().1 {
            if let Some(c) = screen.cell(row, col) {
                line.push_str(c.contents());
            }
        }
        if !line.trim().is_empty() {
            lines += 1;
            println!("{tag} r{row}: {}", line.trim_end());
        }
    }
    if lines == 0 {
        println!(
            "{tag}: no visible output (bytes={})",
            total_bytes.load(std::sync::atomic::Ordering::Relaxed)
        );
    }
}

fn main() {
    let mut direct = portable_pty::CommandBuilder::new("node");
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        let versions = std::path::Path::new(&local).join("cursor-agent").join("versions");
        if let Ok(entries) = std::fs::read_dir(&versions) {
            for entry in entries.flatten() {
                let dir = entry.path();
                let node = dir.join("node.exe");
                let index = dir.join("index.js");
                if node.is_file() && index.is_file() {
                    direct = portable_pty::CommandBuilder::new(node.to_string_lossy().to_string());
                    direct.arg(index.to_string_lossy().to_string());
                    direct.env("CURSOR_INVOKED_AS", "agent");
                    break;
                }
            }
        }
    }
    probe("agent_node", direct);
}
