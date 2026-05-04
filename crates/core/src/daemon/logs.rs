// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Myggiz AB

//! In-memory ring-buffer log layer + redacted IPC stream.
//!
//! ≤ 5000 records held in memory; older records evicted FIFO. Each
//! record passes through a redactor that strips bare 64-char hex blobs
//! (pubkeys), `*.onion` strings, and message-body interpolations
//! ABOVE the `debug` level.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::sync::broadcast;
use tracing::Subscriber;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use tracing_subscriber::Layer;

use crate::daemon::commands::{LogLevel, LogRecord};

const RING_CAP: usize = 5_000;
const STREAM_CAP: usize = 256;

/// Shared handle to the ring buffer and the broadcast channel.
#[derive(Clone)]
pub struct LogSink {
    inner: Arc<LogSinkInner>,
}

struct LogSinkInner {
    ring: Mutex<VecDeque<LogRecord>>,
    next_seq: AtomicU64,
    tx: broadcast::Sender<LogRecord>,
}

impl LogSink {
    /// Create a new `LogSink` with an empty ring buffer.
    pub fn new() -> Self {
        let (tx, _rx) = broadcast::channel(STREAM_CAP);
        Self {
            inner: Arc::new(LogSinkInner {
                ring: Mutex::new(VecDeque::with_capacity(RING_CAP)),
                next_seq: AtomicU64::new(1),
                tx,
            }),
        }
    }

    /// Subscribe to live log records. Lossy (broadcast::Receiver).
    pub fn subscribe(&self) -> broadcast::Receiver<LogRecord> {
        self.inner.tx.subscribe()
    }

    /// Snapshot of records with `seq >= since_seq` (inclusive),
    /// capped at `limit` (also internally clamped to ≤ 1000).
    pub fn snapshot(&self, since_seq: Option<u64>, limit: usize) -> (Vec<LogRecord>, u64) {
        let limit = limit.min(1000);
        let ring = self.inner.ring.lock();
        let start = since_seq.unwrap_or(0);
        let records: Vec<LogRecord> = ring
            .iter()
            .filter(|r| r.seq >= start)
            .take(limit)
            .cloned()
            .collect();
        let next = records.last().map(|r| r.seq + 1).unwrap_or(start);
        (records, next)
    }

    /// Push a record into the ring + broadcast it. Public for tests;
    /// production callers go through the `RingBufferLayer`.
    pub fn push(&self, level: LogLevel, target: String, message: String) {
        let seq = self.inner.next_seq.fetch_add(1, Ordering::SeqCst);
        let rec = LogRecord {
            seq,
            ts_unix_ms: now_unix_ms(),
            level,
            target,
            message,
        };
        let mut ring = self.inner.ring.lock();
        if ring.len() == RING_CAP {
            ring.pop_front();
        }
        ring.push_back(rec.clone());
        drop(ring);
        let _ = self.inner.tx.send(rec); // ignore "no receivers"
    }
}

impl Default for LogSink {
    fn default() -> Self {
        Self::new()
    }
}

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// `tracing-subscriber` layer that funnels events into a `LogSink`,
/// applying redaction above the `debug` level.
pub struct RingBufferLayer {
    sink: LogSink,
    redactor: Redactor,
}

impl RingBufferLayer {
    /// Create a new `RingBufferLayer` that funnels tracing events into `sink`.
    pub fn new(sink: LogSink) -> Self {
        Self {
            sink,
            redactor: Redactor::new(),
        }
    }
}

impl<S> Layer<S> for RingBufferLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = match *metadata.level() {
            tracing::Level::TRACE => LogLevel::Trace,
            tracing::Level::DEBUG => LogLevel::Debug,
            tracing::Level::INFO => LogLevel::Info,
            tracing::Level::WARN => LogLevel::Warn,
            tracing::Level::ERROR => LogLevel::Error,
        };

        // Format the event into a string. Use a Visit impl that
        // concatenates field key=value pairs.
        let mut formatted = String::new();
        let mut visitor = StringVisitor(&mut formatted);
        event.record(&mut visitor);

        // Redact above debug.
        let message = if matches!(level, LogLevel::Trace | LogLevel::Debug) {
            formatted
        } else {
            self.redactor.redact(&formatted)
        };

        self.sink
            .push(level, metadata.target().to_string(), message);
    }
}

struct StringVisitor<'a>(&'a mut String);

impl tracing::field::Visit for StringVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        let _ = std::fmt::Write::write_fmt(self.0, format_args!("{}={:?}", field.name(), value));
    }
}

/// Redactor that strips:
///   - 64-char lowercase hex strings (Ed25519 / X25519 pubkeys, BLAKE2s hashes)
///   - 56-char base32 + ".onion" suffixes (v3 onion addresses)
///   - any field starting with "body=" / "ciphertext=" / "psk=" / "passphrase=" / "seed="
struct Redactor;

static RE_HEX64: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static RE_ONION: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
static RE_SECRET: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

fn re_hex64() -> &'static regex::Regex {
    RE_HEX64.get_or_init(|| {
        // SAFETY: pattern is a compile-time constant and has been verified correct.
        #[allow(clippy::expect_used)]
        regex::Regex::new(r"\b[0-9a-f]{64}\b").expect("hardcoded regex is valid")
    })
}

fn re_onion() -> &'static regex::Regex {
    RE_ONION.get_or_init(|| {
        // SAFETY: pattern is a compile-time constant and has been verified correct.
        #[allow(clippy::expect_used)]
        regex::Regex::new(r"\b[a-z2-7]{56}\.onion\b").expect("hardcoded regex is valid")
    })
}

fn re_secret() -> &'static regex::Regex {
    RE_SECRET.get_or_init(|| {
        // SAFETY: pattern is a compile-time constant and has been verified correct.
        #[allow(clippy::expect_used)]
        regex::Regex::new(r"\b(body|ciphertext|psk|passphrase|seed)=\S+")
            .expect("hardcoded regex is valid")
    })
}

impl Redactor {
    fn new() -> Self {
        // Pre-initialise the statics so they are ready on first event.
        let _ = re_hex64();
        let _ = re_onion();
        let _ = re_secret();
        Self
    }

    fn redact(&self, s: &str) -> String {
        let s = re_hex64().replace_all(s, "[REDACTED-PUBKEY]");
        let s = re_onion().replace_all(&s, "[REDACTED-ONION]");
        let s = re_secret().replace_all(&s, "$1=[REDACTED]");
        s.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_evicts_oldest_when_full() {
        let sink = LogSink::new();
        for i in 0..(RING_CAP + 10) {
            sink.push(LogLevel::Info, "test".into(), format!("msg-{i}"));
        }
        let (records, _) = sink.snapshot(None, 1000);
        // Oldest in buffer should be msg-10 (first 10 evicted).
        assert!(records.iter().any(|r| r.message == "msg-10"));
        assert!(!records.iter().any(|r| r.message == "msg-0"));
    }

    #[test]
    fn snapshot_respects_since_seq_and_limit() {
        let sink = LogSink::new();
        for i in 0..100 {
            sink.push(LogLevel::Info, "t".into(), format!("m-{i}"));
        }
        let (records, next) = sink.snapshot(Some(50), 10);
        assert_eq!(records.len(), 10);
        assert_eq!(records[0].seq, 50);
        assert_eq!(next, 60);
    }

    #[test]
    fn redactor_strips_hex_pubkey_and_onion() {
        let r = Redactor::new();
        let pubkey = "abcd1234".repeat(8); // 64 hex chars
        let onion = format!("{}.onion", "a".repeat(56));
        let input = format!("peer {pubkey} at {onion}");
        let out = r.redact(&input);
        assert!(out.contains("[REDACTED-PUBKEY]"));
        assert!(out.contains("[REDACTED-ONION]"));
        assert!(!out.contains(&pubkey));
        assert!(!out.contains(&onion));
    }

    #[test]
    fn redactor_strips_secret_fields() {
        let r = Redactor::new();
        let out = r.redact("body=hello psk=00112233 ok=true");
        assert!(out.contains("body=[REDACTED]"));
        assert!(out.contains("psk=[REDACTED]"));
        assert!(out.contains("ok=true"));
    }

    #[test]
    fn redactor_leaves_short_hex_alone() {
        let r = Redactor::new();
        let out = r.redact("id=abc123"); // not 64 chars
        assert!(out.contains("abc123"));
    }

    #[test]
    fn ring_buffer_layer_redacts_pubkeys_at_info() {
        use tracing_subscriber::layer::SubscriberExt;
        let sink = LogSink::new();
        let layer = RingBufferLayer::new(sink.clone());
        let subscriber = tracing_subscriber::registry().with(layer);
        let _g = tracing::subscriber::set_default(subscriber);

        let pubkey = "0".repeat(64);
        tracing::info!(peer = %pubkey, "received message");

        let (records, _) = sink.snapshot(None, 100);
        #[allow(clippy::expect_used)]
        let info_record = records
            .iter()
            .find(|r| matches!(r.level, LogLevel::Info))
            .expect("at least one Info record");
        assert!(
            !info_record.message.contains(&pubkey),
            "info-level pubkey must be redacted, got: {}",
            info_record.message
        );
        assert!(info_record.message.contains("[REDACTED-PUBKEY]"));
    }

    #[test]
    fn debug_record_keeps_pubkey_intact() {
        use tracing_subscriber::layer::SubscriberExt;
        let sink = LogSink::new();
        let layer = RingBufferLayer::new(sink.clone());
        let filter = tracing_subscriber::EnvFilter::new("trace");
        let subscriber = tracing_subscriber::registry().with(filter).with(layer);
        let _g = tracing::subscriber::set_default(subscriber);

        let pubkey = "a".repeat(64);
        tracing::debug!(peer = %pubkey, "fine-grained");

        let (records, _) = sink.snapshot(None, 100);
        #[allow(clippy::expect_used)]
        let debug_record = records
            .iter()
            .find(|r| matches!(r.level, LogLevel::Debug))
            .expect("at least one Debug record");
        assert!(
            debug_record.message.contains(&pubkey),
            "debug-level pubkey must NOT be redacted, got: {}",
            debug_record.message
        );
    }
}
