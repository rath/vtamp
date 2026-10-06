use crate::{
    client::Client,
    model::{Command, PlaybackStatus, State, display_time},
};
use std::time::Duration;
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

pub async fn status(client: &Client, max_width: usize, show_artist: bool) -> String {
    // Cover connect and reply together; ordinary CLI commands have longer deadlines.
    let state = tokio::time::timeout(Duration::from_millis(500), async {
        let data = client.request(Command::Status).await?.into_data()?;
        Ok::<State, anyhow::Error>(serde_json::from_value(data)?)
    })
    .await;
    match state {
        Ok(Ok(state)) => render(&state, max_width, show_artist),
        _ => String::new(),
    }
}

fn clean(input: &str) -> String {
    input
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn shorten(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut result = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let cells = grapheme.width();
        if used + cells > width - 1 {
            break;
        }
        result.push_str(grapheme);
        used += cells;
    }
    result.push('…');
    result
}

fn render(state: &State, max_width: usize, show_artist: bool) -> String {
    let indicator = match state.status {
        PlaybackStatus::Playing => "▶",
        PlaybackStatus::Paused => "Ⅱ",
        PlaybackStatus::Stopped => return String::new(),
    };
    let Some(item) = state.current() else {
        return String::new();
    };
    let title = clean(&item.track.title);
    let mut label = if title.is_empty() {
        "Unknown title".into()
    } else {
        title
    };
    let artist = clean(&item.track.artist);
    if show_artist && !artist.is_empty() {
        label.push_str(" — ");
        label.push_str(&artist);
    }
    let times = if item.track.is_live() {
        format!(
            " · {}",
            state
                .stream_status
                .map_or("LIVE", crate::model::StreamStatus::label)
        )
    } else {
        format!(
            " · {} / {}",
            display_time(state.position_ms),
            item.track.time_label()
        )
    };
    let reserved = indicator.width() + 1 + times.width();
    if reserved >= max_width {
        return String::new();
    }
    let line = format!(
        "{indicator} {}{times}",
        shorten(&label, max_width - reserved)
    );
    // tmux's status renderer interprets #[...] as styles even in job output.
    // Escape after truncation so the extra quoting bytes do not consume cells.
    line.replace('#', "##")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{QueueItem, Track};

    fn state(title: &str) -> State {
        let item = QueueItem::new(Track {
            id: "track".into(),
            playback: crate::model::PlaybackSource::File {
                path: "track.m4a".into(),
            },
            title: title.into(),
            artist: "Vince DiCola".into(),
            album: String::new(),
            track_number: 0,
            duration_ms: Some(219_103),
            cover: None,
            video: false,
            source: None,
        });
        State {
            current_id: Some(item.id.clone()),
            queue: vec![item],
            status: PlaybackStatus::Playing,
            position_ms: 56_789,
            ..State::default()
        }
    }

    #[test]
    fn playback_states_and_optional_artist() {
        let mut s = state("Training Montage");
        assert_eq!(render(&s, 50, false), "▶ Training Montage · 0:56 / 3:39");
        assert_eq!(
            render(&s, 80, true),
            "▶ Training Montage — Vince DiCola · 0:56 / 3:39"
        );
        s.status = PlaybackStatus::Paused;
        assert_eq!(render(&s, 50, false), "Ⅱ Training Montage · 0:56 / 3:39");
        s.status = PlaybackStatus::Stopped;
        assert!(render(&s, 50, false).is_empty());
        s.status = PlaybackStatus::Playing;
        s.current_id = None;
        assert!(render(&s, 50, false).is_empty());
    }

    #[test]
    fn truncates_display_cells_without_splitting_graphemes_or_times() {
        for title in [
            "a long song title that will not fit",
            "지금은 우리가 멀리 있을지라도",
            "e\u{301}".repeat(40).as_str(),
            "👩‍🚀".repeat(20).as_str(),
        ] {
            let text = render(&state(title), 30, false);
            assert!(text.width() <= 30, "{text}");
            assert!(text.ends_with("… · 0:56 / 3:39"), "{text}");
        }
        assert_eq!(shorten("e\u{301}abcdef", 3), "e\u{301}a…");
    }

    #[test]
    fn metadata_is_single_line_and_cannot_inject_tmux_styles() {
        let text = render(
            &state("A\nB\t#[fg=red] #{pane_id} #(touch /tmp/nope)"),
            100,
            false,
        );
        assert_eq!(
            text,
            "▶ A B ##[fg=red] ##{pane_id} ##(touch /tmp/nope) · 0:56 / 3:39"
        );
        assert_eq!(
            render(&state("\n\t"), 50, false),
            "▶ Unknown title · 0:56 / 3:39"
        );
    }
}
