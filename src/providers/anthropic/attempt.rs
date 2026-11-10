//! Hold only an uncommitted attempt's thinking; never replay visible output or calls.
use roomkit_ai::{ProviderError, StreamDone, StreamEvent};

const HELD_BYTES: usize = 4 * 1024 * 1024;

#[derive(Default)]
pub(super) struct Attempt {
    pub(super) committed: bool,
    pub(super) error: Option<ProviderError>,
    pub(super) done: Option<StreamDone>,
    held: Vec<StreamEvent>,
    bytes: usize,
}

impl Attempt {
    pub(super) fn accept(&mut self, event: Result<StreamEvent, ProviderError>) -> Vec<StreamEvent> {
        let mut event = match event {
            Ok(StreamEvent::Done(done)) => {
                self.done = Some(done);
                return Vec::new();
            }
            Err(error) => {
                self.error = Some(error);
                return Vec::new();
            }
            Ok(event) => event,
        };
        if let StreamEvent::ThinkingDelta(delta) = &event {
            self.bytes = self
                .bytes
                .saturating_add(delta.thinking.len())
                .saturating_add(delta.signature.as_ref().map_or(0, String::len))
                .saturating_add(delta.redacted.as_ref().map_or(0, String::len));
            if !self.committed && self.bytes < HELD_BYTES {
                self.hold(event);
                return Vec::new();
            }
        }
        if let StreamEvent::ToolCall(call) = &mut event {
            super::replay::keep_input(call);
        }
        self.committed = true;
        let mut ready = std::mem::take(&mut self.held);
        ready.push(event);
        ready
    }

    pub(super) fn failed(&self) -> bool {
        self.error.is_some()
    }

    fn hold(&mut self, event: StreamEvent) {
        if let (Some(StreamEvent::ThinkingDelta(previous)), StreamEvent::ThinkingDelta(next)) =
            (self.held.last_mut(), &event)
            && previous.block == next.block
        {
            previous.thinking.push_str(&next.thinking);
            if next
                .signature
                .as_ref()
                .is_some_and(|signature| !signature.is_empty())
            {
                previous.signature.clone_from(&next.signature);
            }
            if next.redacted.is_some() {
                previous.redacted.clone_from(&next.redacted);
            }
            return;
        }
        // Count empty blocks too, without depending on SSE fragment boundaries.
        self.bytes = self
            .bytes
            .saturating_add(std::mem::size_of::<StreamEvent>());
        self.held.push(event);
    }

    pub(super) fn exhausted(&self) -> bool {
        !self.committed
            && self
                .done
                .as_ref()
                .is_some_and(|d| d.finish_reason.as_deref() == Some("max_tokens"))
    }

    pub(super) fn finish(mut self) -> Vec<StreamEvent> {
        if let Some(done) = self.done.take() {
            self.held.push(StreamEvent::Done(done));
        }
        self.held
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use roomkit_ai::StreamThinkingDelta;

    #[test]
    fn fragmented_reasoning_does_not_disable_the_budget_retry() {
        let mut attempt = Attempt::default();
        for _ in 0..5000 {
            assert!(
                attempt
                    .accept(Ok(StreamEvent::ThinkingDelta(StreamThinkingDelta {
                        block: Some(0),
                        ..StreamThinkingDelta::new("a")
                    })))
                    .is_empty()
            );
        }
        attempt.accept(Ok(StreamEvent::Done(StreamDone {
            finish_reason: Some("max_tokens".into()),
            ..StreamDone::default()
        })));
        assert!(attempt.exhausted());
        assert_eq!(attempt.held.len(), 1);
        assert!(
            matches!(&attempt.held[0], StreamEvent::ThinkingDelta(d) if d.thinking.len() == 5000)
        );
    }

    #[test]
    fn an_empty_signature_fragment_preserves_the_last_nonempty_signature() {
        let mut attempt = Attempt::default();
        for signature in ["signed", ""] {
            attempt.accept(Ok(StreamEvent::ThinkingDelta(StreamThinkingDelta {
                block: Some(0),
                signature: Some(signature.into()),
                ..StreamThinkingDelta::new("")
            })));
        }
        assert!(
            matches!(&attempt.held[0], StreamEvent::ThinkingDelta(d) if d.signature.as_deref() == Some("signed"))
        );
    }
}
