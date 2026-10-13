//! 曲库列表与播放指示器：每首歌两行，标题和时长在上，歌手、专辑和格式在下。

use std::time::Duration;

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, List, ListItem, Paragraph};

use crate::app::App;
use crate::player::PlayState;
use crate::theme::Theme;
use crate::track::{Folder, Track};

use super::ListView;
use super::text::{column_text, fmt_duration, truncate_display};

// 暂停符号显式请求文本字形，两个操作符号都保留三列的图标区域。
pub(super) const PAUSE_ACTION_ICON: &str = "⏸\u{fe0e}  ";
pub(super) const PLAY_ACTION_ICON: &str = "⏵  ";
pub(super) const STOPPED_ICON: &str = "■  ";
pub(super) const INACTIVE_ICON: &str = "   ";
pub(super) const LIST_ICON_WIDTH: usize = 3;
pub(super) const LIST_GAP_WIDTH: usize = 2;
/// 曲库每个列表项占的屏幕行数，鼠标命中测试按它换算。
pub(super) const LIBRARY_ITEM_HEIGHT: u16 = 2;
const HIGHLIGHT_SYMBOL_WIDTH: usize = 2;

pub(super) fn draw_library(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    theme: &Theme,
    view: &mut ListView,
) {
    let scan = if let Some((scanned, found)) = app.scan_progress() {
        format!(" · 扫描中 {scanned}/{found} ")
    } else if app.catalog().folder().is_some() {
        format!(
            " · {}/{} 首 ",
            app.visible_indices().len(),
            app.catalog().len()
        )
    } else {
        format!(" · {} 首 ", app.visible_indices().len())
    };
    // 筛选时用文件夹名代替音乐库路径：路径常常很长，会先被标题栏截掉。
    let place = match app.catalog().folder() {
        Some(folder) => format!("· 文件夹 {}", folder.label()),
        None => format!("· {}", app.library_dir().display()),
    };
    let title = Line::from(vec![
        Span::styled(" ♪ Music Player ", Style::new().fg(theme.primary).bold()),
        // 排序紧跟标题：音乐库路径可能很长，放它后面会先被标题栏截掉。
        Span::styled(
            format!("· {} ", app.config().sort.label()),
            Style::new().fg(theme.muted),
        ),
        Span::styled(place, Style::new().fg(theme.muted)),
        Span::styled(scan, Style::new().fg(theme.muted)),
    ]);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
        .title(title);

    if app.catalog().tracks().is_empty() {
        view.clear();
        let text = if app.scan_progress().is_some() {
            "  正在后台扫描音乐库……\n  界面仍可响应，扫描完成后歌曲会自动出现"
        } else {
            "  音乐库中没有支持的音频文件\n  按 r 重新扫描，或用 --set-library PATH 更换主库"
        };
        frame.render_widget(
            Paragraph::new(text)
                .style(Style::new().fg(theme.muted))
                .block(block),
            area,
        );
        return;
    }

    let usable = usize::from(area.width).saturating_sub(2 + HIGHLIGHT_SYMBOL_WIDTH);
    let duration_width = duration_column_width(app.catalog().longest_duration());
    let muted = Style::new().fg(theme.muted);
    let items = app.visible_indices().iter().filter_map(|index| {
        let track = app.catalog().tracks().get(*index)?;
        let current = app.playback().playing_index == Some(*index);
        let (icon, icon_style) = if current {
            playback_action_indicator(app.player().state(), theme)
        } else {
            (INACTIVE_ICON, Style::new().fg(theme.primary))
        };
        let title_style = if current {
            Style::new().fg(theme.primary).bold()
        } else {
            Style::new().fg(theme.primary)
        };
        let (title, duration, detail) = track_row_text(track, usable, duration_width);
        Some(ListItem::new(vec![
            Line::from(vec![
                Span::styled(icon, icon_style),
                Span::styled(title, title_style),
                Span::styled(duration, muted),
            ]),
            // 第二行与标题左对齐；选中时 ratatui 在这一行用空白代替高亮符号。
            Line::from(vec![Span::raw(INACTIVE_ICON), Span::styled(detail, muted)]),
        ]))
    });

    let inner = block.inner(area);
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().bg(theme.selection_bg).bold())
        .highlight_symbol(Span::styled("▸ ", Style::new().fg(theme.primary)));
    let selected = (!app.visible_indices().is_empty()).then_some(app.view().selected);
    frame.render_stateful_widget(list, area, view.state_for(selected));
    view.record_items(inner, app.visible_indices().len(), LIBRARY_ITEM_HEIGHT);
}

/// 时长列宽：容得下最长的那首，至少按 `m:ss` 留 5 列。
pub(super) fn duration_column_width(longest: Option<Duration>) -> usize {
    longest
        .map_or(0, |duration| fmt_duration(duration).len())
        .max(5)
}

pub(super) fn playback_action_indicator(state: PlayState, theme: &Theme) -> (&'static str, Style) {
    match state {
        PlayState::Playing => (PAUSE_ACTION_ICON, Style::new().fg(theme.primary).bold()),
        PlayState::Paused => (PLAY_ACTION_ICON, Style::new().fg(theme.primary).bold()),
        PlayState::Stopped => (STOPPED_ICON, Style::new().fg(theme.muted)),
    }
}

/// 一首歌去掉播放图标后的文字：标题（补足空白）、时长、第二行的详情。
///
/// 标题与时长合起来恰好占满 `width - 图标宽`，时长右对齐成一列；
/// 详情按同样的宽度截断。
pub(super) fn track_row_text(
    track: &Track,
    width: usize,
    duration_width: usize,
) -> (String, String, String) {
    let content = width.saturating_sub(LIST_ICON_WIDTH);
    let title_width = content.saturating_sub(duration_width + LIST_GAP_WIDTH);
    let title = format!(
        "{}{}",
        column_text(track.display_title(), title_width, false),
        " ".repeat(LIST_GAP_WIDTH)
    );
    let duration = track
        .duration
        .map(fmt_duration)
        .unwrap_or_else(|| "--:--".into());
    let duration = column_text(&duration, duration_width, true);
    // 文件夹打头，第二行不会空着；歌手、专辑和格式未知时直接省略。
    let folder = Folder::of(track);
    let detail = [
        Some(folder.label()),
        track.artist.as_deref(),
        track.album.as_deref(),
        track.format.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" · ");
    (title, duration, truncate_display(&detail, content))
}
