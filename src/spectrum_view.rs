//! Terminal-only animation and drawing; no playback controls.
mod axis;
mod braille;
mod fire;
mod radial;
mod ridge;
mod smooth;
mod sparks;
mod squares;
mod stereo;
mod trail;

use crate::{
    settings::SpectrumStyle,
    spectrum::{BANDS, SpectrumFrame},
    theme::{Palette, spectrum_gradient},
};
use fire::Fire;
use radial::Radial;
use rand::rngs::SmallRng;
use ratatui::{
    Frame,
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use sparks::Sparks;
use std::{
    collections::VecDeque,
    ops::Range,
    time::{Duration, Instant},
};
use stereo::Stereo;

const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
/// Rows the waterfall and ridge remember; more than any pane shows, bounded for memory.
const HISTORY: usize = 256;
/// Frames older than this are stale: bars fall and nothing new is detected.
const LIVE: Duration = Duration::from_millis(300);

/// The bars geometry shared by the bar styles and sparks.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BarKind {
    Zoned,
    Gradient,
    Mono,
    Mirror,
    Dots,
}

/// Styles drawn from the frame history; they change only when a frame arrives.
fn follows_frames(style: SpectrumStyle) -> bool {
    matches!(
        style,
        SpectrumStyle::Waterfall | SpectrumStyle::Ridge | SpectrumStyle::Trail
    )
}

/// Bands shown by item `index` of `count` across the spectrum: neighbors merge when
/// there are fewer items than bands, and a band repeats when there are more.
fn bands(index: usize, count: usize) -> Range<usize> {
    let start = index * BANDS / count;
    start..((index + 1) * BANDS / count).max(start + 1)
}

/// The loudest value in a span of bands.
fn merged(values: &[f32; BANDS], bands: Range<usize>) -> f32 {
    values[bands].iter().copied().fold(0.0, f32::max)
}

/// The band values joined by straight segments between band centers and flat beyond the
/// outer centers. Fewer dot columns than bands merge them instead.
fn sample_at(levels: &[f32; BANDS], x: usize, width: usize) -> f32 {
    if width < BANDS {
        return merged(levels, bands(x, width)).clamp(0.0, 1.0);
    }
    let position = ((x as f32 + 0.5) * BANDS as f32 / width as f32 - 0.5).max(0.0);
    let band = (position as usize).min(BANDS - 1);
    let next = (band + 1).min(BANDS - 1);
    let t = (position - band as f32).min(1.0);
    (levels[band] + (levels[next] - levels[band]) * t).clamp(0.0, 1.0)
}

/// One bar: the bands it merges and its body columns.
struct Bar {
    bands: Range<usize>,
    columns: Range<u16>,
}

/// Bars run from low to high bands, centered in the body with one blank column between
/// neighbors; narrow bodies merge neighboring bands.
fn bar_layout(width: u16) -> impl Iterator<Item = Bar> {
    let width = usize::from(width);
    let count = BANDS.min(width.div_ceil(2));
    let step = (width + 1) / count.max(1);
    let bar = step.saturating_sub(1).max(1);
    let offset = (width + step - bar).saturating_sub(count * step) / 2;
    (0..count).map(move |index| {
        let x = (offset + index * step) as u16;
        Bar {
            bands: bands(index, count),
            columns: x..x + bar as u16,
        }
    })
}

pub(crate) struct SpectrumView {
    pub enabled: bool,
    pub error: Option<String>,
    style: SpectrumStyle,
    frame: Option<SpectrumFrame>,
    received: Instant,
    updated: Instant,
    levels: [f32; BANDS],
    peaks: [f32; BANDS],
    hold: [Instant; BANDS],
    /// Raw levels of recent active frames, oldest first; the waterfall and ridge draw these.
    history: VecDeque<[f32; BANDS]>,
    /// Frames pushed to `history` since it was cleared; ridge lines keep their place by it.
    pushed: u64,
    fire: Fire,
    radial: Radial,
    sparks: Sparks,
    stereo: Stereo,
    rng: SmallRng,
    redraw: bool,
}
impl SpectrumView {
    pub fn new(enabled: bool, style: SpectrumStyle) -> Self {
        let now = Instant::now();
        Self {
            enabled,
            error: None,
            style,
            frame: None,
            received: now,
            updated: now,
            levels: [0.0; BANDS],
            peaks: [0.0; BANDS],
            hold: [now; BANDS],
            history: VecDeque::with_capacity(HISTORY),
            pushed: 0,
            fire: Fire::default(),
            radial: Radial::default(),
            sparks: Sparks::default(),
            stereo: Stereo::default(),
            rng: rand::make_rng(),
            redraw: true,
        }
    }
    pub fn style(&self) -> SpectrumStyle {
        self.style
    }
    /// Switches the rendering; levels, peaks, and history carry over, while fire heat,
    /// radial waves, and sparks start over.
    pub fn set_style(&mut self, style: SpectrumStyle) {
        self.style = style;
        self.reset_effects();
        self.redraw = true;
    }
    pub fn clear(&mut self) {
        self.frame = None;
        self.error = None;
        self.history.clear();
        self.pushed = 0;
        self.reset_levels();
    }
    /// Drops the bar state for a new logical stream (seek, pause, resume) while
    /// keeping the waterfall's past, which was genuinely heard.
    fn reset_levels(&mut self) {
        self.levels.fill(0.0);
        self.peaks.fill(0.0);
        self.reset_effects();
        self.redraw = true;
    }
    fn reset_effects(&mut self) {
        self.fire.reset();
        self.radial.reset();
        self.sparks.reset();
        self.stereo.reset();
    }
    pub fn needs_animation(&self, playing: bool) -> bool {
        if follows_frames(self.style) {
            // Rows only appear with frames; nothing moves between them.
            return self.redraw;
        }
        self.redraw
            || (self.error.is_none()
                && (self.fire.is_hot()
                    || self.radial.is_active()
                    || self.sparks.is_active()
                    || (self.style == SpectrumStyle::Stereo && self.stereo.is_active())
                    || self.levels.iter().chain(&self.peaks).any(|v| *v > 0.0)
                    || (playing
                        && self.received.elapsed() < LIVE
                        && self
                            .frame
                            .as_ref()
                            .is_some_and(|f| f.active && f.levels.iter().any(|v| *v > 0.0)))))
    }
    pub fn accept(&mut self, frame: SpectrumFrame) {
        if self
            .frame
            .as_ref()
            .is_some_and(|f| f.generation > frame.generation)
        {
            return;
        }
        let (new_track, new_generation) = self.frame.as_ref().map_or((false, false), |current| {
            (
                current.current_id != frame.current_id,
                current.generation != frame.generation,
            )
        });
        if new_track {
            self.clear();
        } else if new_generation {
            self.reset_levels();
        }
        self.redraw |= self.error.is_some();
        if frame.active {
            if self.history.len() == HISTORY {
                self.history.pop_front();
            }
            self.history.push_back(frame.levels);
            self.pushed += 1;
            self.redraw |= follows_frames(self.style);
            // Onsets compare two consecutive frames of one stream; a reset or a gap
            // starts the comparison over.
            if self.received.elapsed() < LIVE
                && let Some(previous) = self
                    .frame
                    .as_ref()
                    .filter(|f| f.active && f.generation == frame.generation)
            {
                match self.style {
                    SpectrumStyle::Radial => self.radial.observe(&previous.levels, &frame.levels),
                    SpectrumStyle::Sparks => self.sparks.observe(&previous.levels, &frame.levels),
                    _ => {}
                }
            }
        }
        self.frame = Some(frame);
        self.error = None;
        self.received = Instant::now();
    }
    /// Why the server cannot analyze the current entry, while it produces nothing.
    pub fn unavailable(&self) -> Option<&str> {
        self.frame
            .as_ref()
            .filter(|frame| !frame.active)
            .and_then(|frame| frame.unavailable.as_deref())
    }
    /// The panel showed a fixed message instead of the graph: consume the pending
    /// redraw and leave nothing to animate, so the animation timer settles.
    pub fn notice_drawn(&mut self) {
        self.reset_levels();
        self.redraw = false;
    }
    /// Draws the panel; `cell` is the terminal cell size in pixels, which keeps the
    /// radial style round.
    pub fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        p: Palette,
        bordered: bool,
        playing: bool,
        cell: (u16, u16),
    ) {
        self.redraw = false;
        let inner = self.header(frame, area, &p, bordered);
        if inner.width == 0 || inner.height == 0 {
            return;
        }
        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(error.as_str())
                    .style(Style::default().fg(p.muted))
                    .wrap(ratatui::widgets::Wrap { trim: true }),
                inner,
            );
            return;
        }
        let body = Rect {
            height: inner.height.saturating_sub(1),
            ..inner
        };
        let dt = if follows_frames(self.style) {
            0.0
        } else {
            self.advance(playing)
        };
        let buf = frame.buffer_mut();
        match self.style {
            SpectrumStyle::Bars => self.draw_bars(buf, body, &p, BarKind::Zoned),
            SpectrumStyle::Gradient => self.draw_bars(buf, body, &p, BarKind::Gradient),
            SpectrumStyle::Mono => self.draw_bars(buf, body, &p, BarKind::Mono),
            SpectrumStyle::Mirror => self.draw_bars(buf, body, &p, BarKind::Mirror),
            SpectrumStyle::Dots => self.draw_bars(buf, body, &p, BarKind::Dots),
            SpectrumStyle::Squares => squares::draw(buf, body, &p, &self.levels, &self.peaks),
            SpectrumStyle::Smooth if smooth::fits(body) => {
                smooth::draw(buf, body, &p, &self.levels)
            }
            SpectrumStyle::Trail => trail::draw(buf, body, &p, &self.history),
            SpectrumStyle::Stereo if stereo::fits(body) && self.has_channels() => {
                self.stereo.draw(buf, body, &p)
            }
            SpectrumStyle::Smooth | SpectrumStyle::Stereo => {
                self.draw_bars(buf, body, &p, BarKind::Zoned)
            }
            SpectrumStyle::Waterfall => self.draw_waterfall(buf, body, &p),
            SpectrumStyle::Radial => {
                self.radial.advance(dt);
                self.radial
                    .draw(buf, body, &p, &self.levels, &self.peaks, cell);
            }
            SpectrumStyle::Fire => {
                self.fire
                    .draw(buf, body, &p, &self.levels, dt, &mut self.rng);
            }
            SpectrumStyle::Ridge => ridge::draw(buf, body, &p, &self.history, self.pushed),
            SpectrumStyle::Sparks => {
                self.draw_bars(buf, body, &p, BarKind::Zoned);
                self.sparks
                    .draw(buf, body, &p, &self.levels, dt, &mut self.rng);
            }
        }
        let (graph, mapping) = match self.style {
            SpectrumStyle::Radial => (body, axis::Mapping::Ends),
            SpectrumStyle::Stereo if stereo::fits(body) && self.has_channels() => {
                (stereo::graph(body), axis::Mapping::Bars)
            }
            SpectrumStyle::Smooth if smooth::fits(body) => (body, axis::Mapping::Continuous),
            SpectrumStyle::Waterfall | SpectrumStyle::Fire | SpectrumStyle::Ridge => {
                (body, axis::Mapping::Continuous)
            }
            _ => (body, axis::Mapping::Bars),
        };
        axis::draw(buf, graph, body.bottom(), &p, mapping, self.frame.as_ref());
    }

    fn has_channels(&self) -> bool {
        self.frame
            .as_ref()
            .is_some_and(|frame| frame.channels.is_some())
    }

    fn header(&self, frame: &mut Frame, area: Rect, p: &Palette, bordered: bool) -> Rect {
        let name = if self.style == SpectrumStyle::Stereo && !self.has_channels() {
            "stereo unavailable"
        } else {
            self.style.id()
        };
        if bordered {
            let full = format!(" SPECTRUM · {name} · v close · V style ");
            let title =
                if Line::from(full.as_str()).width() <= usize::from(area.width).saturating_sub(2) {
                    full
                } else if self.style == SpectrumStyle::Stereo
                    && !self.has_channels()
                    && area.width >= 32
                {
                    " SPECTRUM · stereo unavailable ".to_string()
                } else {
                    " SPECTRUM · v close ".to_string()
                };
            let block = Block::default()
                .borders(Borders::ALL)
                .title(Line::styled(title, Style::default().fg(p.muted)))
                .border_style(Style::default().fg(p.border));
            let inner = block.inner(area);
            frame.render_widget(block, area);
            inner
        } else {
            frame.render_widget(
                Paragraph::new(format!("SPECTRUM · {name}")).style(Style::default().fg(p.muted)),
                Rect {
                    height: area.height.min(1),
                    ..area
                },
            );
            Rect {
                y: area.y.saturating_add(1),
                height: area.height.saturating_sub(1),
                ..area
            }
        }
    }

    /// Bar decay and peak hold, shared by every style with falling peaks. Returns the
    /// seconds it applied, which also drive fire, radial waves, and sparks.
    fn advance(&mut self, playing: bool) -> f32 {
        let now = Instant::now();
        let dt = now.duration_since(self.updated).as_secs_f32().min(0.2);
        self.updated = now;
        let live = playing && self.received.elapsed() < LIVE;
        for i in 0..BANDS {
            let target = self
                .frame
                .as_ref()
                .filter(|f| live && f.active)
                .map_or(0.0, |f| f.levels[i].clamp(0.0, 1.0));
            self.levels[i] = target.max(self.levels[i] - dt * 1.8);
            if self.levels[i] >= self.peaks[i] {
                self.peaks[i] = self.levels[i];
                self.hold[i] = now + Duration::from_millis(180);
            } else if now >= self.hold[i] {
                self.peaks[i] = self.levels[i].max(self.peaks[i] - dt * 0.8);
            }
        }
        if self.style == SpectrumStyle::Stereo {
            let target = self
                .frame
                .as_ref()
                .filter(|f| live && f.active)
                .and_then(|f| f.channels.as_ref());
            self.stereo.advance(target, dt);
        }
        dt
    }

    fn draw_bars(&self, buf: &mut Buffer, body: Rect, p: &Palette, kind: BarKind) {
        let height = body.height;
        let rows = if kind == BarKind::Mirror {
            height & !1
        } else {
            height
        };
        let half = rows / 2;
        let scale = f32::from(if kind == BarKind::Mirror { half } else { rows });
        let bottom = body.y + height;
        for bar in bar_layout(body.width) {
            let level = merged(&self.levels, bar.bands.clone()) * scale;
            let peak = merged(&self.peaks, bar.bands) * scale;
            for x in bar.columns {
                let x = body.x + x;
                match kind {
                    BarKind::Zoned | BarKind::Gradient | BarKind::Mono => {
                        for row in 0..rows {
                            let (glyph, color) = Self::bar_cell(kind, p, row, rows, level, peak);
                            buf[(x, bottom - 1 - row)]
                                .set_char(glyph)
                                .set_fg(color)
                                .set_bg(p.bg);
                        }
                    }
                    BarKind::Mirror => {
                        for row in 0..half {
                            let (glyph, color) = Self::bar_cell(kind, p, row, half, level, peak);
                            buf[(x, bottom - half - 1 - row)]
                                .set_char(glyph)
                                .set_fg(color)
                                .set_bg(p.bg);
                            let cell = &mut buf[(x, bottom - half + row)];
                            match (glyph, Self::units(level, row)) {
                                (' ', _) => cell.set_char(' ').set_fg(p.bg).set_bg(p.bg),
                                ('▔', _) => cell.set_char('▁').set_fg(color).set_bg(p.bg),
                                (_, 8) => cell.set_char('█').set_fg(color).set_bg(p.bg),
                                (_, units) => {
                                    cell.set_char(BLOCKS[8 - units]).set_fg(p.bg).set_bg(color)
                                }
                            };
                        }
                        if rows < height {
                            buf[(x, body.y)].set_char(' ').set_fg(p.bg).set_bg(p.bg);
                        }
                    }
                    BarKind::Dots => {
                        let lit = level.round() as u16;
                        let peak_row = (peak.round() as u16).checked_sub(1);
                        for row in 0..rows {
                            let cell = &mut buf[(x, bottom - 1 - row)];
                            if row < lit || peak_row == Some(row) {
                                cell.set_char('●').set_fg(Self::zone(p, row, rows));
                            } else {
                                cell.set_char('·').set_fg(p.selection);
                            }
                            cell.set_bg(p.bg);
                        }
                    }
                }
            }
        }
    }

    /// Eighths of the cell at `row` covered by a bar of `level` rows.
    fn units(level: f32, row: u16) -> usize {
        ((level - row as f32) * 8.0).ceil().clamp(0.0, 8.0) as usize
    }

    fn zone(p: &Palette, row: u16, rows: u16) -> Color {
        let position = row as f32 / rows.max(1) as f32;
        p.spectrum[if position < 0.55 {
            0
        } else if position < 0.8 {
            1
        } else {
            2
        }]
    }

    /// Glyph and color of one cell in a vertical bar, including the peak marker.
    fn bar_cell(
        kind: BarKind,
        p: &Palette,
        row: u16,
        rows: u16,
        level: f32,
        peak: f32,
    ) -> (char, Color) {
        let units = Self::units(level, row);
        let glyph = if units == 0 && peak > 0.05 && row == (peak.ceil() as u16).saturating_sub(1) {
            '▔'
        } else {
            BLOCKS[units]
        };
        let color = match kind {
            BarKind::Gradient => {
                spectrum_gradient(p, row as f32 / rows.saturating_sub(1).max(1) as f32)
            }
            BarKind::Mono if glyph == '▔' => p.text,
            BarKind::Mono => p.accent,
            BarKind::Zoned | BarKind::Mirror | BarKind::Dots => Self::zone(p, row, rows),
        };
        (glyph, color)
    }

    fn draw_waterfall(&self, buf: &mut Buffer, body: Rect, p: &Palette) {
        let width = usize::from(body.width);
        if width == 0 {
            return;
        }
        let bottom = body.y + body.height;
        for row in 0..body.height {
            let y = bottom - 1 - row;
            let levels = self
                .history
                .len()
                .checked_sub(1 + usize::from(row))
                .map(|index| self.history[index]);
            for column in 0..width {
                // Every column shows a band, merged in narrow panes and repeated
                // in wide ones, so the history fills the width without gaps.
                let x = body.x + column as u16;
                let level = levels
                    .map_or(0.0, |levels| merged(&levels, bands(column, width)))
                    .clamp(0.0, 1.0);
                let cell = &mut buf[(x, y)];
                if level <= 0.0 {
                    cell.set_char(' ').set_fg(p.bg).set_bg(p.bg);
                } else {
                    let glyph = if level < 0.25 {
                        '░'
                    } else if level < 0.5 {
                        '▒'
                    } else if level < 0.75 {
                        '▓'
                    } else {
                        '█'
                    };
                    cell.set_char(glyph)
                        .set_fg(spectrum_gradient(p, level))
                        .set_bg(p.bg);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::Theme;
    use rand::SeedableRng;
    use ratatui::{Terminal, backend::TestBackend};

    const CELL: (u16, u16) = (10, 20);

    fn styled(style: SpectrumStyle) -> SpectrumView {
        let mut view = SpectrumView::new(true, style);
        view.rng = SmallRng::seed_from_u64(7);
        view
    }
    fn backend(width: u16, height: u16) -> Terminal<TestBackend> {
        Terminal::new(TestBackend::new(width, height)).unwrap()
    }
    fn render(view: &mut SpectrumView, terminal: &mut Terminal<TestBackend>, playing: bool) {
        terminal
            .draw(|f| view.draw(f, f.area(), Theme::default().palette(), true, playing, CELL))
            .unwrap();
    }
    fn active(levels: [f32; BANDS]) -> SpectrumFrame {
        SpectrumFrame {
            active: true,
            levels,
            ..SpectrumFrame::default()
        }
    }
    fn top_row(terminal: &Terminal<TestBackend>) -> String {
        let buffer = terminal.backend().buffer();
        (0..buffer.area.width)
            .map(|x| buffer[(x, 0)].symbol().to_string())
            .collect()
    }
    /// Lets the next draw see 200 ms of decay with an expired peak hold.
    fn age(view: &mut SpectrumView) {
        view.updated = Instant::now() - Duration::from_millis(200);
        view.hold.fill(Instant::now() - Duration::from_secs(1));
    }

    #[test]
    fn animation_sleeps_after_decay_and_wakes_for_new_audio() {
        let mut view = styled(SpectrumStyle::Bars);
        let mut terminal = backend(40, 12);
        render(&mut view, &mut terminal, false);
        assert!(!view.needs_animation(false));
        view.accept(active([1.0; BANDS]));
        assert!(view.needs_animation(true));
        render(&mut view, &mut terminal, true);
        assert!(view.needs_animation(false), "pause must let peaks fall");
        for _ in 0..10 {
            age(&mut view);
            render(&mut view, &mut terminal, false);
        }
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame::default());
        assert!(!view.needs_animation(true), "silent frames stay idle");
        view.accept(active([0.5; BANDS]));
        assert!(view.needs_animation(true), "resume wakes animation");
        view.received = Instant::now() - Duration::from_secs(1);
        assert!(!view.needs_animation(true), "stale audio cannot wake it");
        view.clear();
        view.error = Some("Disconnected".into());
        render(&mut view, &mut terminal, false);
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame::default());
        assert!(
            view.needs_animation(false),
            "recovery clears a visible error"
        );
    }

    #[test]
    fn colors_peaks_staleness_and_generation_follow_actual_frames() {
        let mut view = styled(SpectrumStyle::Bars);
        let mut terminal = backend(40, 12);
        view.accept(SpectrumFrame {
            generation: 1,
            ..active([1.0; BANDS])
        });
        render(&mut view, &mut terminal, true);
        for color in Theme::default().palette().spectrum {
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .any(|c| c.fg == color && c.symbol() == "█")
            );
        }
        // An odd inner width must still leave one empty column between bars.
        terminal = backend(61, 12);
        render(&mut view, &mut terminal, true);
        for x in 1..60 {
            assert_eq!(
                terminal.backend().buffer()[(x, 9)].symbol(),
                if x % 2 == 1 { "█" } else { " " }
            );
        }
        view.received = Instant::now() - Duration::from_secs(1);
        for _ in 0..10 {
            age(&mut view);
            render(&mut view, &mut terminal, true);
        }
        assert_eq!(view.levels, [0.0; BANDS]);
        assert_eq!(view.peaks, [0.0; BANDS]);
        view.accept(SpectrumFrame {
            generation: 2,
            ..SpectrumFrame::default()
        });
        view.accept(SpectrumFrame {
            generation: 1,
            ..active([1.0; BANDS])
        });
        render(&mut view, &mut terminal, true);
        assert_eq!(view.levels, [0.0; BANDS]);
    }

    #[test]
    fn titles_name_the_style_when_they_fit() {
        let mut view = styled(SpectrumStyle::Gradient);
        let mut terminal = backend(44, 12);
        render(&mut view, &mut terminal, false);
        assert!(top_row(&terminal).contains("SPECTRUM · gradient · v close · V style"));
        let mut terminal = backend(40, 12);
        render(&mut view, &mut terminal, false);
        let top = top_row(&terminal);
        assert!(top.contains("SPECTRUM · v close"), "{top}");
        assert!(!top.contains("gradient"), "{top}");
        terminal
            .draw(|f| view.draw(f, f.area(), Theme::default().palette(), false, false, CELL))
            .unwrap();
        assert!(top_row(&terminal).starts_with("SPECTRUM · gradient"));
        let mut terminal = backend(1, 1);
        render(&mut view, &mut terminal, false);
    }

    #[test]
    fn gradient_runs_between_the_theme_roles() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Gradient);
        let mut terminal = backend(40, 12);
        view.accept(active([1.0; BANDS]));
        render(&mut view, &mut terminal, true);
        // Body rows are y = 1..=9 (title row 0, axis row 10, border row 11).
        let buffer = terminal.backend().buffer();
        let column: Vec<Color> = (1..=9).map(|y| buffer[(1, y)].fg).collect();
        assert!((1..=9).all(|y| buffer[(1, y)].symbol() == "█"));
        assert_eq!(column[8], p.spectrum[0], "bottom row is the low role");
        assert_eq!(column[0], p.spectrum[2], "top row is the high role");
        assert!(
            column.iter().any(|c| !p.spectrum.contains(c)),
            "intermediate rows blend the roles"
        );
        let mut distinct = column.clone();
        distinct.dedup();
        assert_eq!(distinct.len(), column.len(), "every row has its own color");
    }

    #[test]
    fn mono_uses_accent_and_marks_peaks_with_text() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Mono);
        let mut terminal = backend(40, 12);
        view.accept(active([0.5; BANDS]));
        render(&mut view, &mut terminal, true);
        let is_bar = |c: &ratatui::buffer::Cell| {
            c.symbol() != " " && c.symbol().chars().all(|ch| BLOCKS.contains(&ch))
        };
        let buffer = terminal.backend().buffer();
        let bars = buffer.content().iter().filter(|c| is_bar(c)).count();
        assert!(bars > 0);
        assert!(
            buffer
                .content()
                .iter()
                .filter(|c| is_bar(c))
                .all(|c| c.fg == p.accent)
        );
        assert!(!buffer.content().iter().any(|c| c.symbol() == "▔"));
        // Let the level fall below the held peak so the marker appears.
        view.accept(active([0.1; BANDS]));
        view.updated = Instant::now() - Duration::from_millis(200);
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        let markers: Vec<Color> = buffer
            .content()
            .iter()
            .filter(|c| c.symbol() == "▔")
            .map(|c| c.fg)
            .collect();
        assert!(!markers.is_empty());
        assert!(
            markers.iter().all(|c| *c == p.text),
            "peak marker uses the text role"
        );
        assert!(
            buffer
                .content()
                .iter()
                .filter(|c| is_bar(c))
                .all(|c| c.fg == p.accent)
        );
    }

    #[test]
    fn mirror_is_symmetric_and_paints_partial_lower_cells_inverted() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Mirror);
        // Body rows y = 1..=9; eight are used (2..=9), half = 4; level 0.6 → 2.4 rows.
        let mut terminal = backend(40, 12);
        view.accept(active([0.6; BANDS]));
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 1)].symbol(), " ", "odd top row stays blank");
        for y in [4, 5, 6, 7] {
            assert_eq!(buffer[(1, y)].symbol(), "█", "row {y}");
            assert_eq!(buffer[(1, y)].bg, p.bg, "row {y}");
        }
        let upper = &buffer[(1, 3)];
        let lower = &buffer[(1, 8)];
        assert_eq!(upper.symbol(), "▄", "0.4 of a row is 4 eighths");
        assert_eq!(upper.bg, p.bg);
        assert_eq!(lower.symbol(), "▄", "inverse trick: 8 - 4 eighths");
        assert_eq!(lower.fg, p.bg, "inverse trick paints the canvas color");
        assert_eq!(
            lower.bg, upper.fg,
            "inverse trick fills with the zone color"
        );
        assert_eq!(buffer[(1, 2)].symbol(), " ");
        assert_eq!(buffer[(1, 9)].symbol(), " ");
        assert_eq!(buffer[(1, 9)].fg, p.bg);
        // Let the level drop so peak markers appear on both halves.
        view.accept(active([0.05; BANDS]));
        view.updated = Instant::now() - Duration::from_millis(200);
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        let above = (2..=5).find(|y| buffer[(1, *y)].symbol() == "▔");
        let below = (6..=9).find(|y| buffer[(1, *y)].symbol() == "▁");
        assert!(above.is_some() && below.is_some(), "{above:?} {below:?}");
        assert_eq!(
            above.unwrap() + below.unwrap(),
            11,
            "markers mirror each other"
        );
        assert_eq!(
            buffer[(1, below.unwrap())].fg,
            buffer[(1, above.unwrap())].fg
        );
        // A single body row cannot host a mirror; nothing panics and nothing draws.
        let mut terminal = backend(40, 4);
        view.accept(active([1.0; BANDS]));
        render(&mut view, &mut terminal, true);
        assert_eq!(terminal.backend().buffer()[(1, 1)].symbol(), " ");
    }

    #[test]
    fn dots_light_whole_segments_and_hold_a_peak_dot() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Dots);
        let mut terminal = backend(40, 12);
        view.accept(active([0.5; BANDS]));
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        // Body rows y = 1..=9; 0.5 × 9 = 4.5 rounds to 5 lit segments from the bottom.
        let lit: Vec<u16> = (1..=9)
            .filter(|y| buffer[(1, *y)].symbol() == "●")
            .collect();
        assert_eq!(lit, vec![5, 6, 7, 8, 9]);
        assert!(
            (1..=4).all(|y| buffer[(1, y)].symbol() == "·" && buffer[(1, y)].fg == p.selection)
        );
        assert_eq!(buffer[(1, 9)].fg, p.spectrum[0]);
        view.accept(active([0.1; BANDS]));
        view.updated = Instant::now() - Duration::from_millis(200);
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        let lit: Vec<u16> = (1..=9)
            .filter(|y| buffer[(1, *y)].symbol() == "●")
            .collect();
        assert_eq!(
            lit.len(),
            2,
            "one lit segment plus a held peak dot: {lit:?}"
        );
        assert_eq!(lit[1], 9);
        assert!(lit[0] < 8, "the peak dot floats above the lit segment");
    }

    #[test]
    fn waterfall_scrolls_per_frame_freezes_without_frames_and_survives_generations() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Waterfall);
        let mut terminal = backend(40, 12);
        render(&mut view, &mut terminal, true);
        assert!(!view.needs_animation(true), "an empty waterfall is idle");
        view.accept(active([1.0; BANDS]));
        assert!(view.needs_animation(true), "a frame requests one draw");
        render(&mut view, &mut terminal, true);
        assert!(
            !view.needs_animation(true),
            "nothing moves until the next frame"
        );
        let buffer = terminal.backend().buffer();
        // Body rows y = 1..=9, newest at the bottom; 32 bands fill all 38 columns.
        assert!((1..=38).all(|x| buffer[(x, 9)].symbol() == "█"));
        assert_eq!(buffer[(4, 9)].fg, p.spectrum[2]);
        assert_eq!(buffer[(4, 8)].symbol(), " ");
        view.accept(active([0.3; BANDS]));
        view.accept(active([0.0; BANDS]));
        render(&mut view, &mut terminal, true);
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(4, 9)].symbol(), " ", "silence is a blank row");
        assert_eq!(buffer[(4, 9)].fg, p.bg, "blank cells keep a constant fg");
        assert_eq!(buffer[(4, 8)].symbol(), "▒");
        assert_eq!(buffer[(4, 7)].symbol(), "█");
        assert_eq!(view.history.len(), 3);
        for _ in 0..5 {
            age(&mut view);
            render(&mut view, &mut terminal, false);
        }
        assert_eq!(
            terminal.backend().buffer()[(4, 7)].symbol(),
            "█",
            "pause freezes the history"
        );
        assert!(!view.needs_animation(false));
        view.accept(SpectrumFrame {
            generation: 1,
            ..SpectrumFrame::default()
        });
        assert_eq!(view.history.len(), 3, "a new generation keeps the past");
        view.accept(SpectrumFrame {
            generation: 1,
            current_id: Some("next".into()),
            ..SpectrumFrame::default()
        });
        assert!(
            view.history.is_empty(),
            "a new track starts a fresh history"
        );
        view.accept(SpectrumFrame {
            generation: 1,
            current_id: Some("next".into()),
            ..active([0.9; BANDS])
        });
        assert_eq!(view.history.len(), 1);
        view.clear();
        assert!(view.history.is_empty());
        for _ in 0..(HISTORY + 10) {
            view.accept(active([0.2; BANDS]));
        }
        assert_eq!(view.history.len(), HISTORY);
        // Narrow panes merge bands instead of clipping them.
        let mut terminal = backend(20, 12);
        render(&mut view, &mut terminal, true);
        let row: Vec<String> = (1..19)
            .map(|x| terminal.backend().buffer()[(x, 9)].symbol().to_string())
            .collect();
        assert!(row.iter().all(|s| s == "░"), "{row:?}");
    }

    #[test]
    fn switching_styles_keeps_levels_and_history() {
        let mut view = styled(SpectrumStyle::Bars);
        let mut terminal = backend(40, 12);
        view.accept(active([0.8; BANDS]));
        render(&mut view, &mut terminal, true);
        assert!(view.levels.iter().all(|v| *v > 0.0));
        view.set_style(SpectrumStyle::Waterfall);
        assert_eq!(view.style(), SpectrumStyle::Waterfall);
        assert!(view.needs_animation(false), "a style change redraws once");
        assert_eq!(view.history.len(), 1, "history was collected while in bars");
        assert!(view.levels.iter().all(|v| *v > 0.0));
        render(&mut view, &mut terminal, true);
        view.set_style(SpectrumStyle::Dots);
        assert_eq!(view.history.len(), 1);
        assert!(view.levels.iter().all(|v| *v > 0.0));
        // Fire heat and sparks are drawing state, not audio: a switch starts them over.
        view.set_style(SpectrumStyle::Fire);
        age(&mut view);
        render(&mut view, &mut terminal, true);
        assert!(view.fire.is_hot());
        view.set_style(SpectrumStyle::Sparks);
        assert!(!view.fire.is_hot());
        assert!(view.levels.iter().all(|v| *v > 0.0));
        assert_eq!(view.history.len(), 1);
    }

    /// A frame of `level` in `bands` and silence elsewhere.
    fn bands_at(range: Range<usize>, level: f32) -> [f32; BANDS] {
        std::array::from_fn(|band| if range.contains(&band) { level } else { 0.0 })
    }

    /// Lets frames go stale and the envelope, fire, and sparks settle while paused.
    fn settle(view: &mut SpectrumView, terminal: &mut Terminal<TestBackend>) {
        view.received = Instant::now() - Duration::from_secs(1);
        for _ in 0..40 {
            age(view);
            render(view, terminal, false);
        }
    }

    #[test]
    fn every_style_draws_any_theme_at_any_size_and_then_settles() {
        let ramp: [f32; BANDS] = std::array::from_fn(|band| band as f32 / (BANDS - 1) as f32);
        let sizes = [
            (1, 1),
            (40, 3),
            (40, 4),
            (40, 5),
            (40, 6),
            (40, 12),
            (61, 12),
            (120, 40),
        ];
        let variants = [(true, CELL), (false, (0, 0)), (true, (7, 15))];
        for style in SpectrumStyle::ALL {
            let cases = sizes
                .iter()
                .flat_map(|size| variants.map(|variant| (Theme::default(), *size, variant)))
                .chain(Theme::ALL.map(|theme| (theme, (61, 12), (true, CELL))));
            for (theme, (width, height), (bordered, cell)) in cases {
                let p = theme.palette();
                let mut view = styled(style);
                let mut terminal = backend(width, height);
                let mut draw = |view: &mut SpectrumView, playing| {
                    terminal
                        .draw(|f| view.draw(f, f.area(), p, bordered, playing, cell))
                        .unwrap();
                };
                for step in 0..6 {
                    let levels = if step % 2 == 0 { ramp } else { [0.2; BANDS] };
                    view.accept(SpectrumFrame {
                        channels: Some(crate::spectrum::SpectrumChannels {
                            left: levels,
                            right: [0.3; BANDS],
                        }),
                        ..active(levels)
                    });
                    age(&mut view);
                    draw(&mut view, true);
                }
                view.received = Instant::now() - Duration::from_secs(1);
                for _ in 0..15 {
                    age(&mut view);
                    draw(&mut view, false);
                }
                assert!(
                    !view.needs_animation(false),
                    "{} {} {width}×{height} settles",
                    style.id(),
                    theme.id()
                );
                view.error = Some("Disconnected".into());
                draw(&mut view, false);
            }
        }
    }

    #[test]
    fn radial_settles_to_its_resting_ring() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Radial);
        let mut terminal = backend(60, 18);
        render(&mut view, &mut terminal, false);
        assert!(!view.needs_animation(false));
        let resting = terminal.backend().buffer().clone();
        view.accept(active([0.2; BANDS]));
        render(&mut view, &mut terminal, true);
        assert!(!view.radial.is_active(), "one frame has nothing to compare");
        view.accept(active([0.9; BANDS]));
        assert!(view.needs_animation(true));
        render(&mut view, &mut terminal, true);
        assert!(view.radial.is_active(), "a jump between frames is an onset");
        assert_ne!(terminal.backend().buffer(), &resting);
        settle(&mut view, &mut terminal);
        assert!(!view.radial.is_active());
        assert!(!view.needs_animation(false));
        assert_eq!(terminal.backend().buffer(), &resting);
        // Body rows y = 1..=15; the idle ring is drawn in the border role alone.
        assert!((1..=15).all(|y| (1..=58).all(|x| {
            let cell = &terminal.backend().buffer()[(x, y)];
            (cell.symbol() == " " || cell.fg == p.border) && cell.bg == p.bg
        })));
        // A new generation (seek, pause, resume) stops the pulse and waves at once.
        view.accept(SpectrumFrame {
            generation: 1,
            ..active([0.2; BANDS])
        });
        view.accept(SpectrumFrame {
            generation: 1,
            ..active([0.9; BANDS])
        });
        age(&mut view);
        render(&mut view, &mut terminal, true);
        assert!(view.radial.is_active());
        view.accept(SpectrumFrame {
            generation: 2,
            ..SpectrumFrame::default()
        });
        assert!(!view.radial.is_active());
        // Onsets seen under another style send no wave after a switch.
        view.set_style(SpectrumStyle::Bars);
        view.accept(SpectrumFrame {
            generation: 2,
            ..active([0.2; BANDS])
        });
        view.accept(SpectrumFrame {
            generation: 2,
            ..active([0.9; BANDS])
        });
        view.set_style(SpectrumStyle::Radial);
        age(&mut view);
        render(&mut view, &mut terminal, true);
        assert!(!view.radial.is_active());
    }

    #[test]
    fn fire_burns_while_heat_remains_and_resets_with_the_stream() {
        let p = Theme::default().palette();
        let mut view = styled(SpectrumStyle::Fire);
        let mut terminal = backend(40, 12);
        render(&mut view, &mut terminal, false);
        assert!(!view.needs_animation(false), "a cold fire is idle");
        view.accept(SpectrumFrame {
            generation: 1,
            ..active(bands_at(0..8, 1.0))
        });
        for _ in 0..4 {
            age(&mut view);
            render(&mut view, &mut terminal, true);
        }
        assert!(view.fire.is_hot());
        let buffer = terminal.backend().buffer();
        assert!(buffer.content().iter().any(|cell| cell.symbol() == "▀"));
        assert!(
            (1..=9).all(|y| (30..=38).all(|x| buffer[(x, y)].symbol() == " ")),
            "treble columns stay cold"
        );
        // Stale data lets the levels fall and the fire burn out, then the timer stops.
        settle(&mut view, &mut terminal);
        assert!(!view.fire.is_hot());
        assert!(!view.needs_animation(false));
        let buffer = terminal.backend().buffer();
        assert!((1..=9).all(|y| (1..=38).all(|x| {
            let cell = &buffer[(x, y)];
            cell.symbol() == " " && cell.fg == p.bg && cell.bg == p.bg
        })));
        // A new generation (seek, pause, resume) puts the fire out at once.
        view.accept(SpectrumFrame {
            generation: 2,
            ..active([1.0; BANDS])
        });
        age(&mut view);
        render(&mut view, &mut terminal, true);
        assert!(view.fire.is_hot());
        view.accept(SpectrumFrame {
            generation: 3,
            ..SpectrumFrame::default()
        });
        assert!(!view.fire.is_hot());
    }

    #[test]
    fn ridge_redraws_per_frame_and_keeps_history_like_the_waterfall() {
        let mut view = styled(SpectrumStyle::Ridge);
        let mut terminal = backend(40, 12);
        render(&mut view, &mut terminal, true);
        assert!(!view.needs_animation(true), "an empty ridge is idle");
        view.accept(active([0.6; BANDS]));
        assert!(view.needs_animation(true), "a frame requests one draw");
        render(&mut view, &mut terminal, true);
        assert!(
            !view.needs_animation(true),
            "nothing moves until the next frame"
        );
        let drawn = terminal.backend().buffer().clone();
        assert!(drawn.content().iter().any(|cell| {
            cell.symbol() != " "
                && cell
                    .symbol()
                    .chars()
                    .all(|c| ('\u{2800}'..='\u{28FF}').contains(&c))
        }));
        for _ in 0..3 {
            age(&mut view);
            render(&mut view, &mut terminal, false);
        }
        assert_eq!(
            terminal.backend().buffer(),
            &drawn,
            "pause freezes the lines"
        );
        view.accept(SpectrumFrame {
            generation: 1,
            ..SpectrumFrame::default()
        });
        assert_eq!(view.history.len(), 1, "a new generation keeps the past");
        assert_eq!(view.pushed, 1);
        view.accept(SpectrumFrame {
            generation: 1,
            current_id: Some("next".into()),
            ..SpectrumFrame::default()
        });
        assert!(view.history.is_empty(), "a new track starts over");
        assert_eq!(view.pushed, 0);
    }

    #[test]
    fn sparks_follow_rises_between_consecutive_frames_only() {
        let mut view = styled(SpectrumStyle::Sparks);
        let mut bars = styled(SpectrumStyle::Bars);
        let (mut terminal, mut plain) = (backend(64, 14), backend(64, 14));
        let quiet = bands_at(0..BANDS, 0.3);
        let mut loud = quiet;
        loud[20] = 0.8;
        // The first frame after a reset has nothing to compare with.
        for view in [&mut view, &mut bars] {
            view.accept(active(loud));
        }
        render(&mut view, &mut terminal, true);
        assert!(!view.sparks.is_active());
        for frames in [[quiet, quiet], [quiet, loud]] {
            for frame in frames {
                for view in [&mut view, &mut bars] {
                    view.accept(active(frame));
                }
            }
        }
        // Freeze time so both views draw identical bars.
        let now = Instant::now();
        for view in [&mut view, &mut bars] {
            view.updated = now;
        }
        render(&mut view, &mut terminal, true);
        bars.updated = view.updated;
        render(&mut bars, &mut plain, true);
        assert!(view.sparks.is_active());
        let (sparked, plain) = (terminal.backend().buffer(), plain.backend().buffer());
        let mut braille = 0;
        // Body rows y = 1..=11 between the title and the axis.
        for (x, y) in (1..=11).flat_map(|y| (1..=62).map(move |x| (x, y))) {
            let (sparked, plain) = (&sparked[(x, y)], &plain[(x, y)]);
            if sparked
                .symbol()
                .chars()
                .all(|c| ('\u{2801}'..='\u{28FF}').contains(&c))
            {
                braille += 1;
                assert_eq!(plain.symbol(), " ", "sparks only use blank cells");
            } else {
                assert_eq!(sparked, plain, "cell {x},{y}");
            }
        }
        assert!(braille > 0);
        // A rise across a generation change compares nothing.
        view.accept(SpectrumFrame {
            generation: 1,
            ..active(quiet)
        });
        assert!(!view.sparks.is_active(), "a new generation clears sparks");
        view.accept(SpectrumFrame {
            generation: 1,
            ..active(loud)
        });
        age(&mut view);
        render(&mut view, &mut terminal, true);
        assert!(
            view.sparks.is_active(),
            "consecutive frames of the new stream count"
        );
        settle(&mut view, &mut terminal);
        assert!(!view.sparks.is_active());
        assert!(!view.needs_animation(false));
    }
    #[test]
    fn trail_freezes_without_frames_keeps_generations_and_clears_with_tracks() {
        let mut view = styled(SpectrumStyle::Trail);
        let mut terminal = backend(40, 12);
        view.accept(active([0.2; BANDS]));
        view.accept(active([0.9; BANDS]));
        render(&mut view, &mut terminal, true);
        let saved = terminal.backend().buffer().clone();
        assert!(!view.needs_animation(false));
        age(&mut view);
        render(&mut view, &mut terminal, false);
        assert_eq!(&saved, terminal.backend().buffer());
        view.accept(SpectrumFrame {
            generation: 1,
            ..SpectrumFrame::default()
        });
        assert_eq!(view.history.len(), 2);
        view.accept(SpectrumFrame {
            generation: 2,
            current_id: Some("other".into()),
            ..active([0.6; BANDS])
        });
        assert_eq!(view.history.len(), 1);
        view.clear();
        assert!(view.history.is_empty());
    }

    #[test]
    fn stereo_distinguishes_missing_channels_from_silence_and_falls_back() {
        use crate::spectrum::SpectrumChannels;
        let mut stereo = styled(SpectrumStyle::Stereo);
        let mut bars = styled(SpectrumStyle::Bars);
        let mut terminal = backend(40, 12);
        stereo.accept(active([0.7; BANDS]));
        bars.accept(active([0.7; BANDS]));
        render(&mut stereo, &mut terminal, true);
        assert!(top_row(&terminal).contains("stereo unavailable"));
        let fallback = terminal.backend().buffer().clone();
        render(&mut bars, &mut terminal, true);
        for y in 1..11 {
            for x in 1..39 {
                assert_eq!(fallback[(x, y)], terminal.backend().buffer()[(x, y)]);
            }
        }
        stereo.accept(SpectrumFrame {
            channels: Some(SpectrumChannels::default()),
            ..active([0.7; BANDS])
        });
        render(&mut stereo, &mut terminal, true);
        assert!(!top_row(&terminal).contains("unavailable"));
        // Known silent channels draw no meter, even if the combined test signal is loud.
        for y in 1..10 {
            for x in 3..39 {
                assert_eq!(terminal.backend().buffer()[(x, y)].symbol(), " ");
            }
        }
        for (width, height) in [(9, 8), (15, 6)] {
            let mut tiny = backend(width, height);
            render(&mut stereo, &mut tiny, true);
            let fallback = tiny.backend().buffer().clone();
            render(&mut bars, &mut tiny, true);
            assert_eq!(fallback, *tiny.backend().buffer());
        }
    }

    #[test]
    fn every_style_draws_all_pastel_palettes_with_stereo_and_frequency_labels() {
        let catalog = crate::theme::ThemeCatalog::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("themes/pastel"),
        );
        assert!(catalog.warnings.is_empty());
        assert_eq!(catalog.themes.len(), Theme::ALL.len() + 16);
        for theme in catalog.themes {
            for style in SpectrumStyle::ALL {
                for (width, height) in [(1, 1), (40, 12), (71, 27), (72, 28), (90, 28), (120, 40)] {
                    let mut view = styled(style);
                    view.accept(SpectrumFrame {
                        low_hz: 40.0,
                        high_hz: 16_000.0,
                        channels: Some(crate::spectrum::SpectrumChannels {
                            left: [0.8; BANDS],
                            right: [0.3; BANDS],
                        }),
                        ..active([0.6; BANDS])
                    });
                    let mut terminal = backend(width, height);
                    terminal
                        .draw(|f| view.draw(f, f.area(), theme.palette(), true, true, CELL))
                        .unwrap();
                }
            }
        }
    }
}
