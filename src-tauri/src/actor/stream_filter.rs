//! Streaming extractor for inline tool-call tags.
//!
//! Tags look like `<CALL:ADD_TASK>body</CALL>` and may be split across any
//! number of streamed tokens. The filter:
//!   * emits visible text as soon as it is known not to be part of an open tag,
//!   * holds back a short tail that could still be the start of `<CALL:`,
//!   * while inside a tag, suppresses output and buffers the body,
//!   * on the closing tag, yields the captured body for execution.

const OPEN: &str = "<CALL:";
const CLOSE: &str = "</CALL>";

#[derive(Default)]
pub struct StreamFilter {
    capturing: bool,
    /// Buffer of the in-progress tool-call body (after `<CALL:`).
    capture_buf: String,
    /// Normal-text tail not yet flushed (may hold a partial open tag).
    holdback: String,
    /// All visible (non-tag) text emitted so far.
    visible: String,
}

impl StreamFilter {
    pub fn new() -> Self {
        Self::default()
    }

    /// The full visible text accumulated so far (tags removed).
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
                if let Some(j) = self.capture_buf.find(CLOSE) {
                    let body = self.capture_buf[..j].to_string();
                    calls.push(body);
                    let rest = self.capture_buf[j + CLOSE.len()..].to_string();
                    self.capture_buf.clear();
                    self.capturing = false;
                    self.holdback = rest;
                    continue;
                }
                break; // closing tag not here yet
            }

            // Not capturing: look for an opening tag.
            if let Some(i) = self.holdback.find(OPEN) {
                let before = self.holdback[..i].to_string();
                emit.push_str(&before);
                self.visible.push_str(&before);

                let after = self.holdback[i + OPEN.len()..].to_string();
                self.holdback.clear();
                self.capturing = true;
                self.capture_buf = after;
                continue;
            }

            // No open tag: flush everything except a possible partial-tag tail.
            let keep = partial_open_len(&self.holdback);
            let flush_to = self.holdback.len() - keep;
            let flush = self.holdback[..flush_to].to_string();
            emit.push_str(&flush);
            self.visible.push_str(&flush);
            self.holdback = self.holdback[flush_to..].to_string();
            break;
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

/// Length (bytes) of the longest suffix of `buf` that is a proper prefix of
/// `<CALL:`, so we can hold it back in case the tag continues next token.
fn partial_open_len(buf: &str) -> usize {
    let max = (OPEN.len() - 1).min(buf.len());
    for k in (1..=max).rev() {
        let start = buf.len() - k;
        if buf.is_char_boundary(start) && OPEN.starts_with(&buf[start..]) {
            return k;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_call_split_across_tokens() {
        let mut f = StreamFilter::new();
        let mut text = String::new();
        let mut calls = Vec::new();
        for piece in ["Vou ", "anotar. <CA", "LL:ADD_TASK>Comprar pão|2026-06-20</CA", "LL> Pronto!"] {
            let (t, c) = f.push(piece);
            text.push_str(&t);
            calls.extend(c);
        }
        text.push_str(&f.finish());
        assert_eq!(calls, vec!["ADD_TASK>Comprar pão|2026-06-20".to_string()]);
        assert_eq!(text, "Vou anotar.  Pronto!");
    }

    #[test]
    fn passes_plain_text_through() {
        let mut f = StreamFilter::new();
        let (t, c) = f.push("olá mundo");
        let tail = f.finish();
        assert!(c.is_empty());
        assert_eq!(format!("{t}{tail}"), "olá mundo");
    }
}
