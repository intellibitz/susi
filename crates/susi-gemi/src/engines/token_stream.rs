//! Incremental text decoding shared by standard and speculative generation.
use crate::susi_error::{EaiError, EaiResult};
use tokenizers::Tokenizer;

pub(crate) struct TokenStream<'a> {
    tokenizer: &'a Tokenizer,
    ids: Vec<u32>,
    prefix: String,
    prefix_index: usize,
    emitted_bytes: usize,
}

impl<'a> TokenStream<'a> {
    pub(crate) fn new(tokenizer: &'a Tokenizer) -> Self {
        Self {
            tokenizer,
            ids: Vec::new(),
            prefix: String::new(),
            prefix_index: 0,
            emitted_bytes: 0,
        }
    }

    pub(crate) fn push(&mut self, token: u32, callback: &dyn Fn(String)) -> EaiResult<()> {
        let piece = tokenizers::tokenizer::step_decode_stream(
            self.tokenizer,
            vec![token],
            true,
            &mut self.ids,
            &mut self.prefix,
            &mut self.prefix_index,
        )
        .map_err(|e| EaiError::inference(format!("Streaming decode failed: {e}")))?;
        if let Some(piece) = piece {
            self.emitted_bytes += piece.len();
            callback(piece);
        }
        Ok(())
    }

    /// Flush a trailing incomplete byte sequence using the same replacement
    /// behavior as the final full decode, including generation-budget exits.
    pub(crate) fn finish(&self, output: &str, callback: &dyn Fn(String)) {
        if let Some(rest) = output.get(self.emitted_bytes..) {
            if !rest.is_empty() {
                callback(rest.to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use tokenizers::{decoders::byte_fallback::ByteFallback, models::bpe::BPE};

    fn tokenizer() -> Tokenizer {
        let vocab = [
            ("<0xC3>".to_owned(), 0),
            ("<0xA9>".to_owned(), 1),
            ("!".to_owned(), 2),
        ];
        let model = BPE::builder()
            .vocab_and_merges(vocab, vec![])
            .byte_fallback(true)
            .build()
            .unwrap();
        let mut tokenizer = Tokenizer::new(model);
        tokenizer.with_decoder(Some(ByteFallback::default()));
        tokenizer
    }

    #[test]
    fn byte_fragments_are_buffered_until_character_is_complete() {
        let tokenizer = tokenizer();
        let mut stream = TokenStream::new(&tokenizer);
        let output = RefCell::new(String::new());
        let callback = |piece: String| output.borrow_mut().push_str(&piece);
        stream.push(0, &callback).unwrap();
        assert!(output.borrow().is_empty());
        stream.push(1, &callback).unwrap();
        stream.push(2, &callback).unwrap();
        let final_output = tokenizer.decode(&[0, 1, 2], true).unwrap();
        stream.finish(&final_output, &callback);
        assert_eq!(*output.borrow(), "é!");
        assert_eq!(*output.borrow(), final_output);
    }

    #[test]
    fn budget_exit_flushes_incomplete_bytes_consistently() {
        let tokenizer = tokenizer();
        let mut stream = TokenStream::new(&tokenizer);
        let output = RefCell::new(String::new());
        let callback = |piece: String| output.borrow_mut().push_str(&piece);
        stream.push(0, &callback).unwrap();
        let final_output = tokenizer.decode(&[0], true).unwrap();
        stream.finish(&final_output, &callback);
        assert_eq!(*output.borrow(), final_output);
        assert!(!output.borrow().is_empty());
    }
}
