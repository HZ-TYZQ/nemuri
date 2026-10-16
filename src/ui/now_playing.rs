//! 当前播放状态、模式与进度条。

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::app::App;
use crate::theme::Theme;
use crate::track::Folder;

use super::library::{LIST_ICON_WIDTH, playback_action_indicator};
use super::lyrics::inline_lyric;
use super::text::{
    ascii_progress_bar, fmt_duration, mark_ticks, now_playing_text, truncate_display,
};

/// 边框标题。当前所在段有名字时接在后面：放在标题里不和右边的状态栏抢宽度，
/// 名字太长时截断。
fn now_playing_title(app: &App, width: u16) -> String {
    const BASE: &str = " ♫ 正在播放";
    let name = app
        .current_section()
        .and_then(|index| app.current_marks()[index].name.as_deref());
    // 去掉两个边角、“ · ”和末尾的空格。
    let budget = usize::from(width).saturating_sub(2 + UnicodeWidthStr::width(BASE) + 3 + 1);
    match name {
        Some(name) if budget > 0 => format!("{BASE} · {} ", truncate_display(name, budget)),
        _ => format!("{BASE} "),
    }
}

/// 返回进度条自身占据的矩形，供点击跳转做命中测试。
pub(super) fn draw_now_playing(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    theme: &Theme,
    lyrics_pane_shown: bool,
) -> Option<Rect> {
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
        .title(Span::styled(
            now_playing_title(app, area.width),
            Style::new().fg(theme.primary).bold(),
        ));
    // 没有歌词面板时，当前歌词行借用底边显示，不占额外行。
    if !lyrics_pane_shown
        && let Some(text) = inline_lyric(app, usize::from(area.width.saturating_sub(6)))
    {
        block = block.title_bottom(Line::styled(text, Style::new().fg(theme.primary)).centered());
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let volume = if app.player().is_muted() {
        "静音".to_owned()
    } else {
        format!("{}%", app.player().volume())
    };
    // 睡眠定时放在最前面：这一栏放不下时先被截掉的是末尾的队列数。
    let sleep = match app.sleep_remaining() {
        Some(remaining) => format!("⏾ {} · ", fmt_duration(remaining)),
        None if app.sleeps_at_track_end() => "⏾ 本条 · ".to_owned(),
        None => String::new(),
    };
    let privacy = if app.config().privacy {
        "隐私 · "
    } else {
        ""
    };
    // 平衡和互换只在不是默认时显示，免得忘了上次调过。
    let mut channels = match app.config().balance {
        0 => String::new(),
        balance if balance > 0 => format!(" · 偏右 {balance}"),
        balance => format!(" · 偏左 {}", balance.unsigned_abs()),
    };
    if app.config().swap_channels {
        channels.push_str(" · 左右互换");
    }
    let status = format!(
        "{sleep}{privacy}{} · {volume}{channels} · 队列 {}",
        app.playback_mode().label(),
        app.playback().queue.len()
    );
    let status_width = (UnicodeWidthStr::width(status.as_str()) as u16).max(38);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);
    let top =
        Layout::horizontal([Constraint::Min(10), Constraint::Length(status_width)]).split(rows[0]);
    let now = match app.current_track() {
        Some(track) => {
            let (icon, style) = playback_action_indicator(app.player().state(), theme);
            let budget = usize::from(top[0].width).saturating_sub(LIST_ICON_WIDTH);
            // 没有歌手标签时用文件夹名代替：ASMR 通常按创作者分文件夹。
            let folder = Folder::of(track);
            let byline = match (&track.artist, &folder) {
                (Some(artist), _) => Some(artist.as_str()),
                (None, Folder::Named(name)) => Some(name.as_str()),
                (None, Folder::Root) => None,
            };
            let (title, artist) = now_playing_text(track.display_title(), byline, budget);
            Line::from(vec![
                Span::styled(icon, style),
                Span::styled(title, Style::new().fg(theme.primary).bold()),
                Span::styled(artist, Style::new().fg(theme.muted)),
            ])
        }
        None => Line::from(Span::styled("■ 未在播放", Style::new().fg(theme.muted))),
    };
    frame.render_widget(Paragraph::new(now), top[0]);

    frame.render_widget(
        Paragraph::new(status)
            .alignment(Alignment::Right)
            .style(Style::new().fg(theme.muted)),
        top[1],
    );

    let position = app.shown_position();
    let duration = app.shown_duration();
    let ratio = app.shown_ratio();
    let position_label = fmt_duration(position);
    let duration_label = duration.map(fmt_duration).unwrap_or_else(|| "--:--".into());
    let bar_width =
        (rows[1].width as usize).saturating_sub(position_label.len() + duration_label.len() + 3);
    let bar = ascii_progress_bar(ratio, bar_width);
    let bar = match duration.filter(|duration| !duration.is_zero()) {
        Some(duration) => mark_ticks(
            &bar,
            app.current_marks()
                .iter()
                .map(|mark| app.shown(mark.position).as_secs_f64() / duration.as_secs_f64()),
        ),
        None => bar,
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!(" {position_label} "), Style::new().fg(theme.muted)),
            Span::styled(bar, Style::new().fg(theme.primary)),
            Span::styled(format!(" {duration_label}"), Style::new().fg(theme.muted)),
        ])),
        rows[1],
    );

    // 进度条前有一个空格、时间文本和一个空格；只有知道总时长才谈得上跳转。
    let bar_x = rows[1].x + position_label.len() as u16 + 2;
    (bar_width > 0 && duration.is_some_and(|duration| !duration.is_zero()))
        .then(|| Rect::new(bar_x, rows[1].y, bar_width as u16, 1))
}
