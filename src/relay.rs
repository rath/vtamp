//! Relay mode: a local server that forwards commands to a remote vtamp server
//! and plays that server's cast through the local audio device.
//!
//! The TUI and the CLI attach to the local socket exactly as they would to a
//! playback server, so music keeps playing after they exit. Playback state
//! lives on the remote. The relay owns only the local output, the spectrum of
//! what is audible here, and the position correction that accounts for the
//! audio buffered between the two machines.
use crate::{
    audio::{PlaybackBackend, Progress, RodioBackend},
    cast::source::{StreamStart, read_streams},
    daemon::{self, Bind},
    model::*,
    platform::Paths,
    spectrum::Spectrum,
    wire,
};
use anyhow::{Context, Result, bail, ensure};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream as StdUnixStream,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    net::UnixStream,
    sync::{Notify, Semaphore},
};

const RETRY: Duration = Duration::from_secs(2);
const OUTPUT_RETRY: Duration = Duration::from_secs(5);

enum Local {
    Stream(Box<StreamStart>),
    Volume(u8),
    State(Box<State>),
    Shutdown,
}

/// What the local output plays: enough to translate the remote position into
/// what is audible here.
struct Audible {
    item: String,
    start_ms: u64,
    progress: Progress,
}

type Socket = Arc<Mutex<Option<StdUnixStream>>>;

struct Relay {
    remote: PathBuf,
    spectrum: Arc<Spectrum>,
    audible: Arc<Mutex<Option<Audible>>>,
    snapshot: Arc<Mutex<Option<State>>>,
    local: mpsc::SyncSender<Local>,
    shutdown: Notify,
}

impl Relay {
    fn observe(&self, state: State, volume: &mut Option<u8>) {
        if *volume != Some(state.volume) {
            *volume = Some(state.volume);
            let _ = self.local.try_send(Local::Volume(state.volume));
        }
        // Spectrum frames must name the remote's queue entry, or the TUI drops
        // them as belonging to a different track.
        self.spectrum.context(state.current_id.as_deref());
        let _ = self.local.try_send(Local::State(Box::new(state.clone())));
        *lock(&self.snapshot) = Some(state);
    }

    fn audible_position(&self, item: &str) -> Option<u64> {
        lock(&self.audible)
            .as_ref()
            .filter(|audible| audible.item == item)
            .map(|audible| audible.start_ms + audible.progress.position_ms())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub async fn run(paths: Paths, remote: PathBuf) -> Result<()> {
    let Some(bind) = Bind::open(&paths)? else {
        return Ok(());
    };
    let spectrum = Spectrum::start()?;
    let (local, local_rx) = mpsc::sync_channel(64);
    let relay = Arc::new(Relay {
        remote: remote.clone(),
        spectrum: spectrum.clone(),
        audible: Arc::default(),
        snapshot: Arc::default(),
        local: local.clone(),
        shutdown: Notify::new(),
    });
    let stopping = Arc::new(AtomicBool::new(false));
    let socket: Socket = Arc::default();

    // Media keys drive the remote server; Now Playing mirrors its state.
    let media = {
        let remote = remote.clone();
        let handle = tokio::runtime::Handle::current();
        crate::media_controls::Controls::new(move |command| {
            let remote = remote.clone();
            handle.spawn(async move {
                if let Err(error) = request(&remote, command).await {
                    tracing::warn!("Media command not delivered: {error:#}");
                }
            });
            true
        })
    };
    let player = {
        let audible = relay.audible.clone();
        let snapshot = relay.snapshot.clone();
        let socket = socket.clone();
        let spectrum = spectrum.clone();
        std::thread::Builder::new()
            .name("vtamp-relay-player".into())
            .spawn(move || {
                player(
                    local_rx,
                    RodioBackend::with_spectrum(spectrum),
                    media,
                    audible,
                    snapshot,
                    socket,
                )
            })?
    };
    let reader = {
        let remote = remote.clone();
        let local = local.clone();
        let stopping = stopping.clone();
        let socket = socket.clone();
        std::thread::Builder::new()
            .name("vtamp-cast-reader".into())
            .spawn(move || reader(&remote, &local, &stopping, &socket))?
    };
    let watcher = tokio::spawn(watch(relay.clone()));

    let permits = Arc::new(Semaphore::new(64));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tracing::info!(remote = %remote.display(), "vtamp relay ready");
    loop {
        tokio::select! {
            _ = relay.shutdown.notified() => break,
            _ = terminate.recv() => break,
            _ = interrupt.recv() => break,
            accepted = bind.listener.accept() => {
                let (stream, _) = accepted?;
                if let Ok(permit) = permits.clone().try_acquire_owned() {
                    let relay = relay.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(error) = connection(stream, relay).await {
                            tracing::debug!("Client disconnected: {error:#}");
                        }
                    });
                }
            }
        }
    }
    stopping.store(true, Ordering::Release);
    disconnect(&socket);
    let _ = local.send(Local::Shutdown);
    watcher.abort();
    tokio::task::spawn_blocking(move || {
        if player.join().is_err() {
            tracing::error!("Relay player thread panicked");
        }
        if reader.join().is_err() {
            tracing::error!("Cast reader thread panicked");
        }
    })
    .await?;
    drop(bind);
    Ok(())
}

/// Close the cast connection so the reader rejoins the live stream.
fn disconnect(socket: &Socket) {
    if let Some(stream) = lock(socket).take() {
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

fn player(
    commands: mpsc::Receiver<Local>,
    mut backend: RodioBackend,
    mut media: crate::media_controls::Controls,
    audible: Arc<Mutex<Option<Audible>>>,
    snapshot: Arc<Mutex<Option<State>>>,
    socket: Socket,
) {
    let mut volume: Option<u8> = None;
    let mut output_retry: Option<Instant> = None;
    loop {
        match commands.recv_timeout(Duration::from_millis(200)) {
            Ok(Local::Stream(start)) => {
                let StreamStart {
                    serial,
                    tags,
                    offset_ms,
                    source,
                } = *start;
                let tag = |key: &str| {
                    tags.iter()
                        .find(|(k, _)| k == key)
                        .map(|(_, value)| value.as_str())
                };
                let item = tag("VTAMP_ITEM").map(str::to_owned);
                let start_ms = tag("VTAMP_POSITION_MS")
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or(0)
                    + offset_ms;
                let level = volume
                    .unwrap_or_else(|| lock(&snapshot).as_ref().map_or(70, |state| state.volume));
                match backend.play(Box::new(source), level) {
                    Ok(()) => {
                        output_retry = None;
                        *lock(&audible) =
                            item.zip(backend.progress())
                                .map(|(item, progress)| Audible {
                                    item,
                                    start_ms,
                                    progress,
                                });
                        tracing::info!(serial, start_ms, "Playing the remote cast");
                    }
                    Err(error) => {
                        *lock(&audible) = None;
                        if output_retry.is_none() {
                            tracing::warn!("Cannot play the cast locally: {error:#}");
                        }
                        output_retry = Some(Instant::now() + OUTPUT_RETRY);
                    }
                }
            }
            Ok(Local::Volume(value)) => {
                volume = Some(value);
                backend.volume(value);
            }
            Ok(Local::State(state)) => media.update(&state, false),
            Ok(Local::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(reason) = backend.output_event() {
            tracing::warn!(reason, "Local audio output lost; rejoining the cast");
            *lock(&audible) = None;
            // A live cast cannot rewind; rejoin where the server is now.
            disconnect(&socket);
        } else if output_retry.is_some_and(|at| Instant::now() >= at) {
            output_retry = None;
            disconnect(&socket);
        }
    }
    backend.stop();
}

fn reader(remote: &Path, local: &mpsc::SyncSender<Local>, stopping: &AtomicBool, socket: &Socket) {
    let mut reported: Option<String> = None;
    while !stopping.load(Ordering::Acquire) {
        match join_cast(remote) {
            Ok(stream) => {
                reported = None;
                tracing::info!("Joined the remote cast");
                *lock(socket) = stream.try_clone().ok();
                let result = read_streams(&stream, |start| {
                    local.try_send(Local::Stream(Box::new(start))).is_ok()
                        && !stopping.load(Ordering::Acquire)
                });
                *lock(socket) = None;
                if !stopping.load(Ordering::Acquire) {
                    tracing::warn!(?result, "Cast connection ended; rejoining");
                }
            }
            Err(error) => {
                let message = format!("{error:#}");
                if reported.as_ref() != Some(&message) {
                    tracing::warn!("Cannot join the remote cast: {message}");
                    reported = Some(message);
                }
            }
        }
        // Retry in slices so a shutdown stays prompt.
        for _ in 0..20 {
            if stopping.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

fn join_cast(remote: &Path) -> Result<StdUnixStream> {
    let mut stream = StdUnixStream::connect(remote)
        .with_context(|| format!("Cannot connect to {}", remote.display()))?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    write_frame(
        &mut stream,
        &Request {
            version: PROTOCOL_VERSION,
            request: Command::CastWatch,
        },
    )?;
    let reply: Reply = read_frame(&mut stream)?;
    ensure!(
        reply.version == PROTOCOL_VERSION,
        "Incompatible remote protocol version {}",
        reply.version
    );
    reply.into_data()?;
    // Silence keeps flowing while a stream is open; while the remote is stopped
    // nothing arrives, and only a closed socket should end the read.
    stream.set_read_timeout(None)?;
    Ok(stream)
}

fn write_frame<T: Serialize>(stream: &mut StdUnixStream, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(&bytes)?;
    Ok(())
}

fn read_frame<T: DeserializeOwned>(stream: &mut StdUnixStream) -> Result<T> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_be_bytes(len) as usize;
    ensure!(len <= wire::MAX_FRAME, "IPC frame exceeds 16 MiB");
    let mut data = vec![0; len];
    stream.read_exact(&mut data)?;
    Ok(serde_json::from_slice(&data)?)
}

async fn connect(remote: &Path) -> Result<UnixStream> {
    tokio::time::timeout(Duration::from_secs(3), UnixStream::connect(remote))
        .await
        .with_context(|| format!("Connecting to {} timed out", remote.display()))?
        .with_context(|| format!("Cannot connect to {}", remote.display()))
}

async fn request(remote: &Path, command: Command) -> Result<Reply> {
    let mut stream = connect(remote).await?;
    wire::write(
        &mut stream,
        &Request {
            version: PROTOCOL_VERSION,
            request: command,
        },
    )
    .await?;
    tokio::time::timeout(Duration::from_secs(10), wire::read(&mut stream)).await?
}

/// Follow the remote state for the volume to apply locally, the Now Playing
/// mirror, and the current entry that progress events refer to.
async fn watch(relay: Arc<Relay>) {
    let mut volume = None;
    loop {
        if let Err(error) = watch_once(&relay, &mut volume).await {
            tracing::debug!("Remote watch ended: {error:#}");
        }
        *lock(&relay.snapshot) = None;
        tokio::time::sleep(RETRY).await;
    }
}

async fn watch_once(relay: &Relay, volume: &mut Option<u8>) -> Result<()> {
    let mut stream = connect(&relay.remote).await?;
    wire::write(
        &mut stream,
        &Request {
            version: PROTOCOL_VERSION,
            request: Command::Watch,
        },
    )
    .await?;
    let reply: Reply =
        tokio::time::timeout(Duration::from_secs(5), wire::read(&mut stream)).await??;
    let state: State = serde_json::from_value(reply.into_data()?)?;
    relay.observe(state, volume);
    loop {
        let reply: Reply = wire::read(&mut stream).await?;
        match serde_json::from_value(reply.into_data()?)? {
            Event::State(state) => relay.observe(state, volume),
            Event::Shutdown => bail!("The remote server shut down"),
            _ => {}
        }
    }
}

async fn connection(mut stream: UnixStream, relay: Arc<Relay>) -> Result<()> {
    let request: Request =
        match tokio::time::timeout(Duration::from_secs(5), wire::read(&mut stream)).await {
            Ok(Ok(request)) => request,
            _ => {
                return reply(
                    &mut stream,
                    Reply::failure(ApiError::new(
                        "invalid_request",
                        "Expected a bounded JSON request",
                    )),
                )
                .await;
            }
        };
    if request.version != PROTOCOL_VERSION {
        return reply(
            &mut stream,
            Reply::failure(ApiError::new(
                "version_mismatch",
                "Client and server protocol versions differ; restart the server with this binary",
            )),
        )
        .await;
    }
    match request.request {
        Command::ArchiveImport { .. } | Command::ArchiveStatus { .. } => reply(
            &mut stream,
            Reply::failure(ApiError::new(
                "archive_local_only",
                "Run library archive commands on the server's local machine, outside relay mode",
            )),
        )
        .await,
        Command::SpectrumWatch => daemon::spectrum_connection(stream, relay.spectrum.clone()).await,
        Command::Shutdown => {
            reply(&mut stream, Reply::success(json!({"stopped": true}))).await?;
            relay.shutdown.notify_one();
            Ok(())
        }
        Command::ServerInfo => {
            reply(
                &mut stream,
                Reply::success(ServerInfo {
                    mode: ServerMode::Relay,
                    remote: Some(relay.remote.clone()),
                    api_url: None,
                    version: env!("CARGO_PKG_VERSION").into(),
                }),
            )
            .await
        }
        Command::Status | Command::Now | Command::Watch => {
            forward_patched(stream, &relay, request).await
        }
        _ => forward(stream, &relay, request).await,
    }
}

async fn reply(stream: &mut UnixStream, reply: Reply) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(5), wire::write(stream, &reply)).await??;
    Ok(())
}

/// Open the remote connection for a client request, answering the client when
/// the remote is unreachable.
async fn upstream(
    relay: &Relay,
    stream: &mut UnixStream,
    request: &Request,
) -> Result<Option<UnixStream>> {
    match connect(&relay.remote).await {
        Ok(mut upstream) => {
            wire::write(&mut upstream, request).await?;
            Ok(Some(upstream))
        }
        Err(error) => {
            reply(
                stream,
                Reply::failure(ApiError::new(
                    "remote_unavailable",
                    format!("Cannot reach the remote server: {error:#}"),
                )),
            )
            .await?;
            Ok(None)
        }
    }
}

/// Byte-for-byte forwarding covers single replies, long-lived event streams,
/// and the raw cast alike.
async fn forward(mut stream: UnixStream, relay: &Relay, request: Request) -> Result<()> {
    let Some(mut upstream) = upstream(relay, &mut stream, &request).await? else {
        return Ok(());
    };
    tokio::io::copy_bidirectional(&mut stream, &mut upstream).await?;
    Ok(())
}

/// State replies and events pass through with `position_ms` corrected to what
/// is audible here.
async fn forward_patched(mut stream: UnixStream, relay: &Relay, request: Request) -> Result<()> {
    let watch = matches!(request.request, Command::Watch);
    let now = matches!(request.request, Command::Now);
    let Some(mut upstream) = upstream(relay, &mut stream, &request).await? else {
        return Ok(());
    };
    let mut first: Value =
        tokio::time::timeout(Duration::from_secs(125), wire::read(&mut upstream)).await??;
    if first["ok"] == true
        && let Some(data) = first.get_mut("data")
    {
        patch_state(data, now, &|item| relay.audible_position(item));
    }
    tokio::time::timeout(Duration::from_secs(5), wire::write(&mut stream, &first)).await??;
    if !watch {
        return Ok(());
    }
    loop {
        let mut byte = [0u8; 1];
        tokio::select! {
            _ = stream.read(&mut byte) => break,
            frame = wire::read::<_, Value>(&mut upstream) => {
                let mut frame = frame?;
                if frame["ok"] == true && let Some(data) = frame.get_mut("data") {
                    let current = lock(&relay.snapshot)
                        .as_ref()
                        .filter(|state| state.status == PlaybackStatus::Playing)
                        .and_then(|state| state.current_id.clone());
                    patch_event(data, current.as_deref(), &|item| relay.audible_position(item));
                }
                tokio::time::timeout(Duration::from_secs(5), wire::write(&mut stream, &frame)).await??;
            }
        }
    }
    Ok(())
}

/// Replace `position_ms` (and `remaining_ms` for `now`) with the audible
/// position while the same entry is playing. The audible position never
/// exceeds the remote one.
fn patch_state(value: &mut Value, now: bool, audible: &dyn Fn(&str) -> Option<u64>) {
    if value["status"] != "playing" {
        return;
    }
    // A state carries `current_id`; `now` nests the entry under `current`.
    let Some(item) = value["current_id"]
        .as_str()
        .or_else(|| value["current"]["id"].as_str())
    else {
        return;
    };
    let (Some(local), Some(remote)) = (audible(item), value["position_ms"].as_u64()) else {
        return;
    };
    let position = local.min(remote);
    value["position_ms"] = json!(position);
    if now && let Some(duration) = value["duration_ms"].as_u64() {
        value["remaining_ms"] = json!(duration.saturating_sub(position));
    }
}

/// Patch a watch event: `state` events like a state reply, `progress` events
/// against the entry the remote currently plays.
fn patch_event(event: &mut Value, current: Option<&str>, audible: &dyn Fn(&str) -> Option<u64>) {
    match event["event"].as_str() {
        Some("state") => {
            if let Some(state) = event.get_mut("data") {
                patch_state(state, false, audible);
            }
        }
        Some("progress") => {
            let Some(item) = current else { return };
            let Some(progress) = event.get_mut("data") else {
                return;
            };
            let (Some(local), Some(remote)) = (audible(item), progress["position_ms"].as_u64())
            else {
                return;
            };
            progress["position_ms"] = json!(local.min(remote));
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_follow_the_audible_entry_and_never_lead_the_remote() {
        let audible = |item: &str| (item == "q1").then_some(41_000);
        let mut state = json!({"status": "playing", "current_id": "q1", "position_ms": 42_000});
        patch_state(&mut state, false, &audible);
        assert_eq!(state["position_ms"], 41_000);
        // A different entry, a paused remote, or no local audio leave the reply alone.
        let mut other = json!({"status": "playing", "current_id": "q2", "position_ms": 5_000});
        patch_state(&mut other, false, &audible);
        assert_eq!(other["position_ms"], 5_000);
        let mut paused = json!({"status": "paused", "current_id": "q1", "position_ms": 5_000});
        patch_state(&mut paused, false, &audible);
        assert_eq!(paused["position_ms"], 5_000);
        let mut ahead = json!({"status": "playing", "current_id": "q1", "position_ms": 40_000});
        patch_state(&mut ahead, false, &audible);
        assert_eq!(ahead["position_ms"], 40_000);
        let mut now = json!({"status": "playing", "current": {"id": "q1"}, "position_ms": 42_000, "duration_ms": 100_000, "remaining_ms": 58_000});
        patch_state(&mut now, true, &audible);
        assert_eq!(now["position_ms"], 41_000);
        assert_eq!(now["remaining_ms"], 59_000);

        let mut progress =
            json!({"event": "progress", "data": {"position_ms": 42_000, "revision": 3}});
        patch_event(&mut progress, Some("q1"), &audible);
        assert_eq!(progress["data"]["position_ms"], 41_000);
        assert_eq!(progress["data"]["revision"], 3);
        let mut progress =
            json!({"event": "progress", "data": {"position_ms": 42_000, "revision": 3}});
        patch_event(&mut progress, None, &audible);
        assert_eq!(progress["data"]["position_ms"], 42_000);
        let mut state_event = json!({"event": "state", "data": {"status": "playing", "current_id": "q1", "position_ms": 42_000}});
        patch_event(&mut state_event, Some("q1"), &audible);
        assert_eq!(state_event["data"]["position_ms"], 41_000);
        let mut other_event = json!({"event": "library_changed"});
        patch_event(&mut other_event, Some("q1"), &audible);
        assert_eq!(other_event, json!({"event": "library_changed"}));
    }
}
