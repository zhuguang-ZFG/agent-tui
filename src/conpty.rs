//! ConPTY on Windows emits terminal queries that must be answered on the PTY
//! master write side, otherwise child processes hang with no visible output.

pub fn query_responses(data: &[u8], row: u16, col: u16) -> Vec<Vec<u8>> {
    let text = String::from_utf8_lossy(data);
    let mut out = Vec::new();

    if text.contains("\x1b[6n") {
        out.push(format!("\x1b[{row};{col}R").into_bytes());
    }
    if text.contains("\x1b[5n") {
        out.push(b"\x1b[0n".to_vec());
    }
    if text.contains("\x1b[c") {
        // VT100 + ANSI color (so chalk/picocolors enable 256/truecolor with our env).
        out.push(b"\x1b[?1;2c".to_vec());
    }
    if text.contains("\x1b[>c") {
        out.push(b"\x1b[>0;10;1c".to_vec());
    }
    if text.contains("\x1b[18t") {
        out.push(format!("\x1b[8;{row};{col}t").into_bytes());
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responds_to_dsr() {
        let replies = query_responses(b"\x1b[6n", 24, 80);
        assert_eq!(replies, vec![b"\x1b[24;80R".to_vec()]);
    }
}
