//! Reproduce mimo startup in a ConPTY at agent-tui pane dimensions.
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

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
        out.push(b"\x1b[?1;2c".to_vec());
    }
    if text.contains("\x1b[>c") {
        out.push(b"\x1b[>0;10;1c".to_vec());
    }
    out
}

fn probe(rows: u16, cols: u16) {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new("cmd.exe");
    cmd.args(["/K", "mimo"]);
    cmd.env_remove("NO_COLOR");
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("FORCE_COLOR", "1");
    cmd.env("COLUMNS", cols.to_string());
    cmd.env("LINES", rows.to_string());

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let mut writer = pair.master.take_writer().expect("writer");
    thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    });

    let mut reader = pair.master.try_clone_reader().expect("reader");
    let response_tx = tx.clone();
    let raw: std::sync::Arc<std::sync::RwLock<Vec<u8>>> =
        std::sync::Arc::new(std::sync::RwLock::new(Vec::new()));
    let raw_t = raw.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut r) = raw_t.write() {
                        r.extend_from_slice(&buf[..n]);
                    }
                    for reply in query_responses(&buf[..n], rows, cols) {
                        let _ = response_tx.send(reply);
                    }
                }
            }
        }
    });

    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    thread::spawn(move || {
        let _ = child.wait();
    });

    thread::sleep(Duration::from_secs(8));
    let raw = raw.read().unwrap().clone();
    let text = String::from_utf8_lossy(&raw);
    let fail = text.contains("Failed to create TextBuffer");
    println!("{rows}x{cols}: TextBuffer_fail={fail} bytes={}", raw.len());
    if fail {
        for line in text.lines().take(8) {
            println!("  {line}");
        }
    }
}

fn main() {
    for (r, c) in [(17, 58), (12, 38), (24, 80), (30, 100), (8, 24)] {
        probe(r, c);
    }
}
