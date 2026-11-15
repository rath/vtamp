//! Optional external plugin; build with `cargo build --release --example pastel-transcript`.
//! Adapted from Pastel Sketchbook's vtamp commit d4d0be3, under the MIT license.
mod document;
// Reuse the bounded process helper in this example without making it a public SDK.
#[path = "../../src/subprocess.rs"]
mod subprocess;

use anyhow::{Context as _, Result, bail};
use std::{
    io::{BufRead, Write},
    path::PathBuf,
    sync::{Arc, Mutex, atomic::Ordering},
};
use vtamp::plugin::{
    API_VERSION, Action, Context, HostMessage, Item, MAX_MESSAGE, PluginMessage, View,
};

type Output = Arc<Mutex<std::io::Stdout>>;

fn send(output: &Output, message: PluginMessage) -> Result<()> {
    let mut out = output.lock().unwrap();
    let bytes = serde_json::to_vec(&message)?;
    if bytes.len() + 1 > MAX_MESSAGE {
        bail!("Transcript view exceeds the protocol limit");
    }
    out.write_all(&bytes)?;
    out.write_all(b"\n")?;
    out.flush()?;
    Ok(())
}

fn view(document: &document::Document, context: &Context) -> View {
    let mut offset = 0u64;
    let duration = context.track.as_ref().and_then(|track| track.duration_ms);
    let times = document.starts_ms();
    let items = (0..document.sentence_count())
        .map(|index| {
            let text = document.sentence(index).unwrap_or_default().to_owned();
            let start_ms = times
                .and_then(|times| times.get(index).copied())
                .or_else(|| {
                    duration.map(|duration| {
                        ((offset as u128 * duration as u128) / document.chars().max(1) as u128)
                            as u64
                    })
                });
            offset += text.chars().count() as u64 + 1;
            Item {
                text,
                action: None,
                start_ms,
                end_ms: None,
            }
        })
        .collect();
    View {
        title: context
            .track
            .as_ref()
            .map_or("Transcript", |track| &track.title)
            .into(),
        subtitle: format!(
            "{} · {}",
            if document.is_timed() {
                "Caption timings"
            } else {
                "Estimated timings"
            },
            document.captured().unwrap_or("Published transcript")
        ),
        items,
        actions: vec![Action {
            id: "retry".into(),
            title: "Retry transcript".into(),
        }],
    }
}

async fn load(
    context: Context,
    cache: PathBuf,
    stop: subprocess::Cancel,
    output: Output,
) -> Result<()> {
    let notice = |message: &str| {
        send(
            &output,
            PluginMessage::Notice {
                generation: context.generation,
                message: message.into(),
            },
        )
    };
    let Some(track) = &context.track else {
        return notice("Nothing is playing.");
    };
    let Some(source) = track.source.as_ref().and_then(|source| source.youtube()) else {
        return notice("This track has no YouTube transcript.");
    };
    notice("Loading transcript…")?;
    let markdown = document::fetch(&source.video_id, &cache).await?;
    let mut document = document::parse(&markdown);
    if document.is_empty() {
        return notice("This transcript has no text.");
    }
    if let Some(captions) = document::cached_captions(&cache, &source.video_id) {
        document.attach_captions(&captions);
    }
    send(
        &output,
        PluginMessage::View {
            generation: context.generation,
            view: view(&document, &context),
        },
    )?;
    if !document.is_timed()
        && let Ok(Some(captions)) =
            document::fetch_captions(&source.video_id, document.prose(), &cache, stop).await
        && document.attach_captions(&captions)
    {
        send(
            &output,
            PluginMessage::View {
                generation: context.generation,
                view: view(&document, &context),
            },
        )?;
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let (input, mut messages) = tokio::sync::mpsc::channel(8);
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        let mut reader = stdin.lock();
        loop {
            let mut bytes = Vec::new();
            loop {
                let Ok(buffer) = reader.fill_buf() else {
                    return;
                };
                if buffer.is_empty() {
                    return;
                }
                let size = buffer
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(buffer.len(), |at| at + 1);
                if bytes.len() + size > MAX_MESSAGE {
                    return;
                }
                bytes.extend_from_slice(&buffer[..size]);
                reader.consume(size);
                if bytes.last() == Some(&b'\n') {
                    break;
                }
            }
            let message = serde_json::from_slice::<HostMessage>(&bytes);
            if input.blocking_send(message).is_err() {
                return;
            }
        }
    });
    let output = Arc::new(Mutex::new(std::io::stdout()));
    let mut cache = None;
    let mut current: Option<Context> = None;
    let mut worker: Option<tokio::task::JoinHandle<()>> = None;
    let mut stop = subprocess::cancel();
    while let Some(message) = messages.recv().await {
        let context = match message? {
            HostMessage::Init {
                api_version: API_VERSION,
                cache_dir,
                ..
            } => {
                cache = Some(cache_dir);
                send(
                    &output,
                    PluginMessage::Ready {
                        api_version: API_VERSION,
                    },
                )?;
                continue;
            }
            HostMessage::Init { .. } => bail!("Unsupported host API"),
            HostMessage::Invoke { command, context } => {
                if command != "show" {
                    bail!("Unknown command");
                }
                context
            }
            HostMessage::Context { context } => {
                if current
                    .as_ref()
                    .is_some_and(|old| old.generation == context.generation)
                {
                    continue;
                }
                context
            }
            HostMessage::Action { id, generation } => {
                if id != "retry"
                    || current
                        .as_ref()
                        .is_none_or(|old| old.generation != generation)
                {
                    continue;
                }
                current.clone().unwrap()
            }
            HostMessage::Shutdown => break,
        };
        stop.store(true, Ordering::Relaxed);
        if let Some(worker) = worker.take() {
            worker.abort();
        }
        stop = subprocess::cancel();
        current = Some(context.clone());
        let cache = cache
            .clone()
            .context("Host did not initialize the plugin")?;
        let stopping = stop.clone();
        let output = output.clone();
        worker = Some(tokio::spawn(async move {
            let generation = context.generation;
            if let Err(error) = load(context, cache, stopping, output.clone()).await {
                let _ = send(
                    &output,
                    PluginMessage::Notice {
                        generation,
                        message: format!("{error:#}"),
                    },
                );
            }
        }));
    }
    stop.store(true, Ordering::Relaxed);
    if let Some(worker) = worker {
        worker.abort();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_caption_starts_are_sent_as_generic_timed_items() {
        let mut document = document::parse(document::SAMPLE);
        assert!(document.attach_captions(document::SAMPLE_CAPTIONS));
        let context = Context::from_state(
            &vtamp::model::State::default(),
            false,
            vtamp::plugin::Target::Playing,
            None,
        );
        let view = view(&document, &context);
        view.validate().unwrap();
        assert_eq!(view.items.len(), document.sentence_count());
        assert_eq!(view.items[0].start_ms, Some(320));
        assert!(!view.items[0].text.contains("Transcript"));
    }
}
