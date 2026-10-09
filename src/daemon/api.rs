//! A plain HTTP API for apps on a private network, served only with `--api`.
//! `POST /api/rpc` carries the socket protocol's `Request` and `Reply` JSON
//! unchanged; the file routes return a catalog track's audio, cover, or saved
//! video by track ID. Nothing from a URL reaches the filesystem, and commands that take
//! server paths or hold a connection open stay on the local socket. TLS and
//! authentication belong to the network in front of it, as for the HTTP cast.
mod body;
mod range;

use super::{Served, Work, dispatch};
use crate::model::*;
use anyhow::{Context as _, Result};
use body::FileBody;
use bytes::Bytes;
use http::{HeaderValue, Method, StatusCode, header};
use http_body_util::{BodyExt, Empty, Full, LengthLimitError, Limited, combinators::BoxBody};
use hyper::{body::Incoming, server::conn::http1, service::service_fn};
use hyper_util::rt::{TokioIo, TokioTimer};
use range::Range;
use std::{
    convert::Infallible,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, mpsc},
    time::{Duration, UNIX_EPOCH},
};
use tokio::{io::AsyncSeekExt, net::TcpListener, sync::Semaphore};

const MAX_CONNECTIONS: usize = 64;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
/// Also closes idle keep-alive connections, returning their permits.
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);

type ApiBody = BoxBody<Bytes, std::io::Error>;
type Response = http::Response<ApiBody>;

pub(super) struct Context {
    pub(super) sender: mpsc::SyncSender<Work>,
    pub(super) served: Arc<Served>,
}

pub(super) struct Api {
    pub(super) url: String,
    listener: TcpListener,
}

impl Api {
    /// Bind now, so the reported URL carries the actual port.
    pub(super) async fn bind(address: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(address)
            .await
            .with_context(|| format!("Cannot listen for API clients on {address}"))?;
        Ok(Self {
            url: format!("http://{}/api", listener.local_addr()?),
            listener,
        })
    }

    pub(super) async fn serve(self, context: Arc<Context>) {
        let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        loop {
            let (stream, peer) = match self.listener.accept().await {
                Ok(accepted) => accepted,
                Err(error) => {
                    tracing::warn!("API accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            let Ok(permit) = permits.clone().try_acquire_owned() else {
                continue;
            };
            let context = context.clone();
            tokio::spawn(async move {
                let _permit = permit;
                let service = service_fn(move |request| {
                    let context = context.clone();
                    async move { Ok::<_, Infallible>(route(&context, request).await) }
                });
                let connection = http1::Builder::new()
                    .timer(TokioTimer::new())
                    .header_read_timeout(HEADER_TIMEOUT)
                    .serve_connection(TokioIo::new(stream), service);
                if let Err(error) = connection.await {
                    tracing::debug!(%peer, "API client left: {error}");
                }
            });
        }
    }
}

#[derive(Clone, Copy)]
enum Asset {
    Audio,
    Cover,
    Video,
}

async fn route(context: &Context, request: http::Request<Incoming>) -> Response {
    let path = request.uri().path().to_owned();
    let segments: Vec<&str> = match path.strip_prefix("/api/") {
        Some(rest) => rest.split('/').collect(),
        None => vec![],
    };
    let method = request.method().clone();
    match segments.as_slice() {
        ["server"] => match method {
            Method::GET => server(context).await,
            _ => method_not_allowed("GET"),
        },
        ["rpc"] => match method {
            Method::POST => rpc(context, request).await,
            _ => method_not_allowed("POST"),
        },
        ["library", id, asset @ ("audio" | "cover" | "video")] if valid_id(id) => match method {
            Method::GET | Method::HEAD => {
                let asset = match *asset {
                    "audio" => Asset::Audio,
                    "cover" => Asset::Cover,
                    _ => Asset::Video,
                };
                file(context, &request, id, asset).await
            }
            _ => method_not_allowed("GET, HEAD"),
        },
        _ => failure(StatusCode::NOT_FOUND, "not_found", "No such API route"),
    }
}

/// Track IDs are UUIDs; anything else cannot name a catalog row.
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

async fn server(context: &Context) -> Response {
    let import_available = dispatch(&context.sender, Command::ImportAvailable)
        .await
        .into_data()
        .is_ok_and(|data| data["available"] == true);
    let mut data =
        serde_json::to_value(context.served.server_info()).expect("server information serializes");
    data["protocol_version"] = PROTOCOL_VERSION.into();
    data["import_available"] = import_available.into();
    data["cast"] = serde_json::to_value(context.served.cast_info()).expect("cast info serializes");
    json(StatusCode::OK, &Reply::success(data))
}

async fn rpc(context: &Context, request: http::Request<Incoming>) -> Response {
    let body = match Limited::new(request.into_body(), MAX_REQUEST_BYTES)
        .collect()
        .await
    {
        Ok(body) => body.to_bytes(),
        Err(error) if error.is::<LengthLimitError>() => {
            return failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid_request",
                "Requests are limited to 1 MiB",
            );
        }
        Err(error) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("Cannot read the request: {error}"),
            );
        }
    };
    let request: Request = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return failure(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                format!("Expected a JSON request: {error}"),
            );
        }
    };
    let reply = if request.version != PROTOCOL_VERSION {
        Reply::failure(ApiError::new(
            "version_mismatch",
            "Client and server protocol versions differ; update the client or the server",
        ))
    } else if let Err(error) = allowed(&request.request) {
        Reply::failure(error)
    } else {
        match request.request {
            // The worker would treat these as unknown playback commands.
            Command::ServerInfo => Reply::success(context.served.server_info()),
            Command::CastInfo => Reply::success(context.served.cast_info()),
            command => dispatch(&context.sender, command).await,
        }
    };
    json(StatusCode::OK, &reply)
}

/// Subscriptions need the socket, and so do commands naming server paths:
/// over HTTP, files are chosen by track ID only.
fn allowed(command: &Command) -> Result<(), ApiError> {
    let local = |message: &str| Err(ApiError::new("not_available_over_http", message));
    match command {
        Command::Watch | Command::SpectrumWatch | Command::CastWatch | Command::Shutdown => {
            local("This command is only available on the local socket")
        }
        Command::ArchiveImport { .. }
        | Command::StreamPreview { .. }
        | Command::LibraryAdd { .. }
        | Command::LibraryRemove { .. }
        | Command::PlayDirect { path: Some(_), .. } => {
            local("Server paths are not accepted over HTTP; use the local socket")
        }
        Command::Play { paths, .. } | Command::QueueAdd { paths, .. } if !paths.is_empty() => {
            local("Server paths are not accepted over HTTP; use track IDs")
        }
        _ => Ok(()),
    }
}

async fn file(
    context: &Context,
    request: &http::Request<Incoming>,
    id: &str,
    asset: Asset,
) -> Response {
    let track: Track = match dispatch(&context.sender, Command::LibraryTrack { id: id.to_owned() })
        .await
        .into_data()
        .map_err(|error| error.code)
        .and_then(|data| serde_json::from_value(data).map_err(|_| "protocol_error".to_owned()))
    {
        Ok(track) => track,
        Err(code) if code == "track_not_found" => {
            return failure(
                StatusCode::NOT_FOUND,
                "track_not_found",
                "No such library track",
            );
        }
        Err(code) => {
            return failure(
                StatusCode::SERVICE_UNAVAILABLE,
                &code,
                "Cannot look up the track; retry later",
            );
        }
    };
    let path: PathBuf = match asset {
        Asset::Audio => match track.playback.file() {
            Some(path) => path.to_owned(),
            None => {
                return failure(
                    StatusCode::NOT_FOUND,
                    "not_a_file",
                    "Radio channels have no file to download",
                );
            }
        },
        Asset::Cover => match track.cover {
            Some(cover) => cover,
            None => {
                return failure(
                    StatusCode::NOT_FOUND,
                    "cover_not_found",
                    "This track has no cover",
                );
            }
        },
        Asset::Video => match crate::video::sidecar(&track) {
            Some(video) => video,
            None if track.playback.file().is_none() => {
                return failure(
                    StatusCode::NOT_FOUND,
                    "not_a_file",
                    "Radio channels have no file to download",
                );
            }
            None => {
                return failure(
                    StatusCode::NOT_FOUND,
                    "video_not_found",
                    "This track has no saved video",
                );
            }
        },
    };
    serve_file(request, path).await
}

async fn serve_file(request: &http::Request<Incoming>, path: PathBuf) -> Response {
    let missing = || {
        failure(
            StatusCode::NOT_FOUND,
            "file_not_found",
            "The file is not available",
        )
    };
    let Ok(mut file) = tokio::fs::File::open(&path).await else {
        return missing();
    };
    let metadata = match file.metadata().await {
        Ok(metadata) if metadata.is_file() => metadata,
        _ => return missing(),
    };
    let len = metadata.len();
    let modified = metadata.modified().ok();
    let modified_secs = modified
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |since| since.as_secs());
    let etag = body::etag(len, modified_secs);
    let mut response = http::Response::builder()
        .header(header::ETAG, &etag)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, no-cache")
        .header(header::CONTENT_TYPE, body::content_type(&path));
    if let Some(modified) = modified {
        response = response.header(header::LAST_MODIFIED, httpdate::fmt_http_date(modified));
    }
    let text = |name: header::HeaderName| {
        request
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
    };
    if text(header::IF_NONE_MATCH).is_some_and(|header| body::if_none_match(header, &etag)) {
        return build(response.status(StatusCode::NOT_MODIFIED), empty());
    }
    // A range for another version of the file would splice two versions together.
    let range_header = match text(header::IF_RANGE) {
        Some(validator) if validator.trim() != etag => None,
        _ => text(header::RANGE),
    };
    let (status, start, length) = match range::resolve(range_header, len) {
        Range::Full => (StatusCode::OK, 0, len),
        Range::Partial { start, end } => {
            response = response.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
            (StatusCode::PARTIAL_CONTENT, start, end - start + 1)
        }
        Range::Unsatisfiable => {
            return build(
                response
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                    .header(header::CONTENT_LENGTH, 0),
                empty(),
            );
        }
    };
    let response = response
        .status(status)
        .header(header::CONTENT_LENGTH, length);
    if request.method() == Method::HEAD {
        return build(response, empty());
    }
    if start > 0 && file.seek(std::io::SeekFrom::Start(start)).await.is_err() {
        return missing();
    }
    build(response, FileBody::new(file, length).boxed())
}

fn build(response: http::response::Builder, body: ApiBody) -> Response {
    response
        .body(body)
        .expect("API responses use valid static headers")
}

fn empty() -> ApiBody {
    Empty::new().map_err(|never| match never {}).boxed()
}

fn json(status: StatusCode, value: &impl serde::Serialize) -> Response {
    let bytes = serde_json::to_vec(value).expect("API replies serialize");
    build(
        http::Response::builder()
            .status(status)
            .header(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json"),
            )
            .header(header::CACHE_CONTROL, "no-store"),
        Full::new(Bytes::from(bytes))
            .map_err(|never| match never {})
            .boxed(),
    )
}

fn failure(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    json(status, &Reply::failure(ApiError::new(code, message)))
}

fn method_not_allowed(allow: &'static str) -> Response {
    let mut response = failure(
        StatusCode::METHOD_NOT_ALLOWED,
        "method_not_allowed",
        format!("Use {allow}"),
    );
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static(allow));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(json: serde_json::Value) -> Command {
        serde_json::from_value(json).unwrap()
    }

    #[test]
    fn streams_lifetime_and_server_paths_stay_local() {
        for denied in [
            serde_json::json!({"command":"watch"}),
            serde_json::json!({"command":"cast_watch"}),
            serde_json::json!({"command":"spectrum_watch"}),
            serde_json::json!({"command":"shutdown"}),
            serde_json::json!({"command":"library_add","path":"/"}),
            serde_json::json!({"command":"library_remove","path":"/"}),
            serde_json::json!({"command":"archive_import","path":"/a.tar.gz"}),
            serde_json::json!({"command":"stream_preview","path":"/a.m3u"}),
            serde_json::json!({"command":"play_direct","path":"/a.m4a"}),
            serde_json::json!({"command":"play","paths":["/a.m4a"],"track":null,"queue_item":null}),
            serde_json::json!({"command":"queue_add","paths":["/a.m4a"],"track":null}),
        ] {
            let error = allowed(&command(denied.clone())).unwrap_err();
            assert_eq!(error.code, "not_available_over_http", "{denied}");
        }
        for permitted in [
            serde_json::json!({"command":"library_list","query":"","offset":0,"limit":10}),
            serde_json::json!({"command":"play","paths":[],"track":"T","queue_item":null}),
            serde_json::json!({"command":"queue_add","paths":[],"track":"T"}),
            serde_json::json!({"command":"play_direct","track":"T"}),
            serde_json::json!({"command":"import_start","request":{"url":"https://youtu.be/aaaaaaaaaaa"}}),
            serde_json::json!({"command":"status"}),
        ] {
            assert!(allowed(&command(permitted.clone())).is_ok(), "{permitted}");
        }
    }

    #[test]
    fn only_uuid_like_ids_name_tracks() {
        assert!(valid_id("8b3f4a51-2c1e-4f7a-9d0b-1e2f3a4b5c6d"));
        assert!(!valid_id(""));
        assert!(!valid_id(".."));
        assert!(!valid_id("a%2Fb"));
        assert!(!valid_id(&"a".repeat(65)));
    }
}
