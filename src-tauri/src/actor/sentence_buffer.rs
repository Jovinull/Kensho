//! Accumulates streamed tokens and emits complete sentences (split on `.?!`),
//! so TTS can start speaking before the full response is generated.

#[derive(Default)]
pub struct SentenceBuffer {
    buf: String,
}

impl SentenceBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append streamed text; return any newly-completed sentences.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.buf.push_str(text);
        let mut out = Vec::new();
        while let Some(end) = find_sentence_end(&self.buf) {
            let sentence = self.buf[..end].trim().to_string();
            self.buf = self.buf[end..].trim_start().to_string();
            if !sentence.is_empty() {
                out.push(sentence);
            }
        }
        out
    }

    /// Return any trailing text not terminated by punctuation.
    pub fn flush(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf).trim().to_string();
        if rest.is_empty() {
            None
        } else {
            Some(rest)
        }
    }
}

/// Byte index just past the first sentence terminator (`.`, `?`, `!`).
fn find_sentence_end(s: &str) -> Option<usize> {
    s.char_indices()
        .find(|(_, c)| matches!(c, '.' | '?' | '!'))
        .map(|(i, c)| i + c.len_utf8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_sentences_across_pushes() {
        let mut b = SentenceBuffer::new();
        assert!(b.push("Olá").is_empty()); // no terminator yet
        let s = b.push(", tudo bem? Vou ajudar.");
        assert_eq!(s, vec!["Olá, tudo bem?".to_string(), "Vou ajudar.".to_string()]);
        assert!(b.flush().is_none());
    }

    #[test]
    fn flush_returns_unterminated_tail() {
        let mut b = SentenceBuffer::new();
        assert!(b.push("Resposta sem ponto").is_empty());
        assert_eq!(b.flush(), Some("Resposta sem ponto".to_string()));
    }

    #[test]
    fn handles_exclamation_and_question() {
        let mut b = SentenceBuffer::new();
        let s = b.push("Pronto! E agora?");
        assert_eq!(s, vec!["Pronto!".to_string(), "E agora?".to_string()]);
    }
}
