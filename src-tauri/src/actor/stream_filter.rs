//! Streaming extractor for inline tool-call tags, with **fuzzy** matching.
//!
//! Canonical form: `<CALL:ADD_TASK>body</CALL>`. Small models drift, so we also
//! accept (case-insensitive, extra spaces, `[` instead of `<`):
//!   `[CALL: ADD_TASK]body[/CALL]`, `<call:add_task>body</call>`, etc.
//!
//! The filter emits visible text as soon as it can't be part of an open marker,
//! holds back a short tail that might still become one, suppresses everything
//! inside a tag, and yields the captured body on the closing marker. Markers
//! may be split across any number of streamed tokens.

const OPEN_SCAN_MAX: usize = 24; // bound for partial-open tail retention

#[derive(Default)]
pub struct StreamFilter {
    capturing: bool,
    capture_buf: String,
    holdback: String,
    visible: String,
}

impl StreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn visible_text(&self) -> &str {
        &self.visible
    }

    /// Feed one streamed piece. Returns `(text_to_emit, completed_call_bodies)`.
    pub fn push(&mut self, piece: &str) -> (String, Vec<String>) {
        let mut emit = String::new();
        let mut calls = Vec::new();

        if self.capturing {
            self.capture_buf.push_str(piece);
        } else {
            self.holdback.push_str(piece);
        }

        loop {
            if self.capturing {
                if let Some((cs, ce)) = find_close(self.capture_buf.as_bytes()) {
                    let body = self.capture_buf[..cs].to_string();
                    calls.push(body);
                    let rest = self.capture_buf[ce..].to_string();
                    self.capture_buf.clear();
                    self.capturing = false;
                    self.holdback = rest;
                    continue;
                }
                break; // closing marker not complete yet
            }

            match scan_open(self.holdback.as_bytes()) {
                OpenScan::Found { start, body_start } => {
                    let before = self.holdback[..start].to_string();
                    emit.push_str(&before);
                    self.visible.push_str(&before);
                    let after = self.holdback[body_start..].to_string();
                    self.holdback.clear();
                    self.capturing = true;
                    self.capture_buf = after;
                    continue;
                }
                OpenScan::Partial { start } => {
                    let flush = self.holdback[..start].to_string();
                    emit.push_str(&flush);
                    self.visible.push_str(&flush);
                    self.holdback = self.holdback[start..].to_string();
                    break;
                }
                OpenScan::None => {
                    emit.push_str(&self.holdback);
                    self.visible.push_str(&self.holdback);
                    self.holdback.clear();
                    break;
                }
            }
        }

        (emit, calls)
    }

    /// Flush remaining held-back text at end of stream (it was never a tag).
    pub fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.holdback);
        self.visible.push_str(&rest);
        rest
    }
}

const CALL: [u8; 4] = [b'c', b'a', b'l', b'l'];

enum OpenScan {
    Found { start: usize, body_start: usize },
    Partial { start: usize },
    None,
}

enum OpenTry {
    Complete(usize),
    Partial,
    No,
}

/// Try to match an opening marker `[<\[]\s*call\s*:` at byte `i`.
fn try_open(b: &[u8], i: usize) -> OpenTry {
    let mut j = i;
    if b[j] != b'<' && b[j] != b'[' {
        return OpenTry::No;
    }
    j += 1;
    while j < b.len() && b[j] == b' ' {
        j += 1;
    }
    if j >= b.len() {
        return OpenTry::Partial;
    }
    for &expected in &CALL {
        if j >= b.len() {
            return OpenTry::Partial;
        }
        if b[j].to_ascii_lowercase() != expected {
            return OpenTry::No;
        }
        j += 1;
    }
    while j < b.len() && b[j] == b' ' {
        j += 1;
    }
    if j >= b.len() {
        return OpenTry::Partial;
    }
    if b[j] != b':' {
        return OpenTry::No;
    }
    OpenTry::Complete(j + 1)
}

fn scan_open(b: &[u8]) -> OpenScan {
    for i in 0..b.len() {
        if b[i] == b'<' || b[i] == b'[' {
            match try_open(b, i) {
                OpenTry::Complete(end) => return OpenScan::Found { start: i, body_start: end },
                // A partial only happens when the buffer runs out mid-marker, so
                // nothing after it could complete first — but bound the tail.
                OpenTry::Partial => {
                    if b.len() - i <= OPEN_SCAN_MAX {
                        return OpenScan::Partial { start: i };
                    }
                    // Too long to be a marker tail: treat as plain text.
                    continue;
                }
                OpenTry::No => continue,
            }
        }
    }
    OpenScan::None
}

/// Try to match a closing marker `[<\[]\s*/?\s*call\s*[>\]]` at byte `i`.
fn try_close(b: &[u8], i: usize) -> Option<usize> {
    let mut j = i;
    if b[j] != b'<' && b[j] != b'[' {
        return None;
    }
    j += 1;
    while j < b.len() && b[j] == b' ' {
        j += 1;
    }
    if j < b.len() && b[j] == b'/' {
        j += 1;
        while j < b.len() && b[j] == b' ' {
            j += 1;
        }
    }
    for &expected in &CALL {
        if j >= b.len() || b[j].to_ascii_lowercase() != expected {
            return None;
        }
        j += 1;
    }
    while j < b.len() && b[j] == b' ' {
        j += 1;
    }
    if j < b.len() && (b[j] == b'>' || b[j] == b']') {
        return Some(j + 1);
    }
    None
}

fn find_close(b: &[u8]) -> Option<(usize, usize)> {
    for i in 0..b.len() {
        if b[i] == b'<' || b[i] == b'[' {
            if let Some(end) = try_close(b, i) {
                return Some((i, end));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(pieces: &[&str]) -> (String, Vec<String>) {
        let mut f = StreamFilter::new();
        let mut text = String::new();
        let mut calls = Vec::new();
        for p in pieces {
            let (t, c) = f.push(p);
            text.push_str(&t);
            calls.extend(c);
        }
        text.push_str(&f.finish());
        (text, calls)
    }

    #[test]
    fn canonical_split_across_tokens() {
        let (text, calls) = run(&[
            "Vou ",
            "anotar. <CA",
            "LL:ADD_TASK>Comprar pão|2026-06-20</CA",
            "LL> Pronto!",
        ]);
        assert_eq!(calls, vec!["ADD_TASK>Comprar pão|2026-06-20".to_string()]);
        assert_eq!(text, "Vou anotar.  Pronto!");
    }

    #[test]
    fn fuzzy_brackets_and_spaces() {
        let (text, calls) = run(&["ok [CALL: ADD_TASK]Comprar leite[/CALL] feito"]);
        // Body keeps the space after the colon; ToolCall::parse trims the name.
        assert_eq!(calls, vec![" ADD_TASK]Comprar leite".to_string()]);
        assert_eq!(text, "ok  feito");
        // Verify the parser still recovers the canonical command + args.
        let parsed = crate::services::ToolCall::parse(&calls[0]);
        assert_eq!(parsed.name, "ADD_TASK");
        assert_eq!(parsed.raw_args, "Comprar leite");
    }

    #[test]
    fn fuzzy_lowercase() {
        let (_text, calls) = run(&["<call:add_task>x</call>"]);
        assert_eq!(calls, vec!["add_task>x".to_string()]);
    }

    #[test]
    fn plain_text_with_lonely_brackets_passes_through() {
        let (text, calls) = run(&["if a < b && c[0] > 1 then ok"]);
        assert!(calls.is_empty());
        assert_eq!(text, "if a < b && c[0] > 1 then ok");
    }

    #[test]
    fn plain_text_passes_through() {
        let (text, calls) = run(&["olá mundo"]);
        assert!(calls.is_empty());
        assert_eq!(text, "olá mundo");
    }
}
