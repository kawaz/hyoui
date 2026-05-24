//! Observer trait and default implementations.
//!
//! An [`Observer`] sits between the parent process and the child PTY and
//! sees every byte flowing in both directions. Implementations may record,
//! transform, or simply pass the bytes through.
//!
//! The trait mirrors the MoonBit prototype's `Observer` interface
//! (`lib/observer/observer.mbt` in the bootstrap repository):
//!
//! * [`on_output`](Observer::on_output) is called for bytes coming **from**
//!   the child (PTY master read) before they are written to the parent's
//!   stdout.
//! * [`on_input`](Observer::on_input) is called for bytes coming **from**
//!   the parent (stdin read) before they are written to the PTY master.
//! * [`capture`](Observer::capture) returns an accumulated textual snapshot
//!   of what the observer has seen (used by recording / replay observers).
//!
//! The default [`NullObserver`] is a transparent pass-through that captures
//! nothing — useful as a baseline and as a stand-in when no observation is
//! requested.
//!
//! Each hook takes `&mut self` so stateful observers (loggers, recorders)
//! can accumulate data. Stateless implementations like [`NullObserver`]
//! simply ignore the receiver.

/// Trait for observing and optionally transforming the byte streams that
/// flow between the parent process and the child PTY.
///
/// All hooks return a `Vec<u8>` that is forwarded to the next stage. An
/// observer that wishes to be transparent should return the input bytes
/// unchanged (see [`NullObserver`]).
pub trait Observer {
    /// Hook called for bytes read from the child PTY before they are
    /// forwarded to the parent's stdout.
    fn on_output(&mut self, data: &[u8]) -> Vec<u8>;

    /// Hook called for bytes read from the parent's stdin before they are
    /// forwarded to the child PTY.
    fn on_input(&mut self, data: &[u8]) -> Vec<u8>;

    /// Return the observer's current capture buffer as a `String`.
    ///
    /// Observers that do not record anything (such as [`NullObserver`])
    /// return an empty string.
    fn capture(&self) -> String;
}

/// A no-op [`Observer`] that passes both streams through unchanged and
/// captures nothing.
///
/// Useful as a default when no observation behaviour is required and as a
/// baseline for tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullObserver;

impl NullObserver {
    /// Construct a new [`NullObserver`].
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Observer for NullObserver {
    fn on_output(&mut self, data: &[u8]) -> Vec<u8> {
        data.to_vec()
    }

    fn on_input(&mut self, data: &[u8]) -> Vec<u8> {
        data.to_vec()
    }

    fn capture(&self) -> String {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_observer_on_output_passes_data_through() {
        let mut obs = NullObserver::new();
        let data: &[u8] = b"hello world";
        let result = obs.on_output(data);
        assert_eq!(result, data);
    }

    #[test]
    fn null_observer_on_input_passes_data_through() {
        let mut obs = NullObserver::new();
        let data: &[u8] = b"user input";
        let result = obs.on_input(data);
        assert_eq!(result, data);
    }

    #[test]
    fn null_observer_capture_returns_empty_string() {
        let obs = NullObserver::new();
        let result = obs.capture();
        assert_eq!(result, "");
    }
}
