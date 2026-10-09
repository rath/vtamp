mod api;
mod archive;
mod covers;
mod imports;
mod loudness;
#[cfg(target_os = "macos")]
use crate::audio::RodioBackend;
use crate::{
    audio::{PlaybackBackend, headless::HeadlessBackend},
    cast::{self, Hub},
    engine::Engine,
    library::{self, Scan},
    model::*,
    platform::Paths,
    spectrum::Spectrum,
    store::Store,
    wire,
};
use anyhow::{Context, Result};
use fs2::FileExt;
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};
use tokio::{
    net::{UnixListener, UnixStream},
    sync::{Notify, Semaphore, broadcast, oneshot},
};

type Answer = oneshot::Sender<Reply>;
enum ImportPlayback {
    Enqueue,
    Play,
    Direct,
}
struct Observers {
    events: broadcast::Sender<Event>,
    media: crate::media_controls::Controls,
    spectrum: Arc<Spectrum>,
}
enum Work {
    Youtube(Box<crate::imports::Message>),
    Retag {
        id: String,
        result: crate::metadata::Metadata,
        answer: Answer,
    },
    Request(Command, Answer),
    Catalog {
        scan: Scan,
        job: ScanJob,
    },
    Imported {
        scan: Scan,
        playback: ImportPlayback,
        answer: Answer,
    },
}

/// The per-user lock and control socket that every server mode owns.
pub(crate) struct Bind {
    _lock: fs::File,
    socket: std::path::PathBuf,
    pub(crate) listener: UnixListener,
}

impl Bind {
    /// `None` when another server already holds this home's lock.
    pub(crate) fn open(paths: &Paths) -> Result<Option<Self>> {
        paths.prepare()?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(paths.runtime.join("server.lock"))?;
        if lock.try_lock_exclusive().is_err() {
            return Ok(None);
        }
        let socket = paths.socket();
        if socket.exists() {
            fs::remove_file(&socket)?;
        }
        let listener = UnixListener::bind(&socket).context("Cannot bind the control socket")?;
        fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
        Ok(Some(Self {
            _lock: lock,
            socket,
            listener,
        }))
    }
}

impl Drop for Bind {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}

pub async fn run(
    paths: Paths,
    headless: bool,
    cast_enabled: bool,
    http: Option<std::net::SocketAddr>,
    api: Option<std::net::SocketAddr>,
) -> Result<()> {
    #[cfg(not(target_os = "macos"))]
    let headless = {
        if !headless {
            tracing::info!("Device playback is not built for this platform; running headless");
        }
        true
    };
    let Some(bind) = Bind::open(&paths)? else {
        return Ok(());
    };
    let mut store = Store::open(&paths.database())?;
    crate::deletion::recover(&paths, &store)?;
    crate::archive::recover(&paths, &store)?;
    store.interrupt_scans(unix_ms())?;
    store.repair_import_titles()?;
    store.interrupt_imports()?;
    if paths.data.join("imports/youtube").is_dir() {
        store.add_root(&paths.data.join("imports/youtube"))?;
    }
    let state = store.restore()?;
    let (sender, receiver) = mpsc::sync_channel(64);
    let (events, _) = broadcast::channel(64);
    let media_commands = sender.clone();
    let media = crate::media_controls::Controls::new(move |command| {
        let (answer, _) = oneshot::channel();
        media_commands
            .try_send(Work::Request(command, answer))
            .is_ok()
    });
    let spectrum = Spectrum::start()?;
    let cast = (headless || cast_enabled || http.is_some()).then(|| Arc::new(Hub::default()));
    let http = match http {
        Some(address) => {
            Some(cast::http::HttpCast::bind(address, &cast::http::token(&paths)?).await?)
        }
        None => None,
    };
    let api = match api {
        Some(address) => Some(api::Api::bind(address).await?),
        None => None,
    };
    let served = Arc::new(Served {
        cast: cast.clone(),
        url: http.as_ref().map(|http| http.url.clone()),
        api_url: api.as_ref().map(|api| api.url.clone()),
        headless,
    });
    let http =
        http.map(|http| tokio::spawn(http.serve(cast.clone().expect("a cast exists with HTTP"))));
    let api = api.map(|api| {
        tokio::spawn(api.serve(Arc::new(api::Context {
            sender: sender.clone(),
            served: served.clone(),
        })))
    });
    let shutdown = Arc::new(Notify::new());
    let thread = {
        let events = events.clone();
        let shutdown = shutdown.clone();
        let tx = sender.clone();
        let spectrum = spectrum.clone();
        let cast = cast.clone();
        std::thread::Builder::new()
            .name("vtamp-player".into())
            .spawn(move || {
                let backend: Box<dyn PlaybackBackend> = if headless {
                    let hub = cast.clone().expect("headless servers always cast");
                    match HeadlessBackend::new(hub, cast::DEFAULT_BITRATE) {
                        Ok(backend) => Box::new(backend),
                        Err(error) => {
                            tracing::error!("Cannot start the headless backend: {error:#}");
                            let _ = events.send(Event::Shutdown);
                            shutdown.notify_one();
                            return;
                        }
                    }
                } else {
                    #[cfg(target_os = "macos")]
                    {
                        let device = RodioBackend::with_spectrum(spectrum.clone());
                        match &cast {
                            Some(hub) => match device.with_cast(hub.clone(), cast::DEFAULT_BITRATE)
                            {
                                Ok(backend) => Box::new(backend),
                                Err(error) => {
                                    tracing::error!("Cannot start the cast encoder: {error:#}");
                                    let _ = events.send(Event::Shutdown);
                                    shutdown.notify_one();
                                    return;
                                }
                            },
                            None => Box::new(device),
                        }
                    }
                    #[cfg(not(target_os = "macos"))]
                    unreachable!("non-macOS servers are always headless")
                };
                let result = worker(
                    paths,
                    store,
                    Engine::new(state, backend),
                    receiver,
                    tx,
                    Observers {
                        events: events.clone(),
                        media,
                        spectrum,
                    },
                );
                if let Err(error) = result {
                    tracing::error!("Player stopped: {error:#}");
                }
                let _ = events.send(Event::Shutdown);
                shutdown.notify_one();
            })?
    };
    let permits = Arc::new(Semaphore::new(64));
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
    tracing::info!(
        headless,
        cast = cast.is_some(),
        url = served.url.as_deref(),
        api = served.api_url.as_deref(),
        "vtamp server ready"
    );
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = terminate.recv() => { let _ = dispatch(&sender, Command::Shutdown).await; },
            _ = interrupt.recv() => { let _ = dispatch(&sender, Command::Shutdown).await; },
            accepted = bind.listener.accept() => {
                let (stream, _) = accepted?;
                if let Ok(permit) = permits.clone().try_acquire_owned() {
                    let sender = sender.clone(); let events = events.clone(); let spectrum = spectrum.clone(); let served = served.clone();
                    tokio::spawn(async move {
                        let _permit = permit;
                        if let Err(error) = connection(stream, sender, events, spectrum, served).await { tracing::debug!("Client disconnected: {error:#}"); }
                    });
                }
            }
        }
    }
    if let Some(http) = http {
        http.abort();
    }
    if let Some(api) = api {
        api.abort();
    }
    // Give the shutdown acknowledgement and final event time to reach clients.
    tokio::time::sleep(Duration::from_millis(100)).await;
    thread
        .join()
        .map_err(|_| anyhow::anyhow!("Player thread panicked"))?;
    drop(bind);
    Ok(())
}

async fn dispatch(sender: &mpsc::SyncSender<Work>, command: Command) -> Reply {
    let (tx, rx) = oneshot::channel();
    if sender.try_send(Work::Request(command, tx)).is_err() {
        return Reply::failure(ApiError::new(
            "server_busy",
            "Server is busy or shutting down; retry later",
        ));
    }
    match tokio::time::timeout(Duration::from_secs(120), rx).await {
        Ok(Ok(reply)) => reply,
        _ => Reply::failure(ApiError::new(
            "timeout",
            "The command outcome is unknown; inspect status before retrying",
        )),
    }
}

/// What this server offers besides playback commands.
struct Served {
    cast: Option<Arc<Hub>>,
    url: Option<String>,
    api_url: Option<String>,
    headless: bool,
}

impl Served {
    fn server_info(&self) -> ServerInfo {
        ServerInfo {
            mode: if self.headless {
                ServerMode::Headless
            } else {
                ServerMode::Device
            },
            remote: None,
            api_url: self.api_url.clone(),
            version: env!("CARGO_PKG_VERSION").into(),
        }
    }
    fn cast_info(&self) -> CastInfo {
        cast_info(self.cast.as_deref(), self.url.clone())
    }
}

async fn connection(
    mut stream: UnixStream,
    sender: mpsc::SyncSender<Work>,
    events: broadcast::Sender<Event>,
    spectrum: Arc<Spectrum>,
    served: Arc<Served>,
) -> Result<()> {
    let request: Request =
        match tokio::time::timeout(Duration::from_secs(5), wire::read(&mut stream)).await {
            Ok(Ok(request)) => request,
            _ => {
                wire::write(
                    &mut stream,
                    &Reply::failure(ApiError::new(
                        "invalid_request",
                        "Expected a bounded JSON request",
                    )),
                )
                .await?;
                return Ok(());
            }
        };
    if request.version != PROTOCOL_VERSION {
        wire::write(
            &mut stream,
            &Reply::failure(ApiError::new(
                "version_mismatch",
                "Client and server protocol versions differ; restart the server with this binary",
            )),
        )
        .await?;
        return Ok(());
    }
    if matches!(request.request, Command::SpectrumWatch) {
        return spectrum_connection(stream, spectrum).await;
    }
    if matches!(request.request, Command::ServerInfo) {
        let reply = Reply::success(served.server_info());
        tokio::time::timeout(Duration::from_secs(5), wire::write(&mut stream, &reply)).await??;
        return Ok(());
    }
    if matches!(request.request, Command::CastInfo) {
        let reply = Reply::success(served.cast_info());
        tokio::time::timeout(Duration::from_secs(5), wire::write(&mut stream, &reply)).await??;
        return Ok(());
    }
    if matches!(request.request, Command::CastWatch) {
        return match &served.cast {
            Some(hub) => cast_connection(stream, hub.clone(), served.url.clone()).await,
            None => {
                let reply = Reply::failure(ApiError::new(
                    "cast_unavailable",
                    "This server plays through its audio device; start it with --headless to cast",
                ));
                tokio::time::timeout(Duration::from_secs(5), wire::write(&mut stream, &reply))
                    .await??;
                Ok(())
            }
        };
    }
    let watch = matches!(request.request, Command::Watch);
    let mut subscription = events.subscribe();
    let reply = dispatch(
        &sender,
        if watch {
            Command::Status
        } else {
            request.request
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(5), wire::write(&mut stream, &reply)).await??;
    if watch && reply.ok {
        let available = dispatch(&sender, Command::ImportAvailable)
            .await
            .into_data()?["available"]
            == true;
        if available {
            let jobs = dispatch(&sender, Command::Imports).await.into_data()?;
            tokio::time::timeout(
                Duration::from_secs(5),
                wire::write(
                    &mut stream,
                    &Reply::success(Event::Imports(serde_json::from_value(jobs)?)),
                ),
            )
            .await??;
        }
        loop {
            let event = match subscription.recv().await {
                Ok(event) => event,
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    if available {
                        let jobs = dispatch(&sender, Command::Imports).await.into_data()?;
                        tokio::time::timeout(
                            Duration::from_secs(5),
                            wire::write(
                                &mut stream,
                                &Reply::success(Event::Imports(serde_json::from_value(jobs)?)),
                            ),
                        )
                        .await??;
                    }
                    let reply = dispatch(&sender, Command::Status).await;
                    Event::State(serde_json::from_value(reply.into_data()?)?)
                }
                Err(_) => break,
            };
            tokio::time::timeout(
                Duration::from_secs(5),
                wire::write(&mut stream, &Reply::success(&event)),
            )
            .await??;
            if matches!(event, Event::Shutdown) {
                break;
            }
        }
    }
    Ok(())
}

pub(crate) async fn spectrum_connection(
    mut stream: UnixStream,
    spectrum: Arc<Spectrum>,
) -> Result<()> {
    use tokio::io::AsyncReadExt;
    let mut subscription = spectrum.subscribe();
    let first = subscription.frames.borrow_and_update().clone();
    tokio::time::timeout(
        Duration::from_secs(2),
        wire::write(&mut stream, &Reply::success(first)),
    )
    .await??;
    loop {
        let mut byte = [0u8; 1];
        tokio::select! {
            // This stream is read-only after subscribing. EOF releases demand immediately.
            _ = stream.read(&mut byte) => break,
            changed = subscription.frames.changed() => {
                if changed.is_err() { break; }
                let frame = subscription.frames.borrow_and_update().clone();
                tokio::time::timeout(Duration::from_secs(2), wire::write(&mut stream, &Reply::success(frame))).await??;
            }
        }
    }
    Ok(())
}

fn cast_info(hub: Option<&Hub>, url: Option<String>) -> CastInfo {
    CastInfo {
        available: hub.is_some(),
        url,
        codec: "opus".into(),
        container: "ogg".into(),
        bitrate: hub.map_or(0, |_| cast::DEFAULT_BITRATE),
        sample_rate: cast::codec::SAMPLE_RATE,
        channels: cast::codec::CHANNELS as u8,
        listeners: hub.map_or(0, Hub::listeners),
    }
}

/// After the JSON handshake the connection carries raw Ogg pages. A listener
/// that joins mid-stream receives the open stream's headers first.
async fn cast_connection(mut stream: UnixStream, hub: Arc<Hub>, url: Option<String>) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let (headers, mut listener) = hub.subscribe();
    let reply = Reply::success(cast_info(Some(&hub), url));
    tokio::time::timeout(Duration::from_secs(2), wire::write(&mut stream, &reply)).await??;
    if let Some(headers) = headers {
        tokio::time::timeout(Duration::from_secs(5), stream.write_all(&headers)).await??;
    }
    loop {
        let mut byte = [0u8; 1];
        tokio::select! {
            // Read-only after subscribing: EOF or any byte releases the listener.
            _ = stream.read(&mut byte) => break,
            chunk = listener.recv() => match chunk {
                Ok(chunk) => {
                    tokio::time::timeout(Duration::from_secs(5), stream.write_all(&chunk)).await??;
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    // The missed pages may include a new stream's headers.
                    tracing::debug!(skipped, "Cast listener fell behind");
                    if let Some(headers) = hub.headers() {
                        tokio::time::timeout(Duration::from_secs(5), stream.write_all(&headers)).await??;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }
    Ok(())
}

fn worker(
    paths: Paths,
    mut store: Store,
    mut engine: Engine<Box<dyn PlaybackBackend>>,
    rx: mpsc::Receiver<Work>,
    tx: mpsc::SyncSender<Work>,
    observers: Observers,
) -> Result<()> {
    let Observers {
        events,
        mut media,
        spectrum,
    } = observers;
    let events = &events;
    let mut last_save = Instant::now();
    let mut checkpoint = (engine.state.revision, engine.state.position_ms);
    let mut last_progress = Instant::now();
    let mut imports = 0usize;
    let mut active_scan: Option<String> = None;
    let mut youtube = imports::Runtime::new(&store)?;
    let mut covers = covers::Runtime::new();
    let mut archive = archive::Runtime::new();
    let mut normalization = loudness::Runtime::new(&store, &mut engine, events)?;
    loop {
        let revision_before = engine.state.revision;
        let mut changed = false;
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(Work::Youtube(message)) => {
                if let Err(error) = youtube.message(*message, &mut store, events) {
                    tracing::error!("Cannot persist import update: {error:#}");
                }
            }
            Ok(Work::Retag { id, result, answer }) => {
                let result = store
                    .edit_metadata(&id, None, None, None, Some(result))
                    .and_then(|track| {
                        imports::update_queue_metadata(&track, &mut engine, &store, events)?;
                        Ok(Reply::success(track))
                    });
                let _ = answer.send(result.unwrap_or_else(failure));
            }
            Ok(Work::Request(command, answer)) => {
                if archive.active() && archive::Runtime::conflicts(&command) {
                    let _ = answer.send(Reply::failure(ApiError::new(
                        "library_busy",
                        "Wait for the archive restore to finish",
                    )));
                } else if let Command::ArchiveStatus { id } = &command {
                    let _ = answer.send(archive.status(id).unwrap_or_else(failure));
                } else if let Command::ArchiveImport { path } = &command {
                    let result = if engine.state.scanning
                        || imports > 0
                        || youtube.active()
                        || covers.active()
                    {
                        Err(ApiError::new(
                            "library_busy",
                            "Wait for scans, downloads, covers and metadata tasks to finish",
                        )
                        .into())
                    } else {
                        archive.start(path.clone(), &paths, &store)
                    };
                    let _ = answer.send(result.unwrap_or_else(failure));
                } else if let Some(result) = youtube.command(&command, &paths, &mut store, events) {
                    let _ = answer.send(result.unwrap_or_else(failure));
                } else if let Some(result) = covers.command(&command, &paths, &store) {
                    let _ = answer.send(result.unwrap_or_else(failure));
                } else if matches!(
                    command,
                    Command::ImportPreview { .. }
                        | Command::ImportCapabilities
                        | Command::LibraryRetag { .. }
                ) && youtube.tasks_busy()
                {
                    let _ = answer.send(failure(anyhow::anyhow!(
                        "Too many metadata or preview requests; try again shortly"
                    )));
                } else {
                    match command {
                        Command::StreamPreview { path } => {
                            if !path.is_absolute() {
                                let _ = answer.send(Reply::failure(ApiError::new(
                                    "invalid_arguments",
                                    "Playlist path must be absolute",
                                )));
                                continue;
                            }
                            if youtube.tasks_busy() {
                                let _ = answer.send(Reply::failure(ApiError::new(
                                    "server_busy",
                                    "Too many preview requests",
                                )));
                                continue;
                            }
                            youtube.spawn_task(move |_| {
                                let result =
                                    crate::streams::read_playlist(&path).map(Reply::success);
                                let _ = answer.send(result.unwrap_or_else(failure));
                            });
                        }
                        Command::StreamAdd { entries } => {
                            let result = store.add_streams(&entries).map(Reply::success);
                            if result.is_ok() {
                                let _ = events.send(Event::LibraryChanged);
                            }
                            let _ = answer.send(result.unwrap_or_else(failure));
                        }
                        Command::StreamRemove { id } => {
                            let result = store
                                .remove_stream(&id)
                                .map(|()| Reply::success(json!({"removed": id})));
                            if result.is_ok() {
                                let _ = events.send(Event::LibraryChanged);
                            }
                            let _ = answer.send(result.unwrap_or_else(failure));
                        }
                        Command::LibraryDelete { id } => {
                            let revision = engine.state.revision;
                            let result = (|| -> Result<Reply> {
                                if engine.state.scanning
                                    || imports > 0
                                    || youtube.active()
                                    || covers.active()
                                {
                                    return Err(ApiError::new("library_busy", "Wait for scans, imports and cover updates to finish before deleting").into());
                                }
                                let result =
                                    crate::deletion::delete(&paths, &mut store, &mut engine, &id)?;
                                Ok(Reply::success(result))
                            })();
                            if result.is_ok() {
                                if engine.state.revision != revision {
                                    let _ = events.send(Event::State(engine.state.clone()));
                                }
                                let _ = events.send(Event::LibraryChanged);
                            }
                            let _ = answer.send(result.unwrap_or_else(failure));
                        }
                        Command::PlayDirect { path, track } => {
                            if path.is_some() == track.is_some() {
                                let _ = answer.send(Reply::failure(ApiError::new(
                                    "invalid_arguments",
                                    "Provide exactly one path or library track",
                                )));
                                continue;
                            }
                            if let Some(id) = track {
                                let result = (|| -> Result<()> {
                                    let track = store.track(&id)?.ok_or_else(|| {
                                        ApiError::new("track_not_found", "Library track not found")
                                    })?;
                                    engine.play_direct(track)
                                })();
                                finish_command(result, answer, &mut engine, &store, events)?;
                            } else if let Some(path) = path {
                                if !path.is_absolute() || !path.is_file() {
                                    let _ = answer.send(Reply::failure(ApiError::new(
                                        "invalid_arguments",
                                        "Direct playback requires one absolute file path",
                                    )));
                                    continue;
                                }
                                if imports >= 4 {
                                    let _ = answer.send(Reply::failure(ApiError::new(
                                        "server_busy",
                                        "Too many imports in progress",
                                    )));
                                    continue;
                                }
                                let old = store.records()?;
                                let cache = paths.cache.clone();
                                let tx = tx.clone();
                                imports += 1;
                                std::thread::spawn(move || {
                                    let scan = library::scan(&[path], &old, &cache);
                                    let _ = tx.send(Work::Imported {
                                        scan,
                                        playback: ImportPlayback::Direct,
                                        answer,
                                    });
                                });
                            }
                        }
                        Command::Shutdown => {
                            store.save(&engine.state)?;
                            engine.stop();
                            let _ = answer.send(Reply::success(json!({"stopped": true})));
                            break;
                        }
                        Command::ImportPreview { request } => {
                            let paths = paths.clone();
                            youtube.spawn_task(move |stop| {
                                let result = (|| -> Result<Reply> {
                                    let config = crate::import_config::Config::load(&paths)?;
                                    let mut request = request;
                                    request.validate()?;
                                    let preview = crate::youtube::preview(
                                        &request.url,
                                        request.playlist,
                                        request.range,
                                        &config,
                                        &stop,
                                    )?;
                                    let mut result = json!({"preview":preview});
                                    if let Some(range) = request.range {
                                        result["range"] = json!(range);
                                    }
                                    Ok(Reply::success(result))
                                })();
                                let _ = answer.send(result.unwrap_or_else(failure));
                            });
                        }
                        Command::ImportAvailable => {
                            let _=answer.send(Reply::success(json!({"available":crate::import_config::youtube_available(&paths)})));
                        }
                        Command::ImportCapabilities => {
                            let paths = paths.clone();
                            youtube.spawn_task(move |stop| {
                                let result =
                                    crate::import_config::YoutubeConfig::load(&paths).map(|c| {
                                        Reply::success(
                                            crate::import_config::capabilities_with_cancel(
                                                &c, &stop,
                                            ),
                                        )
                                    });
                                let _ = answer.send(result.unwrap_or_else(failure));
                            });
                        }
                        Command::LibraryEdit {
                            id,
                            title,
                            artist,
                            album,
                        } => {
                            let result = store
                                .edit_metadata(&id, title, artist, album, None)
                                .and_then(|track| {
                                    imports::update_queue_metadata(
                                        &track,
                                        &mut engine,
                                        &store,
                                        events,
                                    )?;
                                    Ok(Reply::success(track))
                                });
                            let _ = answer.send(result.unwrap_or_else(failure));
                        }
                        Command::LibraryRetag { id } => {
                            let source = store.track(&id).and_then(|t| {
                                t.and_then(|t| t.source)
                                    .context("Track has no YouTube source")
                            });
                            match source {
                                Ok(source) => {
                                    let tx = tx.clone();
                                    let paths = paths.clone();
                                    youtube.spawn_task(move |stop| {
                                        match crate::import_config::Config::load(&paths) {
                                            Ok(config) => {
                                                let result = crate::metadata::resolve(
                                                    &source, &config, &paths, &stop,
                                                );
                                                let mut work = Work::Retag { id, result, answer };
                                                while !stop
                                                    .load(std::sync::atomic::Ordering::Relaxed)
                                                {
                                                    match tx.try_send(work) {
                                                        Ok(())
                                                        | Err(mpsc::TrySendError::Disconnected(
                                                            _,
                                                        )) => break,
                                                        Err(mpsc::TrySendError::Full(w)) => {
                                                            work = w;
                                                            std::thread::sleep(
                                                                Duration::from_millis(10),
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                let _ = answer.send(failure(e));
                                            }
                                        }
                                    });
                                }
                                Err(e) => {
                                    let _ = answer.send(failure(e));
                                }
                            }
                        }
                        Command::Status | Command::Watch | Command::Volume { value: None } => {
                            let _ = answer.send(Reply::success(&engine.state));
                        }
                        Command::Normalize { enabled } => {
                            let result = (|| -> Result<Reply> {
                                if let Some(enabled) = enabled {
                                    let mut candidate = engine.state.clone();
                                    candidate.normalization.enabled = enabled;
                                    candidate.revision += 1;
                                    store.save(&candidate)?;
                                    engine.state = candidate;
                                    let _ = events.send(Event::State(engine.state.clone()));
                                }
                                Ok(Reply::success(
                                    json!({"normalization":engine.state.normalization,"applies_to":"next_playback"}),
                                ))
                            })();
                            let _ = answer.send(result.unwrap_or_else(failure));
                        }
                        Command::Now => {
                            let _ = answer.send(Reply::success(engine.state.now()));
                        }
                        Command::SleepStatus => {
                            let _ = answer.send(Reply::success(
                                json!({"scheduled_stop":engine.state.scheduled_stop}),
                            ));
                        }
                        Command::QueuePage { offset, limit } => {
                            let items: Vec<_> = engine
                                .state
                                .queue
                                .iter()
                                .skip(offset)
                                .take(limit.clamp(1, 1000))
                                .collect();
                            let _ = answer.send(Reply::success(json!({"items":items,"total":engine.state.queue.len(),"offset":offset,"queue_revision":engine.state.queue_revision})));
                        }
                        Command::QueueEdit {
                            edit,
                            dry_run,
                            if_queue_revision,
                            request_id,
                        } => {
                            let revision = engine.state.revision;
                            let reply = match edit_queue(
                                &mut engine,
                                &mut store,
                                &edit,
                                dry_run,
                                if_queue_revision,
                                request_id.as_deref(),
                            ) {
                                Ok(reply) => reply,
                                Err(error) => failure(error),
                            };
                            if engine.state.revision != revision {
                                let _ = events.send(Event::State(engine.state.clone()));
                            }
                            let _ = answer.send(reply);
                        }
                        Command::LibrarySearch {
                            filter,
                            offset,
                            limit,
                        } => {
                            let reply = match store.search_filtered(&filter, offset, limit) {
                                Ok((tracks, total)) => Reply::success(
                                    json!({"tracks":tracks,"total":total,"offset":offset}),
                                ),
                                Err(error) => failure(error),
                            };
                            let _ = answer.send(reply);
                        }
                        Command::LibraryTrack { id } => {
                            let reply = match store.track(&id) {
                                Ok(Some(track)) => Reply::success(track),
                                Ok(None) => Reply::failure(ApiError::new(
                                    "track_not_found",
                                    "Library track not found",
                                )),
                                Err(error) => failure(error),
                            };
                            let _ = answer.send(reply);
                        }
                        Command::ScanStatus { id } => {
                            let reply = match store.scan_job(&id) {
                                Ok(job) => Reply::success(job),
                                Err(error) => failure(error),
                            };
                            let _ = answer.send(reply);
                        }
                        Command::LibraryList {
                            query,
                            offset,
                            limit,
                            anchor,
                            kind,
                        } => {
                            let reply = if let Some(id) = anchor {
                                match store.search_around(&query, kind, &id, limit) {
                                    Ok(page) => Reply::success(page),
                                    Err(e) => failure(e),
                                }
                            } else {
                                match store.search(&query, kind, offset, limit) {
                                    Ok((tracks, total)) => Reply::success(
                                        json!({"tracks": tracks, "total": total, "offset": offset}),
                                    ),
                                    Err(e) => failure(e),
                                }
                            };
                            let _ = answer.send(reply);
                        }
                        Command::LibraryRoots => {
                            let reply = match store.roots() {
                                Ok(roots) => Reply::success(roots),
                                Err(e) => failure(e),
                            };
                            let _ = answer.send(reply);
                        }
                        Command::LibraryAdd { .. }
                        | Command::LibraryRemove { .. }
                        | Command::LibraryScan => {
                            let result = (|| -> Result<String> {
                                if engine.state.scanning {
                                    return Err(ApiError::new(
                                        "scan_in_progress",
                                        "A library scan is already running",
                                    )
                                    .with_details(json!({"job_id":active_scan}))
                                    .into());
                                }
                                match &command {
                                    Command::LibraryAdd { path } => {
                                        let path = path
                                            .canonicalize()
                                            .context("Music directory does not exist")?;
                                        if !path.is_dir() {
                                            anyhow::bail!("Library roots must be directories");
                                        }
                                        store.add_root(&path)?;
                                    }
                                    Command::LibraryRemove { path } => {
                                        store.remove_root(
                                            &path.canonicalize().unwrap_or(path.clone()),
                                        )?;
                                    }
                                    _ => (),
                                }
                                let roots = store.roots()?;
                                let old = store.records()?;
                                let cache = paths.cache.clone();
                                let tx = tx.clone();
                                let mut job = ScanJob {
                                    job_id: uuid::Uuid::new_v4().to_string(),
                                    status: "running".into(),
                                    started_at_ms: unix_ms(),
                                    finished_at_ms: None,
                                    summary: None,
                                    error: None,
                                };
                                store.save_scan(&job, None)?;
                                let id = job.job_id.clone();
                                std::thread::spawn(move || {
                                    let scan = library::scan(&roots, &old, &cache);
                                    job.summary = Some(library::scan_summary(&scan, &old));
                                    let _ = tx.send(Work::Catalog { scan, job });
                                });
                                engine.state.scanning = true;
                                active_scan = Some(id.clone());
                                Ok(id)
                            })();
                            changed = result.is_ok();
                            let reply = match result {
                                Ok(id) => Reply::success(json!({"scanning": true, "job_id":id})),
                                Err(e) => failure(e),
                            };
                            let _ = answer.send(reply);
                        }
                        Command::QueueAdd {
                            paths: ref input_paths,
                            ..
                        }
                        | Command::Play {
                            paths: ref input_paths,
                            ..
                        } if !input_paths.is_empty() => {
                            if imports >= 4 {
                                let _ = answer.send(Reply::failure(ApiError::new(
                                    "server_busy",
                                    "Too many imports in progress",
                                )));
                                continue;
                            }
                            let inputs = input_paths.clone();
                            let playback = if matches!(command, Command::Play { .. }) {
                                ImportPlayback::Play
                            } else {
                                ImportPlayback::Enqueue
                            };
                            let cache = paths.cache.clone();
                            let old = store.records()?;
                            let tx = tx.clone();
                            imports += 1;
                            std::thread::spawn(move || {
                                let scan = library::scan(&inputs, &old, &cache);
                                let _ = tx.send(Work::Imported {
                                    scan,
                                    playback,
                                    answer,
                                });
                            });
                        }
                        Command::QueueAdd {
                            track: Some(ref id),
                            ..
                        }
                        | Command::Play {
                            track: Some(ref id),
                            ..
                        } => {
                            let result = (|| -> Result<()> {
                                let track = store.track(id)?.ok_or_else(|| {
                                    ApiError::new("track_not_found", "Library track not found")
                                })?;
                                if matches!(command, Command::Play { .. }) {
                                    engine.play_track(track)?;
                                } else {
                                    engine.add(vec![track])?;
                                }
                                Ok(())
                            })();
                            finish_command(result, answer, &mut engine, &store, events)?;
                        }
                        command => {
                            let result = engine.apply(&command);
                            finish_command(result, answer, &mut engine, &store, events)?;
                        }
                    }
                }
            }
            Ok(Work::Catalog { scan, mut job }) => {
                normalization.retry();
                engine.state.scanning = false;
                active_scan = None;
                engine.state.last_error = None;
                job.status = "completed".into();
                job.finished_at_ms = Some(unix_ms());
                match store.save_scan(&job, Some(&scan.records)) {
                    Ok(()) => {
                        let _ = events.send(Event::LibraryChanged);
                    }
                    Err(error) => {
                        job.status = "failed".into();
                        job.error = Some(format!("Cannot save library: {error:#}"));
                        engine.state.last_error = job.error.clone();
                        store.save_scan(&job, None)?;
                    }
                }
                if scan.warning_count > 0 {
                    engine.state.last_error.get_or_insert_with(|| {
                        format!(
                            "Scan completed with {} warning(s): {}",
                            scan.warning_count, scan.warnings[0]
                        )
                    });
                    for warning in &scan.warnings {
                        tracing::warn!("{warning}");
                    }
                }
                let _ = events.send(Event::ScanCompleted(job));
                changed = true;
            }
            Ok(Work::Imported {
                scan,
                playback,
                answer,
            }) => {
                imports = imports.saturating_sub(1);
                let result = (|| -> Result<()> {
                    if scan.records.is_empty() {
                        anyhow::bail!("No supported audio found. {}", scan.warnings.join("; "));
                    }
                    if !scan.warnings.is_empty() {
                        engine.state.last_error = Some(scan.warnings.join("; "));
                    }
                    let tracks: Vec<_> = scan.records.into_iter().map(|r| r.track).collect();
                    if matches!(playback, ImportPlayback::Direct) {
                        if tracks.len() != 1 {
                            anyhow::bail!("Direct playback requires one audio file");
                        }
                        return engine.play_direct(tracks.into_iter().next().unwrap());
                    }
                    let id = engine.add(tracks)?;
                    if matches!(playback, ImportPlayback::Play) {
                        engine.apply(&Command::Play {
                            paths: vec![],
                            track: None,
                            queue_item: id,
                        })?;
                    }
                    Ok(())
                })();
                finish_command(result, answer, &mut engine, &store, events)?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => (),
        }
        archive.poll(&mut store, events);
        let busy = engine.state.scanning || imports > 0;
        if let Err(error) = youtube.poll(&paths, &mut store, &mut engine, &tx, busy, events) {
            tracing::error!("Import scheduler: {error:#}");
        }
        if let Err(error) = covers.poll(&mut store, &mut engine, events) {
            tracing::error!("Cover refresh: {error:#}");
        }
        match normalization.poll(&store, &mut engine) {
            Ok(updated) => changed |= updated,
            Err(error) => tracing::warn!(%error, "Loudness scheduler"),
        }
        // Commands/catalog changes persist their revisions before replying.
        // Capture that position before the next tick advances it again.
        if engine.state.revision != revision_before {
            checkpoint = (engine.state.revision, engine.state.position_ms);
            last_save = Instant::now();
        }
        changed |= engine.tick();
        if changed {
            engine.state.revision += 1;
            store.save(&engine.state)?;
            checkpoint = (engine.state.revision, engine.state.position_ms);
            last_save = Instant::now();
            let _ = events.send(Event::State(engine.state.clone()));
        }
        spectrum.context(engine.state.current_id.as_deref());
        media.update(&engine.state, engine.output_waiting());
        if last_progress.elapsed() >= Duration::from_secs(1) {
            // Heartbeats also let abandoned watch connections be detected while paused.
            let _ = events.send(Event::Progress {
                position_ms: engine.state.position_ms,
                revision: engine.state.revision,
            });
            last_progress = Instant::now();
        }
        if last_save.elapsed() >= Duration::from_secs(5) {
            let current = (engine.state.revision, engine.state.position_ms);
            if current != checkpoint {
                store.save(&engine.state)?;
                checkpoint = current;
            }
            last_save = Instant::now();
        }
    }
    Ok(())
}

fn finish_command(
    result: Result<()>,
    answer: Answer,
    engine: &mut Engine<Box<dyn PlaybackBackend>>,
    store: &Store,
    events: &broadcast::Sender<Event>,
) -> Result<()> {
    engine.state.revision += 1;
    if let Err(error) = &result {
        engine.state.last_error = Some(format!("{error:#}"));
    }
    store.save(&engine.state)?;
    let _ = events.send(Event::State(engine.state.clone()));
    let reply = match result {
        Ok(()) => Reply::success(&engine.state),
        Err(error) => failure(error),
    };
    let _ = answer.send(reply);
    Ok(())
}
fn failure(error: anyhow::Error) -> Reply {
    Reply::failure(
        error
            .downcast_ref::<ApiError>()
            .cloned()
            .unwrap_or_else(|| ApiError::new("operation_failed", format!("{error:#}"))),
    )
}

fn edit_queue<B: crate::audio::PlaybackBackend>(
    engine: &mut Engine<B>,
    store: &mut Store,
    edit: &QueueEdit,
    dry_run: bool,
    expected: Option<u64>,
    request_id: Option<&str>,
) -> Result<Reply> {
    if let Some(id) = request_id
        && (dry_run
            || id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"-_.:".contains(&c)))
    {
        return Err(ApiError::new("invalid_arguments", "Request IDs require 1–128 ASCII letters, digits, -, _, ., or : and cannot be used with dry run").into());
    }
    let payload = serde_json::to_string(&(edit, expected))?;
    let now = unix_ms();
    if let Some(id) = request_id
        && let Some(reply) = store.replay(id, &payload, now)?
    {
        return Ok(reply);
    }
    if let Some(expected) = expected
        && expected != engine.state.queue_revision
    {
        return Err(
            ApiError::new("queue_conflict", "Queue changed; inspect it before editing")
                .with_details(json!({"expected":expected,"actual":engine.state.queue_revision}))
                .into(),
        );
    }
    let (state, changes) = crate::queue_edit::prepare(&engine.state, edit, store)?;
    let reply = Reply::success(
        json!({"applied":!dry_run,"previous_queue_revision":engine.state.queue_revision,
        "queue_revision":state.queue_revision,"changes":changes}),
    );
    if serde_json::to_vec(&reply)?.len() > wire::MAX_FRAME {
        return Err(ApiError::new(
            "response_too_large",
            "Edit response exceeds the transport limit; use smaller batches",
        )
        .into());
    }
    if !dry_run {
        store.commit_edit(
            &state,
            request_id.map(|id| (id, payload.as_str(), &reply)),
            now,
        )?;
        engine.accept_queue_edit(state);
    }
    Ok(reply)
}

#[cfg(test)]
mod agent_tests {
    use super::*;
    use crate::{audio::PlaybackBackend, library::Record};
    use std::path::Path;
    #[derive(Default)]
    struct Silent;
    impl PlaybackBackend for Silent {
        fn load(&mut self, _: &Path, _: u64, _: u8, _: bool) -> Result<()> {
            panic!("Queue edits must never reload output")
        }
        fn pause(&mut self) {
            panic!("Queue edits must not pause output")
        }
        fn resume(&mut self) -> Result<()> {
            panic!("Queue edits must not resume output")
        }
        fn stop(&mut self) {
            panic!("Queue edits must not stop output")
        }
        fn volume(&mut self, _: u8) {
            panic!("Queue edits must not change volume")
        }
        fn seek(&mut self, _: u64) -> Result<()> {
            panic!("Queue edits must not seek")
        }
        fn position(&self) -> u64 {
            0
        }
        fn finished(&self) -> bool {
            false
        }
    }
    fn setup() -> (tempfile::TempDir, Store, Engine<Silent>) {
        let home = tempfile::tempdir().unwrap();
        let mut store = Store::open(&home.path().join("state.db")).unwrap();
        let track = Track {
            id: "song".into(),
            playback: crate::model::PlaybackSource::File {
                path: "/song.wav".into(),
            },
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            track_number: 1,
            duration_ms: Some(10000),
            cover: None,
            video: false,
            source: None,
        };
        store
            .replace_catalog(&[Record {
                track: track.clone(),
                modified: 1,
                bytes: 1,
            }])
            .unwrap();
        let item = QueueItem::new(track);
        let state = State {
            current_id: Some(item.id.clone()),
            queue: vec![item],
            position_ms: 1234,
            status: PlaybackStatus::Playing,
            ..State::default()
        };
        store.save(&state).unwrap();
        (home, store, Engine::new(state, Silent))
    }
    fn add() -> QueueEdit {
        QueueEdit {
            operations: vec![QueueOperation::Add {
                track_ids: vec!["song".into(), "song".into()],
                after_current: true,
                index: None,
            }],
        }
    }
    #[test]
    fn batch_edits_preserve_direct_output_and_continue_after_a_removed_cursor() {
        let (_home, mut store, mut engine) = setup();
        let cursor = engine.state.current_id.clone().unwrap();
        let direct = QueueItem::new(engine.state.queue[0].track.clone());
        engine.state.current_id = Some(direct.id.clone());
        engine.state.direct = Some(Box::new(direct.clone()));
        engine.state.queue_cursor = Some(cursor.clone());
        let position = engine.state.position_ms;
        edit_queue(&mut engine, &mut store, &add(), false, Some(0), None).unwrap();
        assert_eq!(engine.state.queue[0].id, cursor);
        let pending = engine.state.play_next.clone();
        let removal = QueueEdit {
            operations: vec![QueueOperation::Remove {
                queue_item_ids: vec![cursor],
            }],
        };
        edit_queue(&mut engine, &mut store, &removal, false, None, None).unwrap();
        assert!(engine.state.queue_cursor.is_none());
        assert_eq!(engine.state.current(), Some(&direct));
        assert_eq!(engine.state.position_ms, position);
        assert_eq!(engine.state.status, PlaybackStatus::Playing);
        assert_eq!(engine.state.play_next, pending);
        assert_eq!(store.restore().unwrap().current(), Some(&direct));
    }

    #[test]
    fn batch_replay_precedes_revision_check_and_survives_restart() {
        let (home, mut store, mut engine) = setup();
        let original = engine.state.current_id.clone();
        let preview = edit_queue(&mut engine, &mut store, &add(), true, Some(0), None).unwrap();
        assert_eq!(preview.data.unwrap()["applied"], false);
        assert_eq!(engine.state.queue.len(), 1);
        let first = edit_queue(
            &mut engine,
            &mut store,
            &add(),
            false,
            Some(0),
            Some("batch-1"),
        )
        .unwrap();
        assert_eq!(engine.state.queue.len(), 3);
        assert_eq!(engine.state.current_id, original);
        assert_eq!(engine.state.position_ms, 1234);
        assert_eq!(engine.state.status, PlaybackStatus::Playing);
        assert_ne!(engine.state.queue[1].id, engine.state.queue[2].id);
        let replay = edit_queue(
            &mut engine,
            &mut store,
            &add(),
            false,
            Some(0),
            Some("batch-1"),
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(replay).unwrap()
        );
        drop(store);
        let mut store = Store::open(&home.path().join("state.db")).unwrap();
        let mut engine = Engine::new(store.restore().unwrap(), Silent);
        let replay = edit_queue(
            &mut engine,
            &mut store,
            &add(),
            false,
            Some(0),
            Some("batch-1"),
        )
        .unwrap();
        assert_eq!(replay.data, first.data);
        assert_eq!(engine.state.queue.len(), 3);
        assert_eq!(engine.state.play_next.len(), 2);
        assert!(
            edit_queue(&mut engine, &mut store, &add(), false, Some(0), None)
                .unwrap_err()
                .is::<ApiError>()
        );
        let error = edit_queue(
            &mut engine,
            &mut store,
            &add(),
            false,
            Some(1),
            Some("batch-1"),
        )
        .unwrap_err();
        assert_eq!(
            error.downcast_ref::<ApiError>().unwrap().code,
            "request_id_conflict"
        );
    }
    #[test]
    fn invalid_edit_and_storage_failure_leave_engine_and_receipt_unchanged() {
        let (home, mut store, mut engine) = setup();
        let original = serde_json::to_value(&engine.state).unwrap();
        for operation in [
            QueueOperation::Remove {
                queue_item_ids: vec![engine.state.current_id.clone().unwrap()],
            },
            QueueOperation::Move {
                queue_item_id: "missing".into(),
                index: 0,
            },
            QueueOperation::Add {
                track_ids: vec!["missing".into()],
                after_current: false,
                index: None,
            },
        ] {
            let mut edit = add();
            edit.operations.push(operation);
            assert!(
                edit_queue(&mut engine, &mut store, &edit, false, None, Some("failed")).is_err()
            );
            assert_eq!(serde_json::to_value(&engine.state).unwrap(), original);
            assert!(
                store
                    .replay("failed", "unused", unix_ms())
                    .unwrap()
                    .is_none()
            );
        }
        let connection = rusqlite::Connection::open(home.path().join("state.db")).unwrap();
        connection.execute_batch("CREATE TRIGGER fail_session BEFORE UPDATE ON session BEGIN SELECT RAISE(FAIL,'injected failure'); END;").unwrap();
        assert!(
            edit_queue(
                &mut engine,
                &mut store,
                &add(),
                false,
                None,
                Some("db-failed")
            )
            .is_err()
        );
        assert_eq!(serde_json::to_value(&engine.state).unwrap(), original);
        assert!(
            store
                .replay("db-failed", "unused", unix_ms())
                .unwrap()
                .is_none()
        );
        connection
            .execute_batch("DROP TRIGGER fail_session;")
            .unwrap();
        assert!(
            edit_queue(
                &mut engine,
                &mut store,
                &add(),
                false,
                None,
                Some("db-failed")
            )
            .is_ok()
        );
    }
    #[test]
    fn mixed_edits_apply_in_order_and_protect_current_even_when_paused() {
        let (_home, mut store, mut engine) = setup();
        edit_queue(&mut engine, &mut store, &add(), false, None, None).unwrap();
        engine.state.status = PlaybackStatus::Paused;
        let current = engine.state.current_id.clone().unwrap();
        let remove = engine.state.queue[1].id.clone();
        let remaining = engine.state.queue[2].id.clone();
        let edit = QueueEdit {
            operations: vec![
                QueueOperation::Remove {
                    queue_item_ids: vec![remove],
                },
                QueueOperation::Move {
                    queue_item_id: current.clone(),
                    index: 1,
                },
            ],
        };
        edit_queue(&mut engine, &mut store, &edit, false, None, None).unwrap();
        assert_eq!(engine.state.queue[0].id, remaining);
        assert_eq!(engine.state.queue[1].id, current);
        assert_eq!(engine.state.position_ms, 1234);
        assert_eq!(engine.state.status, PlaybackStatus::Paused);
        assert_eq!(engine.state.play_next, vec![remaining]);
    }
}
