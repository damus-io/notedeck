use crate::ClientMessage;

pub(in crate::relay) type QueuedRelayFrame = (u64, ClientMessage);

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
                frames.push((generation, msg));
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
