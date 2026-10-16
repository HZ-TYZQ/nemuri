//! 歌词：宽终端在曲库右侧常驻滚动面板，窄终端只在播放区边框上显示同步歌词的当前行。

use std::ops::Range;

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Paragraph};

use crate::app::App;
use crate::lyrics::Lyrics;
use crate::theme::Theme;

use super::text::truncate_display;

/// 曲库区域至少这么宽才分出歌词面板，保证曲库仍能显示标题、歌手和时长。
const MIN_SPLIT_WIDTH: u16 = 96;
const MAX_PANE_WIDTH: u16 = 48;

/// 歌词面板的宽度；不显示面板时为 0。
///
/// 只看歌词开关而不看当前曲目有没有歌词：切到没有歌词的曲目时曲库宽度不变，
/// 布局不会跟着跳动。
pub(super) fn lyrics_pane_width(area_width: u16, enabled: bool) -> u16 {
    if !enabled || area_width < MIN_SPLIT_WIDTH {
        return 0;
    }
    (area_width * 2 / 5).min(MAX_PANE_WIDTH)
}

/// 当前行及与它同一时刻的行（如译文），一起高亮。
fn current_range(lyrics: &Lyrics, position: std::time::Duration) -> Option<Range<usize>> {
    let last = lyrics.current_index(position)?;
    let time = lyrics.lines()[last].time;
    let first = lyrics.lines()[..last]
        .iter()
        .rposition(|line| line.time != time)
        .map_or(0, |index| index + 1);
    Some(first..last + 1)
}

pub(super) fn draw_lyrics(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let lyrics = app.lyrics();
    let mut title = vec![Span::styled(
        " 歌词 ",
        Style::new().fg(theme.primary).bold(),
    )];
    if lyrics.is_some_and(|lyrics| !lyrics.is_synced()) {
        // 说明这份歌词为什么不跟着唱的那句高亮。
        title.push(Span::styled("· 纯文本 ", Style::new().fg(theme.muted)));
    }
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
        .title(Line::from(title));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.is_empty() {
        return;
    }
    let Some(lyrics) = lyrics else {
        draw_placeholder(frame, app, inner, theme);
        return;
    };

    let lines = lyrics.lines();
    let height = usize::from(inner.height);
    let current = current_range(lyrics, app.player().position());
    let overflow = lines.len().saturating_sub(height);
    let start = if lyrics.is_synced() {
        // 当前行尽量停在面板中间；开头和结尾不留多余空白。
        let anchor = current.as_ref().map_or(0, |range| range.start);
        anchor
            .saturating_sub(height.saturating_sub(1) / 2)
            .min(overflow)
    } else {
        // 纯文本没有时间：放得下就整段显示，放不下按播放进度匀速往下滚。
        (overflow as f64 * file_ratio(app)).round() as usize
    };
    let width = usize::from(inner.width);
    let text: Vec<Line> = lines
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(index, line)| {
            let style = if current.as_ref().is_some_and(|range| range.contains(&index)) {
                Style::new().fg(theme.primary).bold()
            } else {
                Style::new().fg(theme.muted)
            };
            Line::styled(truncate_display(&line.text, width), style).centered()
        })
        .collect();
    frame.render_widget(Paragraph::new(text), inner);
}

/// 纯文本歌词按在整个文件里的进度滚动：歌词对应的是整个文件，不是正在放的那一段。
fn file_ratio(app: &App) -> f64 {
    app.effective_duration()
        .filter(|duration| !duration.is_zero())
        .map(|duration| app.player().position().as_secs_f64() / duration.as_secs_f64())
        .unwrap_or_default()
        .clamp(0.0, 1.0)
}

/// 面板常驻，没有歌词可显示时留一行提示，停在有歌词时当前行所在的高度。
fn draw_placeholder(frame: &mut Frame, app: &App, inner: Rect, theme: &Theme) {
    let hint = if app.current_track().is_some() {
        "暂无歌词"
    } else {
        "未在播放"
    };
    let row = Rect {
        y: inner.y + inner.height.saturating_sub(1) / 2,
        height: 1,
        ..inner
    };
    frame.render_widget(
        Line::styled(
            truncate_display(hint, usize::from(inner.width)),
            Style::new().fg(theme.muted),
        )
        .centered(),
        row,
    );
}

/// 窄终端下放在播放区底边的当前行；没有正在唱的非空行时为 None。
pub(super) fn inline_lyric(app: &App, max_width: usize) -> Option<String> {
    let lyrics = app.lyrics()?;
    let range = current_range(lyrics, app.player().position())?;
    let text = lyrics.lines()[range]
        .iter()
        .map(|line| line.text.as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" / ");
    (!text.is_empty()).then(|| format!(" {} ", truncate_display(&text, max_width)))
}
