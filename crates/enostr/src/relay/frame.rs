use crate::ClientMessage;

/// A client message queued for one websocket connection generation, so it is
/// dropped rather than sent if the socket reconnects before it is flushed.
#[derive(Debug)]
pub(in crate::relay) struct QueuedRelayFrame {
    pub(in crate::relay) generation: u64,
    pub(in crate::relay) message: ClientMessage,
}

/// A relay-local step's result beside the frames it queued producing it.
pub(in crate::relay) struct Framed<T> {
    pub(in crate::relay) value: T,
    pub(in crate::relay) frames: Vec<QueuedRelayFrame>,
}

/// Relay-frame target used by relay-local protocol engines.
pub(in crate::relay) enum RelayFrameSink {
    Disconnected,
    Transport {
        current_generation: Option<u64>,
        frames: Vec<QueuedRelayFrame>,
    },
}

impl RelayFrameSink {
    pub(in crate::relay) fn transport(current_generation: Option<u64>) -> Self {
        Self::Transport {
            current_generation,
            frames: Vec::new(),
        }
    }

    pub(in crate::relay) fn disconnected() -> Self {
        Self::Disconnected
    }

    pub(in crate::relay) fn send(&mut self, msg: ClientMessage) -> Option<u64> {
        match self {
            Self::Disconnected => None,
            Self::Transport {
                current_generation,
                frames,
            } => {
                let generation = (*current_generation)?;
                frames.push(QueuedRelayFrame {
                    generation,
                    message: msg,
                });
                Some(generation)
            }
        }
    }

    pub(in crate::relay) fn into_frames(self) -> Vec<QueuedRelayFrame> {
        match self {
            Self::Disconnected => Vec::new(),
            Self::Transport { frames, .. } => frames,
        }
    }
}
