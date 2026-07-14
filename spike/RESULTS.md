# Phase 0.4 — UDS perf spike results

Throwaway spike (never merged). Branch: `smores/maki-server-phase0`, dir `spike/`.
Machine: linux x86_64, smol 2 / futures-lite 2, std `UnixListener` wrapped in `smol::Async`.

## Protocol

- `spike serve <sock>`: 100 events/sec for 60s, JSON length-prefixed framing (4-byte LE);
  every 10s a 2000-event burst packed into a 100ms window.
- `spike client <sock>`: reader thread into a flume channel; main loop wakes every 16ms,
  drains, records per-wake drain duration and per-event latency (send timestamp embedded
  in each event as micros since the server epoch; client establishes epoch on first frame).
- `spike spawn <sock>`: fork the serve binary, poll connect with 10/20/40…ms backoff up to 3s,
  20 iterations.
- CPU via `getrusage(RUSAGE_SELF)` over the 60s run.

## Thresholds (design §10) vs measured

| Metric | Threshold | Measured p99 | Pass |
|---|---|---|---|
| Frame drain | < 1 ms | 3.37µs (max 195µs) | yes |
| Event latency | < 5 ms | 41µs (max 1.36ms, during burst) | yes |
| Spawn-to-connect | < 100 ms | 10.2ms | yes |
| Client steady-state CPU | < 2% of a core | 1.118% (0.671s CPU / 60s wall) | yes |

All thresholds met. D1 (TUI becomes a daemon client) is not gated by performance.

## Notes

- Drain p99 is microsecond-scale because the client only wakes every 16ms; the drain itself
  is a channel drain, not I/O — actual frame read latency is captured by the event-latency
  metric instead.
- Event latency max (1.36ms) occurs during the 2000-event burst where the reader thread and
  main loop contend; still 4x under the threshold.
- Spawn metric is lower-bounded by the 10ms first-sleep backoff; real spawn-to-connect is
  faster, comfortably under 100ms.
- Latency uses the monotonic clock (`Instant`), which is comparable across processes on the
  same host; the handshake passes an arbitrary marker so only the delta matters.
