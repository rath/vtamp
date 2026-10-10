use super::*;
use crate::streams::Entry;

pub(super) enum Dialog {
    Name {
        url: String,
        text: String,
    },
    Preview {
        path: PathBuf,
        entries: Option<Vec<Entry>>,
        error: Option<String>,
        scroll: u16,
    },
    Remove {
        id: String,
        name: String,
    },
}
impl App {
    pub(super) fn playback_label(&self) -> &str {
        if !self.connected {
            return "DISCONNECTED";
        }
        match self.state.status {
            PlaybackStatus::Playing => self
                .state
                .stream_status
                .map_or("PLAYING", StreamStatus::label),
            PlaybackStatus::Paused => {
                if self
                    .state
                    .current()
                    .is_some_and(|item| item.track.is_live())
                {
                    "LIVE · PAUSED"
                } else {
                    "PAUSED"
                }
            }
            PlaybackStatus::Stopped => "STOPPED",
        }
    }
    pub(super) fn stream_input(&mut self, input: &str, commands: &mpsc::Sender<Command>) -> bool {
        let input = input.trim();
        if let Ok(url) = url::Url::parse(input) {
            let host = url.host_str().unwrap_or("");
            if host == "youtu.be"
                || host.ends_with(".youtu.be")
                || host == "youtube.com"
                || host.ends_with(".youtube.com")
            {
                return false;
            }
            if matches!(url.scheme(), "http" | "https") {
                self.stream_dialog = Some(Dialog::Name {
                    url: input.into(),
                    text: String::new(),
                });
                return true;
            }
        }
        let path = PathBuf::from(input);
        if crate::streams::is_playlist(&path) {
            match platform::absolute(&path) {
                Ok(path) => {
                    self.stream_dialog = Some(Dialog::Preview {
                        path: path.clone(),
                        entries: None,
                        error: None,
                        scroll: 0,
                    });
                    self.send(commands, Command::StreamPreview { path });
                }
                Err(error) => self.notice(error.to_string()),
            }
            return true;
        }
        false
    }
    pub(super) fn stream_key(&mut self, key: KeyEvent, commands: &mpsc::Sender<Command>) -> bool {
        let Some(dialog) = &mut self.stream_dialog else {
            return false;
        };
        if let Dialog::Name { text, .. } = dialog
            && edit_line(text, key)
        {
            return true;
        }
        if key.code == KeyCode::Esc {
            self.stream_dialog = None;
            return true;
        }
        match dialog {
            Dialog::Name { url, text } if key.code == KeyCode::Enter => {
                match (Entry {
                    url: url.clone(),
                    name: text.clone(),
                })
                .validated()
                {
                    Ok(entry) => {
                        self.stream_dialog = None;
                        self.send(
                            commands,
                            Command::StreamAdd {
                                entries: vec![entry],
                            },
                        );
                    }
                    Err(error) => self.notice(error.to_string()),
                }
            }
            Dialog::Preview {
                entries, scroll, ..
            } => match key.code {
                KeyCode::Enter if entries.is_some() => {
                    let entries = entries.take().unwrap();
                    self.stream_dialog = None;
                    self.send(commands, Command::StreamAdd { entries });
                }
                KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::PageDown => *scroll = scroll.saturating_add(10),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::Home => *scroll = 0,
                KeyCode::End => *scroll = u16::MAX,
                _ => (),
            },
            Dialog::Remove { id, .. } if key.code == KeyCode::Enter => {
                let id = id.clone();
                self.stream_dialog = None;
                self.send(commands, Command::StreamRemove { id });
            }
            _ => (),
        }
        true
    }
    pub(super) fn stream_reply(
        &mut self,
        command: Command,
        result: Result<Value, String>,
        commands: &mpsc::Sender<Command>,
    ) {
        if let Command::StreamPreview { path } = command {
            if let Some(Dialog::Preview {
                path: pending,
                entries,
                error,
                ..
            }) = &mut self.stream_dialog
                && *pending == path
            {
                match result
                    .and_then(|value| serde_json::from_value(value).map_err(|e| e.to_string()))
                {
                    Ok(value) => *entries = Some(value),
                    Err(message) => *error = Some(message),
                }
            }
            return;
        }
        match result {
            Ok(value) => {
                if matches!(command, Command::StreamAdd { .. }) {
                    let notice = format!(
                        "Added {} channels · {} already registered",
                        value["added"], value["existing"]
                    );
                    self.notice(notice.clone());
                    if let Some(id) = value["tracks"][0]["id"]
                        .as_str()
                        .or_else(|| value["first_registered_id"].as_str())
                    {
                        self.select_library_when_ready(id.into(), notice);
                        return;
                    }
                } else {
                    self.notice("Stream registration removed. Queued copies remain available.");
                }
                self.refresh(commands);
            }
            Err(error) => self.notice(error),
        }
    }
    pub(super) fn draw_stream_dialog(&mut self, frame: &mut Frame, area: Rect, content: Rect) {
        let p = self.theme.palette();
        let Some(dialog) = &mut self.stream_dialog else {
            return;
        };
        self.caret = None;
        if let Dialog::Name { text, .. } = dialog {
            draw_prompt(
                frame,
                p,
                &mut self.caret,
                content,
                " Channel name · Enter add · Esc cancel ",
                text.as_str(),
            );
            return;
        }
        let popup = centered(area, 78, 22);
        let panel = block(p, " LIVE RADIO ", true).style(Style::default().fg(p.text).bg(p.panel));
        let inner = panel.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(panel, popup);
        let [body, footer] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(2)]).areas(inner);
        let hint = match dialog {
            Dialog::Preview {
                entries,
                error,
                scroll,
                ..
            } => {
                let text = if let Some(error) = error {
                    format!("Cannot import playlist\n\n{error}")
                } else if let Some(entries) = entries {
                    format!(
                        "{} channels · no audio will be downloaded\n\n{}",
                        entries.len(),
                        entries
                            .iter()
                            .map(|entry| format!("{}\n{}", entry.name, entry.url))
                            .collect::<Vec<_>>()
                            .join("\n\n")
                    )
                } else {
                    "Reading playlist…".into()
                };
                let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
                let max = paragraph
                    .line_count(body.width)
                    .saturating_sub(body.height as usize)
                    .min(u16::MAX as usize) as u16;
                *scroll = (*scroll).min(max);
                frame.render_widget(paragraph.scroll((*scroll, 0)), body);
                if entries.is_some() {
                    "Enter add all · Esc cancel\n↑/↓ or PgUp/PgDn scroll"
                } else {
                    "Esc close"
                }
            }
            Dialog::Remove { name, .. } => {
                frame.render_widget(Paragraph::new(format!("Remove {name} from Library?\n\nCurrent playback and queued copies will continue.")).wrap(Wrap { trim: false }), body);
                "Enter remove · Esc cancel"
            }
            Dialog::Name { .. } => unreachable!(),
        };
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(p.muted)),
            footer,
        );
    }
    pub(super) fn draw_spectrum(&mut self, frame: &mut Frame, area: Rect, bordered: bool) {
        let p = self.theme.palette();
        if let Some(reason) = self.spectrum.unavailable() {
            let mut paragraph = Paragraph::new(format!("{reason}.\nv closes · Tab lists"))
                .wrap(Wrap { trim: false })
                .style(Style::default().fg(p.muted));
            if bordered {
                paragraph = paragraph.block(block(p, " SPECTRUM ", false));
            }
            frame.render_widget(paragraph, area);
            self.spectrum.notice_drawn();
        } else {
            let font = self.artwork.font_size();
            self.spectrum.draw(
                frame,
                area,
                p,
                bordered,
                self.connected && self.state.status == PlaybackStatus::Playing,
                (font.width, font.height),
            );
        }
    }
}
