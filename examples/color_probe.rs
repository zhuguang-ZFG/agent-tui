//! Verify ConPTY passthrough preserves truecolor through vt100.
use std::io::{Read, Write};
use std::sync::{Arc, RwLock};
use std::sync::mpsc;
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

fn main() {
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows: 8,
            cols: 60,
            pixel_width: 0,
            pixel_height: 0,
        })
        .expect("openpty");

    let mut cmd = CommandBuilder::new("node");
    cmd.args([
        "-e",
        r#"process.stdout.write('\x1b[38;2;255;80;40morange\x1b[0m \x1b[38;2;120;200;255msky\x1b[0m\n')"#,
    ]);
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM", "xterm-256color");

    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let mut writer = pair.master.take_writer().expect("writer");
    thread::spawn(move || {
        while let Ok(bytes) = rx.recv() {
            let _ = writer.write_all(&bytes);
            let _ = writer.flush();
        }
    });

    let parser = Arc::new(RwLock::new(Parser::new(8, 60, 0)));
    let parser_t = parser.clone();
    let response_tx = tx.clone();
    let mut reader = pair.master.try_clone_reader().expect("reader");
    let raw: Arc<RwLock<Vec<u8>>> = Arc::new(RwLock::new(Vec::new()));
    let raw_t = raw.clone();
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut r) = raw_t.write() {
                        r.extend_from_slice(&buf[..n]);
                    }
                    for reply in query_responses(&buf[..n], 8, 60) {
                        let _ = response_tx.send(reply);
                    }
                    if let Ok(mut p) = parser_t.write() {
                        p.process(&buf[..n]);
                    }
                }
            }
        }
    });

    let mut child = pair.slave.spawn_command(cmd).expect("spawn");
    thread::spawn(move || {
        let _ = child.wait();
    });

    thread::sleep(Duration::from_secs(2));

    let screen = parser.read().unwrap().screen().clone();
    let mut rgb_count = 0usize;
    let mut indexed_count = 0usize;
    for row in 0..screen.size().0 {
        for col in 0..screen.size().1 {
            if let Some(cell) = screen.cell(row, col) {
                match cell.fgcolor() {
                    vt100::Color::Rgb(_, _, _) => rgb_count += 1,
                    vt100::Color::Idx(_) => indexed_count += 1,
                    _ => {}
                }
            }
        }
    }

    let raw = raw.read().unwrap().clone();
    println!("raw bytes: {}", raw.len());
    if !raw.is_empty() {
        let preview = String::from_utf8_lossy(&raw[..raw.len().min(200)]);
        println!("raw preview: {preview:?}");
    }
    println!("fg RGB cells: {rgb_count}");
    println!("fg indexed cells: {indexed_count}");
    if rgb_count > 0 {
        println!("PASS: truecolor preserved through ConPTY");
    } else {
        println!("FAIL: no RGB cells");
        std::process::exit(1);
    }
}
