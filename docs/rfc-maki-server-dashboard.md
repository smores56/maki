# RFC: `maki server` (singleton daemon) + `maki dashboard` (multi-client agent view)

> Draft for upstream discussion on `tontinton/maki`.
> Author: Sam Mohr. Full design doc (rev 2) is the authoritative reference; this is the condensed version for review.

## Summary

Two features:

- **`maki server`** — a singleton per-user background daemon hosting every maki agent session. Spawned automatically the first time anything needs it (zellij-style re-exec + daemonize). Owns all session state and persistence, serves a versioned wire protocol over a Unix domain socket.
- **`maki dashboard`** — a TUI client emulating Claude Code's agent view (grouped session list, peek panel, dispatch input, full steering) in maki's design language. Any number of dashboards and chat TUIs attach to the same session and see an identical, consistent view.

This is a **full client/server split** (the opencode model): plain `maki` becomes a client of the daemon. The interactive TUI no longer hosts the agent in-process. Chosen over a local-first/lazy-tee hybrid because it gives one hosting environment, trivially correct multi-client semantics, and free zellij-style persistence — sessions live in the daemon, so "detach" is a client disconnecting and "attach mid-turn" is snapshot + subscribe. Nothing transfers between processes.

No performance loss is met by protocol/client design, not by avoiding the socket:
1. Coalesced, two-tier event delivery (lossless transcript vs droppable ephemeral output).
2. Delta-based tool output on the wire (never resend accumulated buffers).
3. The TUI paints its first frame before the connection completes; a warm daemon makes subsequent startups *faster* than today (MCP, model registry, HTTP pools already initialized).
4. Rendering stays client-side, once per 16 ms frame, exactly as today.

## Decisions (settled during design review)

| # | Decision | Choice |
|---|---|---|
| D1 | TUI hosting model | TUI is always a daemon client (no in-process agent) |
| D2 | Detach semantics | Live mid-turn attach/detach — free under D1, sessions never leave the daemon |
| D3 | Wire protocol | New versioned maki protocol (not ACP, not SDK stream-json) |
| D4 | Dashboard steering | Full agent-view parity: dispatch, peek+reply, permission answers, cancel, attach; first-answer-wins on conflicts |
| D5 | Fallback mode | Daemon-only; no embedded/standalone fallback |
| D6 | Upgrade policy | Contract-versioned side-by-side daemons; old daemon drains, never kills sessions |
| D7 | Rollout | Incremental, flag-gated (`MAKI_DAEMON=1`), flip default at parity, delete old path last |
| D8 | run_id ownership | Daemon-allocated per session; `SendInput` returns the assigned `run_id` |
| D9 | Wire evolution | Any change to a maki-wire type bumps `CONTRACT_VERSION`. No in-contract schema drift — serde enums reject unknown variants, so additive changes silently break same-contract peers |
| D10 | Crash durability | v1 ships a per-session lossless-event journal sidecar; daemon crash loses at most the journal flush interval, not the whole in-flight turn |
| D11 | Snapshot size | Snapshots are tail-first (last 200 messages + live overlay); older history via `GetMessages` paging on scroll |
| D12 | Rewind concurrency | `Rewind` rejected with `WireError::Busy` unless the session is idle; clients must `Cancel` first |
| D13 | Event size cap | Lossless events capped at `MAX_EVENT_BYTES` (4 MiB) < `MAX_FRAME` (16 MiB); oversized tool outputs truncated with `truncated: true`, so every lossless event is always deliverable |

## Architecture

```
 ┌────────────┐  ┌────────────┐  ┌─────────────┐
 │ chat TUI   │  │ dashboard  │  │ maki-acp    │  ... clients
 │ (maki)     │  │ (maki db)  │  │ (thin)      │    (any number)
 └─────┬──────┘  └─────┬──────┘  └──────┬──────┘
       │ UDS            │ UDS            │ UDS
       │ wire v{N}      │ wire v{N}      │ wire v{N}
 ┌─────┴────────────────┴────────────────┴──────┐
 │ maki server (singleton daemon, per-user)     │
 │  ┌──────────────────────────────────────────┐ │
 │  │ supervisor: roster, RPC dispatch, clients │ │
 │  │  ┌─────────────────┐ ┌─────────────────┐  │ │
 │  │  │ SessionHost     │ │ SessionHost ... │  │ │
 │  │  │  agent loop     │ │  agent loop     │  │ │
 │  │  │  pump: translate│ │  pump ...       │  │ │
 │  │  │  → fold → ring  │ │                 │  │ │
 │  │  │  → journal → persist                 │  │ │
 │  │  │  → broadcast            │ │          │  │ │
 │  │  └─────────────────┘ └─────────────────┘  │ │
 │  └────────────────────────────────────────────┘ │
 └─────────────────────┬─────────────────────────┬─┘
                       │ maki-storage (JSONL)    │ maki-wire (crate)
```

New workspace crates: `maki-wire` (types, framing, shared state fold), `maki-server` (daemon), `maki-client` (client library + dashboard). Phases 1-3 are pure addition; phase 4 ports the chat TUI behind `MAKI_DAEMON=1`.

## Performance

A throwaway perf spike (UDS echo daemon + stub client) gated the project in phase 0.4. All thresholds met on linux x86_64:

| Metric | Threshold | Measured p99 |
|---|---|---|
| Frame drain | < 1 ms | 3.37µs |
| Event latency | < 5 ms | 41µs |
| Spawn-to-connect | < 100 ms | 10.2ms |
| Client steady-state CPU | < 2% of a core | 1.118% |

D1 is not gated by performance.

## Alternatives considered

- **Local-first / lazy-tee**: keep the agent in-process, add a tee to a daemon for multi-client. Rejected: two hosting environments, divergence bugs, no clean detach mid-turn.
- **Reuse ACP**: ACP mandates full replay on `session/load` and is strictly point-to-point stdio. Wrong shape for a multi-client long-lived daemon.
- **Server-rendered ANSI (zellij)**: rejected — maki's rendering stays client-side for theme/terminal independence; zellij's upgrade fragility is a known regret.
- **Hybrid SessionHost** (in-process under a flag, daemon under another): documented retreat if perf gates fail (they didn't).

## Ask

Specifically requesting maintainer ack on:

- **(a)** Acceptance of new workspace crates `maki-wire`, `maki-server`, `maki-client`.
- **(b)** Acceptance of D1 (the chat TUI becomes a daemon client; the in-process agent path is deleted in phase 6 after a flag-gated transition).
- **(c)** Acceptance of D5 (daemon-only, no embedded standalone fallback after the transition).

Phases 1-3 are pure addition (new crates, new subcommands) and independently valuable even if the eventual TUI port (phase 4) is deferred. Nothing merges before ack of (a). Link to full design doc and turnkey per-task specs to follow.
