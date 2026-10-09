//! File responses: content types, validators, and a body that streams the
//! requested bytes in bounded chunks instead of reading the file whole.
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use std::{
    io,
    path::Path,
    pin::Pin,
    task::{Context, Poll, ready},
};
use tokio::io::{AsyncRead, ReadBuf};

const CHUNK: usize = 64 * 1024;

pub fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some("m4a" | "mp4") => "audio/mp4",
        Some("aac") => "audio/aac",
        Some("mp3") => "audio/mpeg",
        Some("flac") => "audio/flac",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        _ => "application/octet-stream",
    }
}

/// A strong validator: a rewritten file changes its length or modification time.
pub fn etag(len: u64, modified_secs: u64) -> String {
    format!("\"{len:x}-{modified_secs:x}\"")
}

/// Whether an `If-None-Match` header names this entity. Weak comparison, as
/// RFC 9110 §13.1.2 requires for this header.
pub fn if_none_match(header: &str, etag: &str) -> bool {
    header.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

/// `remaining` bytes from the file's current position.
pub struct FileBody {
    file: tokio::fs::File,
    remaining: u64,
    buffer: Box<[u8]>,
}

impl FileBody {
    pub fn new(file: tokio::fs::File, remaining: u64) -> Self {
        Self {
            file,
            remaining,
            buffer: vec![0; CHUNK].into_boxed_slice(),
        }
    }
}

impl Body for FileBody {
    type Data = Bytes;
    type Error = io::Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            return Poll::Ready(None);
        }
        let want = this.buffer.len().min(this.remaining as usize);
        let mut buffer = ReadBuf::new(&mut this.buffer[..want]);
        ready!(Pin::new(&mut this.file).poll_read(cx, &mut buffer))?;
        let read = buffer.filled();
        if read.is_empty() {
            return Poll::Ready(Some(Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the file became shorter while it was being sent",
            ))));
        }
        this.remaining -= read.len() as u64;
        Poll::Ready(Some(Ok(Frame::data(Bytes::copy_from_slice(read)))))
    }

    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type(Path::new("/a/audio.m4a")), "audio/mp4");
        assert_eq!(content_type(Path::new("/a/Song.MP3")), "audio/mpeg");
        assert_eq!(content_type(Path::new("/a/x.flac")), "audio/flac");
        assert_eq!(content_type(Path::new("/a/cover.JPG")), "image/jpeg");
        assert_eq!(content_type(Path::new("/a/1.png")), "image/png");
        assert_eq!(
            content_type(Path::new("/a/README")),
            "application/octet-stream"
        );
    }

    #[test]
    fn if_none_match_accepts_lists_weak_tags_and_wildcards() {
        let tag = etag(4096, 1_700_000_000);
        assert_eq!(tag, "\"1000-6553f100\"");
        assert!(if_none_match(&tag, &tag));
        assert!(if_none_match(&format!("\"x\", W/{tag}"), &tag));
        assert!(if_none_match("*", &tag));
        assert!(!if_none_match("\"1000-0\"", &tag));
    }

    #[tokio::test]
    async fn file_body_streams_exactly_the_remaining_bytes() {
        use http_body_util::BodyExt;
        use tokio::io::AsyncSeekExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        let bytes: Vec<u8> = (0..200_000u32).map(|i| i as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let mut file = tokio::fs::File::open(&path).await.unwrap();
        file.seek(io::SeekFrom::Start(10)).await.unwrap();
        let body = FileBody::new(file, 150_000);
        assert_eq!(body.size_hint().exact(), Some(150_000));
        let collected = body.collect().await.unwrap().to_bytes();
        assert_eq!(&collected[..], &bytes[10..150_010]);
    }
}
