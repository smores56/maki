//! Throwaway perf spike for the maki daemon wire protocol.
//!
//! `spike serve <sock>` — bind a UDS, replay 100 events/sec for 60s, burst 2000
//! events per 100ms window every 10s.
//!
//! `spike client <sock>` — connect, drain every 16ms, measure drain duration
//! and per-event latency (send timestamp embedded in each event via a shared
//! monotonic-clock epoch established at handshake).
//!
//! `spike spawn <sock>` — measure spawn-to-connect: fork the serve binary, time
//! from spawn to first successful connect.
//!
//! Latency uses the monotonic clock (Instant) on both sides. The two processes
//! share the same kernel monotonic clock source, so Instant deltas are directly
//! comparable across the socket. The handshake exchanges an arbitrary epoch so
//! we only need the delta.

use std::io;
use std::process::Command;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use futures_lite::AsyncWriteExt;
use serde::{Deserialize, Serialize};
use smol::io::AsyncReadExt;

const FRAME_HEADER: usize = 4;
const MAX_FRAME: u32 = 16 * 1024 * 1024;
const EVENT_INTERVAL: Duration = Duration::from_millis(10); // 100 events/sec
const RUNTIME: Duration = Duration::from_secs(60);
const BURST_EVERY: Duration = Duration::from_secs(10);
const BURST_COUNT: usize = 2000;
const BURST_WINDOW: Duration = Duration::from_millis(100);
const EVENT_TEXT_LEN: usize = 50;
const CLIENT_POLL: Duration = Duration::from_millis(16);
const SPAWN_ITERS: usize = 20;
const HANDSHAKE: &[u8] = b"hello";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpikeEvent {
    #[serde(rename = "type")]
    kind: String,
    text: String,
    seq: u64,
    /// Microseconds since the server's epoch (Instant::now() at first send).
    /// Client computes latency = (its Instant::now() - epoch) - send_us.
    send_us: u64,
}

#[derive(Parser)]
#[command(name = "spike")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Bind a UDS at <sock> and replay the perf script.
    Serve { sock: String },
    /// Connect to <sock>, drain frames for RUNTIME, print latency stats.
    Client { sock: String },
    /// Fork the serve binary, measure spawn-to-connect p50/p99.
    Spawn { sock: String },
}

fn main() {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Serve { sock } => serve(&sock),
        Cmd::Client { sock } => smol::block_on(client(&sock)),
        Cmd::Spawn { sock } => spawn_measure(&sock),
    }
}

// ---- framing ----

async fn write_frame_async<W: smol::io::AsyncWrite + Unpin>(
    w: &mut W,
    payload: &[u8],
) -> io::Result<()> {
    let len = payload.len() as u32;
    w.write_all(&len.to_le_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

async fn read_frame_async<R: smol::io::AsyncRead + Unpin>(
    r: &mut R,
) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0u8; FRAME_HEADER];
    match r.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(header);
    if len > MAX_FRAME {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame {len} exceeds max {MAX_FRAME}"),
        ));
    }
    let mut buf = vec![0u8; len as usize];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

// ---- server ----

fn serve(sock_path: &str) {
    let _ = std::fs::remove_file(sock_path);
    let listener = std::os::unix::net::UnixListener::bind(sock_path).expect("bind UDS");
    eprintln!("[serve] listening on {sock_path}");
    eprintln!(
        "[serve] {RUNTIME:?}, 100 ev/s, {BURST_COUNT}-event bursts every {BURST_EVERY:?}"
    );

    smol::block_on(async {
        let listener = smol::Async::new(listener).expect("async listener");
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    smol::spawn(handle_client(stream)).detach();
                }
                Err(e) => {
                    eprintln!("[serve] accept error: {e}");
                    break;
                }
            }
        }
    });
}

async fn handle_client(stream: smol::Async<std::os::unix::net::UnixStream>) {
    let (mut read, mut write) = futures_lite::io::split(stream);

    // Wait for handshake; ignore content.
    let _ = read_frame_async(&mut read).await;

    let epoch = Instant::now();
    let mut seq: u64 = 0;
    let mut rng = Lcg::new();
    let start = Instant::now();
    let mut next_burst = start + BURST_EVERY;

    loop {
        let now = Instant::now();
        let elapsed = now.duration_since(start);
        if elapsed >= RUNTIME {
            eprintln!("[serve] run complete after {elapsed:?}");
            return;
        }
        if now >= next_burst {
            eprintln!("[serve] burst start at {elapsed:?}");
            let burst_start = Instant::now();
            let mut sent_in_burst = 0usize;
            for _ in 0..BURST_COUNT {
                seq += 1;
                let ev = SpikeEvent {
                    kind: "text_delta".to_string(),
                    text: random_text(&mut rng, EVENT_TEXT_LEN),
                    seq,
                    send_us: epoch.elapsed().as_micros() as u64,
                };
                let payload = serde_json::to_vec(&ev).expect("encode");
                if write_frame_async(&mut write, &payload).await.is_err() {
                    eprintln!("[serve] client gone during burst");
                    return;
                }
                sent_in_burst += 1;
                if burst_start.elapsed() >= BURST_WINDOW {
                    break;
                }
            }
            eprintln!(
                "[serve] burst done, {sent_in_burst} frames in {:?}",
                burst_start.elapsed()
            );
            next_burst += BURST_EVERY;
            continue;
        }
        seq += 1;
        let ev = SpikeEvent {
            kind: "text_delta".to_string(),
            text: random_text(&mut rng, EVENT_TEXT_LEN),
            seq,
            send_us: epoch.elapsed().as_micros() as u64,
        };
        let payload = serde_json::to_vec(&ev).expect("encode");
        if write_frame_async(&mut write, &payload).await.is_err() {
            eprintln!("[serve] client gone");
            return;
        }
        smol::Timer::after(EVENT_INTERVAL).await;
    }
}

#[derive(Clone)]
struct Lcg(u64);

impl Lcg {
    fn new() -> Self {
        Self(std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x2545_f491_4f6c_dd1d)
            | 1)
    }
    fn next_u64(&mut self) -> u64 {
        // Numerical Recipes LCG constants.
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0
    }
    fn random_byte(&mut self) -> u8 {
        (self.next_u64() >> 33) as u8
    }
}

fn random_text(rng: &mut Lcg, len: usize) -> String {
    (0..len)
        .map(|_| {
            let b = (rng.random_byte() % 26) + b'a';
            char::from(b)
        })
        .collect()
}

// ---- client ----

async fn client(sock: &str) {
    println!("[client] connecting to {sock}");
    let stream = smol::Async::<std::os::unix::net::UnixStream>::connect(sock)
        .await
        .expect("connect");
    let (mut read, mut write) = futures_lite::io::split(stream);

    // Handshake. The epoch is established dynamically: the first event we
    // receive with send_us=0 is the reference; from then on use its arrival
    // instant as the client epoch counterpart.
    write_frame_async(&mut write, HANDSHAKE).await.expect("handshake");
    drop(write);

    let (tx, rx) = flume::unbounded::<(Instant, SpikeEvent)>();
    smol::spawn(async move {
        loop {
            match read_frame_async(&mut read).await {
                Ok(Some(frame)) => {
                    match serde_json::from_slice::<SpikeEvent>(&frame) {
                        Ok(ev) => {
                            let _ = tx.send((Instant::now(), ev));
                        }
                        Err(e) => {
                            eprintln!("[client] decode error: {e}");
                            return;
                        }
                    }
                }
                Ok(None) => {
                    eprintln!("[client] server closed");
                    return;
                }
                Err(e) => {
                    eprintln!("[client] read error: {e}");
                    return;
                }
            }
        }
    })
    .detach();

    let mut drain_durations: Vec<Duration> = Vec::new();
    let mut latencies: Vec<Duration> = Vec::new();
    let mut client_epoch: Option<Instant> = None;
    let mut server_epoch_us: Option<u64> = None;

    loop {
        smol::Timer::after(CLIENT_POLL).await;
        let drain_start = Instant::now();
        let mut count = 0usize;
        for (arrived, ev) in rx.drain() {
            count += 1;
            if client_epoch.is_none() {
                // First event: establish the client-side epoch counterpart.
                client_epoch = Some(arrived);
                server_epoch_us = Some(ev.send_us);
                continue;
            }
            let client_epoch = client_epoch.unwrap();
            let server_epoch_us = server_epoch_us.unwrap();
            let server_send = server_epoch_us + ev.send_us;
            let client_recv_us = arrived.duration_since(client_epoch).as_micros() as u64;
            let elapsed = client_recv_us.saturating_sub(server_send);
            latencies.push(Duration::from_micros(elapsed));
        }
        if count > 0 {
            drain_durations.push(drain_start.elapsed());
        }
        if rx.is_disconnected() && rx.is_empty() {
            break;
        }
    }

    report("drain duration (per 16ms wake)", drain_durations);
    report("event latency", latencies);

    println!("\n=== steady-state CPU ===");
    let ru = rusage_self();
    let user_us = (ru.ru_utime.tv_sec as u64 * 1_000_000) + ru.ru_utime.tv_usec as u64;
    let sys_us = (ru.ru_stime.tv_sec as u64 * 1_000_000) + ru.ru_stime.tv_usec as u64;
    let cpu_us = user_us + sys_us;
    let wall_s = RUNTIME.as_secs_f64().max(1.0);
    let core_pct = (cpu_us as f64 / 1_000_000.0) / wall_s * 100.0;
    println!("user: {:.3}s  sys: {:.3}s  total: {:.3}s", user_us as f64 / 1e6, sys_us as f64 / 1e6, cpu_us as f64 / 1e6);
    println!("wall: {:.1}s  core%: {:.3}%", wall_s, core_pct);
}

fn rusage_self() -> libc::rusage {
    use libc::{getrusage, rusage, RUSAGE_SELF};
    let mut ru: rusage = unsafe { std::mem::zeroed() };
    unsafe {
        getrusage(RUSAGE_SELF, &mut ru as *mut rusage);
    }
    ru
}

fn report(label: &str, mut samples: Vec<Duration>) {
    println!("\n=== {label} ===");
    if samples.is_empty() {
        println!("no samples");
        return;
    }
    samples.sort();
    let n = samples.len();
    println!("samples: {n}");
    println!("p50: {:?}", samples[n / 2]);
    let p99_idx = (n * 99) / 100;
    let p99_idx = if p99_idx >= n { n - 1 } else { p99_idx };
    println!("p99: {:?}", samples[p99_idx]);
    println!("max: {:?}", samples[n - 1]);
}

// ---- spawn-and-connect measurement ----

fn spawn_measure(sock: &str) {
    let exe = std::env::current_exe().expect("current_exe");
    let mut times: Vec<Duration> = Vec::with_capacity(SPAWN_ITERS);
    for i in 0..SPAWN_ITERS {
        let _ = std::fs::remove_file(sock);
        let mut cmd = Command::new(&exe);
        cmd.arg("serve").arg(sock);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::null());
        cmd.stderr(std::process::Stdio::null());
        let start = Instant::now();
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[spawn] iter {i}: spawn failed: {e}");
                continue;
            }
        };
        let mut connected = false;
        let backoffs = [10, 20, 40, 80, 160, 320, 640, 1280, 2560, 5120];
        let mut total_sleep = Duration::ZERO;
        for &b in &backoffs {
            std::thread::sleep(Duration::from_millis(b));
            total_sleep += Duration::from_millis(b);
            if std::os::unix::net::UnixStream::connect(sock).is_ok() {
                connected = true;
                break;
            }
            if total_sleep >= Duration::from_secs(3) {
                break;
            }
        }
        let elapsed = start.elapsed();
        let _ = child.kill();
        let _ = child.wait();
        if connected {
            times.push(elapsed);
        } else {
            eprintln!("[spawn] iter {i}: failed to connect after {elapsed:?}");
        }
    }
    println!("\n=== spawn-to-connect ===");
    if times.is_empty() {
        println!("no successful connections");
        return;
    }
    times.sort();
    let n = times.len();
    println!("samples: {n}/{SPAWN_ITERS}");
    println!("p50: {:?}", times[n / 2]);
    let p99_idx = (n * 99) / 100;
    let p99_idx = if p99_idx >= n { n - 1 } else { p99_idx };
    println!("p99: {:?}", times[p99_idx]);
    println!("max: {:?}", times[n - 1]);
}

