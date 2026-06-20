//! The chat-message envelope. A `kind = Message` event's plaintext is a framed
//! `MessageBody` (text + optional `reply_to`) rather than raw bytes, so a reply can
//! reference its parent. Backward-compatible: anything not starting with the magic
//! (or not decoding) is treated as a legacy raw-text message (`reply_to = None`).

use crate::eventlog::event::EventId;
use bincode::Options;
use serde::{Deserialize, Serialize};

/// Frames a structured body so it's distinguishable from legacy raw-text plaintext.
const MSG_MAGIC: &[u8] = b"MTB1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody {
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
}

impl MessageBody {
    pub fn new(text: Vec<u8>, reply_to: Option<EventId>) -> Self {
        MessageBody { text, reply_to }
    }

    /// `MSG_MAGIC ‖ bincode(self)` — the plaintext that gets sealed.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MSG_MAGIC.len() + 64);
        out.extend_from_slice(MSG_MAGIC);
        out.extend_from_slice(
            &bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("message body serializes"),
        );
        out
    }

    /// Recover a body from sealed-then-opened plaintext. Structured if it starts with
    /// the magic and decodes; otherwise the bytes are a legacy raw message.
    pub fn decode(bytes: &[u8]) -> MessageBody {
        if let Some(rest) = bytes.strip_prefix(MSG_MAGIC) {
            if let Ok(body) = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .reject_trailing_bytes()
                .deserialize::<MessageBody>(rest)
            {
                return body;
            }
        }
        MessageBody {
            text: bytes.to_vec(),
            reply_to: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_reply() {
        let plain = MessageBody::new(b"hello".to_vec(), None);
        assert_eq!(MessageBody::decode(&plain.encode()), plain);
        let reply = MessageBody::new(b"yes!".to_vec(), Some(EventId::new([7u8; 32])));
        assert_eq!(MessageBody::decode(&reply.encode()), reply);
    }

    #[test]
    fn legacy_raw_text_decodes_as_textonly() {
        // A pre-envelope message (raw UTF-8, no magic) still opens.
        let body = MessageBody::decode(b"just raw text");
        assert_eq!(body.text, b"just raw text");
        assert_eq!(body.reply_to, None);
    }

    #[test]
    fn text_that_starts_like_magic_but_isnt_a_body_falls_back() {
        // Bytes beginning with the magic but not a valid body are treated as raw.
        let mut bytes = MSG_MAGIC.to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFF]); // not a valid MessageBody encoding
        let body = MessageBody::decode(&bytes);
        assert_eq!(body.text, bytes);
        assert_eq!(body.reply_to, None);
    }
}
