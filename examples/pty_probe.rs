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

fn probe(label: &str, program: &str, args: &[&str]) {
    let tag = label.to_string();
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new(program);
    for arg in args {
        cmd.arg(*arg);
    }

    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let mut writer = pair.master.take_writer().expect("writer");
    thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    });

    let parser = Arc::new(RwLock::new(Parser::new(24, 80, 0)));
    let parser_t = parser.clone();
    let response_tx = tx.clone();
    let mut reader = pair.master.try_clone_reader().expect("reader");
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    for reply in query_responses(&buf[..n], 24, 80) {
                        let _ = response_tx.send(reply);
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
        Ok(mut child) => thread::spawn(move || {
            let _ = child.wait();
        }),
        Err(e) => {
            println!("{tag}: spawn failed: {e:#}");
            return;
        }
    };

    thread::sleep(Duration::from_secs(3));
    let p = parser.read().unwrap();
    let screen = p.screen();
    let mut lines = 0;
    for row in 0..screen.size().0 {
        let mut row_text = String::new();
        for col in 0..screen.size().1 {
            if let Some(cell) = screen.cell(row, col) {
                row_text.push_str(cell.contents());
            }
        }
        if !row_text.trim().is_empty() {
            lines += 1;
            println!("{tag}: row{row}: {}", row_text.trim_end());
        }
    }
    if lines == 0 {
        println!("{tag}: (no visible output after 3s)");
    }
}

fn main() {
    probe("cmd_echo", "cmd.exe", &["/C", "echo PTY_OK && ver"]);
    probe("claude_ver", "claude", &["--version"]);
}
