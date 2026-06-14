#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthState {
    Ok,
    Warn(String),
    Dead(String),
}

const FATAL: &[&str] = &[
    "Failed to create TextBuffer",
    "TextBuffer is destroyed",
    "A fatal error occurred!",
    "error.OutOfMemory",
    "An error occurred in Effect.tryPromise",
    "FATAL ERROR",
];

const WARN: &[&str] = &[
    "setup issues",
    "Auto-update failed",
    "spawn failed",
    "--trust can only be used",
];

pub fn scan_screen(text: &str) -> HealthState {
    let lower = text.to_lowercase();
    for pat in FATAL {
        if lower.contains(&pat.to_lowercase()) {
            return HealthState::Dead((*pat).into());
        }
    }
    for pat in WARN {
        if lower.contains(&pat.to_lowercase()) {
            return HealthState::Warn((*pat).into());
        }
    }
    HealthState::Ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_mimo_textbuffer_fatal() {
        let state = scan_screen("Error: Failed to create TextBuffer\n");
        assert!(matches!(state, HealthState::Dead(_)));
    }

    #[test]
    fn detects_claude_warn() {
        let state = scan_screen("9 setup issues: MCP");
        assert!(matches!(state, HealthState::Warn(_)));
    }

    #[test]
    fn detects_oom_fatal() {
        let state = scan_screen("Failed to create renderer: error.OutOfMemory");
        assert!(matches!(state, HealthState::Dead(_)));
    }

    #[test]
    fn detects_effect_crash_fatal() {
        let state = scan_screen("An error occurred in Effect.tryPromise");
        assert!(matches!(state, HealthState::Dead(_)));
    }
}
