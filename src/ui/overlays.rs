//! 帮助、播放列表、播放队列、文件夹和确认弹层。

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use ratatui::prelude::*;
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, Paragraph, Wrap};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, Overlay};
use crate::theme::Theme;
use crate::track::Track;

use super::keymap::{
    self, DELETE_CONFIRM, FOLDERS, HELP, NAME_INPUT, PLAYLIST_TRACKS, PLAYLISTS, QUEUE,
};
use super::text::{fmt_duration, now_playing_text, truncate_display};
use super::{ListView, ViewLayout};

pub(super) fn draw_overlay(frame: &mut Frame, app: &App, theme: &Theme, layout: &mut ViewLayout) {
    let view = &mut layout.overlay;
    // 非列表型弹层没有可点击的行；先失效，命中测试才不会用上一帧的坐标。
    if !matches!(
        app.view().overlay,
        Overlay::Playlists | Overlay::PlaylistTracks | Overlay::Queue | Overlay::Folders
    ) {
        view.clear();
    }
    match app.view().overlay {
        Overlay::None => {}
        Overlay::Help => layout.help_max_scroll = draw_help(frame, app, theme, view),
        Overlay::Playlists => draw_playlists(frame, app, theme, view),
        Overlay::Queue => draw_queue(frame, app, theme, view),
        Overlay::PlaylistTracks => draw_playlist_tracks(frame, app, theme, view),
        Overlay::Folders => draw_folders(frame, app, theme, view),
        Overlay::NameInput => draw_text_popup(
            frame,
            " 新建播放列表 ",
            vec![
                "请输入名称：",
                &format!("> {}█", app.view().name_input),
                "",
                &keymap::hints(NAME_INPUT),
            ],
            58,
            8,
            theme,
        ),
        Overlay::DeleteConfirm => {
            let name = app
                .playlists()
                .all()
                .get(app.view().playlist_selected)
                .map(|playlist| playlist.name.as_str())
                .unwrap_or("");
            draw_text_popup(
                frame,
                " 确认删除 ",
                vec![
                    &format!("删除播放列表“{name}”？"),
                    "音乐文件不会被删除。",
                    "",
                    &keymap::hints(DELETE_CONFIRM),
                ],
                56,
                8,
                theme,
            );
        }
    }
}

fn draw_playlists(frame: &mut Frame, app: &App, theme: &Theme, view: &mut ListView) {
    let area = centered(frame.area(), 70, 70);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            " 播放列表 ",
            Style::new().fg(theme.primary).bold(),
        ))
        .border_style(Style::new().fg(theme.border));
    let items = if app.playlists().all().is_empty() {
        vec![ListItem::new(Span::styled(
            "暂无播放列表，按 c 创建",
            Style::new().fg(theme.muted),
        ))]
    } else {
        app.playlists()
            .all()
            .iter()
            .map(|playlist| {
                ListItem::new(format!("{}  ({} 首)", playlist.name, playlist.tracks.len()))
            })
            .collect()
    };
    // 与队列面板一致：列表只用提示行以上的空间，点击行与显示行才能一一对应。
    let (inner, viewport) = list_viewport(area, &block);
    frame.render_widget(block, area);
    let list = List::new(items)
        .highlight_symbol(Span::styled("▸ ", Style::new().fg(theme.primary)))
        .highlight_style(Style::new().bg(theme.selection_bg).bold());
    let selected = (!app.playlists().all().is_empty()).then_some(app.view().playlist_selected);
    frame.render_stateful_widget(list, viewport, view.state_for(selected));
    view.record(viewport, app.playlists().all().len());
    let help = help_row(inner);
    frame.render_widget(
        Paragraph::new(keymap::hints(PLAYLISTS)).style(Style::new().fg(theme.muted)),
        help,
    );
}

fn draw_playlist_tracks(frame: &mut Frame, app: &App, theme: &Theme, view: &mut ListView) {
    let area = centered(frame.area(), 78, 76);
    frame.render_widget(Clear, area);
    let Some(playlist) = app.playlists().all().get(app.view().playlist_selected) else {
        view.clear();
        return;
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" {} ", playlist.name),
            Style::new().fg(theme.primary).bold(),
        ))
        .border_style(Style::new().fg(theme.border));
    let items = playlist.tracks.iter().map(|path| {
        let missing = !path.is_file();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("?");
        if missing {
            ListItem::new(Line::from(vec![
                Span::styled("⚠ ", Style::new().fg(theme.danger)),
                Span::styled(name, Style::new().fg(theme.danger)),
                Span::styled("  文件不可用", Style::new().fg(theme.muted)),
            ]))
        } else {
            ListItem::new(name)
        }
    });
    let (inner, viewport) = list_viewport(area, &block);
    frame.render_widget(block, area);
    let list = List::new(items)
        .highlight_symbol(Span::styled("▸ ", Style::new().fg(theme.primary)))
        .highlight_style(Style::new().bg(theme.selection_bg).bold());
    let selected = (!playlist.tracks.is_empty()).then_some(app.view().playlist_track_selected);
    frame.render_stateful_widget(list, viewport, view.state_for(selected));
    view.record(viewport, playlist.tracks.len());
    let help = help_row(inner);
    frame.render_widget(
        Paragraph::new(keymap::hints(PLAYLIST_TRACKS)).style(Style::new().fg(theme.muted)),
        help,
    );
}

const MISSING_LABEL: &str = " 不在曲库中";

/// 把队列路径一次性解析成曲库下标。整体是 O(曲库 + 队列)，
/// 避免逐条线性查找在大曲库加长队列时退化成平方级扫描。
pub(super) fn resolve_queue_rows(
    tracks: &[Track],
    queue: &VecDeque<PathBuf>,
) -> Vec<Option<usize>> {
    let wanted: HashSet<&Path> = queue.iter().map(PathBuf::as_path).collect();
    let mut found: HashMap<&Path, usize> = HashMap::with_capacity(wanted.len());
    for (index, track) in tracks.iter().enumerate() {
        if wanted.contains(track.path.as_path()) {
            found.entry(track.path.as_path()).or_insert(index);
        }
    }
    queue
        .iter()
        .map(|path| found.get(path.as_path()).copied())
        .collect()
}

fn draw_queue(frame: &mut Frame, app: &App, theme: &Theme, view: &mut ListView) {
    let area = centered(frame.area(), 74, 70);
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            format!(" 播放队列 · {} 首 ", app.playback().queue.len()),
            Style::new().fg(theme.primary).bold(),
        ))
        .border_style(Style::new().fg(theme.border));

    if app.playback().queue.is_empty() {
        view.clear();
        frame.render_widget(
            Paragraph::new("  队列是空的\n  在曲库中按 a 加到队尾，按 A 设为下一首")
                .style(Style::new().fg(theme.muted))
                .block(block),
            area,
        );
        return;
    }

    let rows = resolve_queue_rows(app.catalog().tracks(), &app.playback().queue);
    let order_width = format!("{}. ", app.playback().queue.len()).len();
    let duration_width = rows
        .iter()
        .filter_map(|row| app.catalog().tracks().get((*row)?)?.duration)
        .map(|duration| fmt_duration(duration).len())
        .max()
        .unwrap_or(5)
        .max(5);
    // 去掉左右边框和 highlight_symbol 后，中间留一列间隔给时长。
    let text_width = usize::from(area.width)
        .saturating_sub(2 + 2 + order_width + duration_width + 1)
        .max(1);

    let items = app
        .playback()
        .queue
        .iter()
        .zip(&rows)
        .enumerate()
        .map(|(order, (path, row))| {
            let order = format!("{:>width$}. ", order + 1, width = order_width - 2);
            let Some(track) = row.and_then(|index| app.catalog().tracks().get(index)) else {
                let name = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("?");
                // 播放时这些条目会被跳过，因此名字要给提示语让出显示宽度。
                let name_width = text_width
                    .saturating_sub(UnicodeWidthStr::width(MISSING_LABEL))
                    .max(1);
                return ListItem::new(Line::from(vec![
                    Span::styled(order, Style::new().fg(theme.muted)),
                    Span::styled(
                        truncate_display(name, name_width),
                        Style::new().fg(theme.danger),
                    ),
                    Span::styled(MISSING_LABEL, Style::new().fg(theme.muted)),
                ]));
            };
            let (title, artist) =
                now_playing_text(track.display_title(), track.artist.as_deref(), text_width);
            let duration = track
                .duration
                .map(fmt_duration)
                .unwrap_or_else(|| "--:--".to_owned());
            let text_used =
                UnicodeWidthStr::width(title.as_str()) + UnicodeWidthStr::width(artist.as_str());
            let padding = " ".repeat(
                text_width.saturating_sub(text_used)
                    + duration_width.saturating_sub(duration.len())
                    + 1,
            );
            ListItem::new(Line::from(vec![
                Span::styled(order, Style::new().fg(theme.muted)),
                Span::styled(title, Style::new().fg(theme.primary)),
                Span::styled(artist, Style::new().fg(theme.muted)),
                Span::styled(padding, Style::new().fg(theme.muted)),
                Span::styled(duration, Style::new().fg(theme.muted)),
            ]))
        });

    // 列表只用底部提示行以上的空间，长队列不会有条目被提示行盖住。
    let (inner, viewport) = list_viewport(area, &block);
    frame.render_widget(block, area);
    let list = List::new(items)
        .highlight_symbol(Span::styled("▸ ", Style::new().fg(theme.primary)))
        .highlight_style(Style::new().bg(theme.selection_bg).bold());
    frame.render_stateful_widget(
        list,
        viewport,
        view.state_for(Some(app.view().queue_selected)),
    );
    view.record(viewport, app.playback().queue.len());
    let help = help_row(inner);
    frame.render_widget(
        Paragraph::new(keymap::hints(QUEUE)).style(Style::new().fg(theme.muted)),
        help,
    );
}

/// 第 0 行是“全部”，之后每个文件夹一行：名称左对齐、曲目数右对齐，
/// 当前筛选用 ● 标出。
fn draw_folders(frame: &mut Frame, app: &App, theme: &Theme, view: &mut ListView) {
    let folders = app.catalog().folders();
    let rows = folders.len() + 1;
    // 列表行、上下边框和提示行。
    let area = centered(frame.area(), 60, (rows as u16).saturating_add(3));
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(
            " 文件夹 ",
            Style::new().fg(theme.primary).bold(),
        ))
        .border_style(Style::new().fg(theme.border));

    let active = app.catalog().folder();
    let entries = std::iter::once(("全部", app.catalog().len(), active.is_none())).chain(
        folders
            .iter()
            .map(|(folder, count)| (folder.label(), *count, active == Some(folder))),
    );
    // 去掉左右边框和 highlight_symbol；标记占两列，名称和数目之间至少留一列。
    let text_width = usize::from(area.width).saturating_sub(2 + 2 + 2);
    let items = entries.map(|(name, count, current)| {
        let count = format!("{count} 首");
        let name_width = text_width
            .saturating_sub(UnicodeWidthStr::width(count.as_str()) + 1)
            .max(1);
        let name = truncate_display(name, name_width);
        let padding = " ".repeat(
            text_width
                .saturating_sub(UnicodeWidthStr::width(name.as_str()))
                .saturating_sub(UnicodeWidthStr::width(count.as_str()))
                .max(1),
        );
        let marker = if current { "● " } else { "  " };
        ListItem::new(Line::from(vec![
            Span::styled(marker, Style::new().fg(theme.primary)),
            Span::styled(name, Style::new().fg(theme.primary)),
            Span::styled(padding, Style::new().fg(theme.muted)),
            Span::styled(count, Style::new().fg(theme.muted)),
        ]))
    });

    let (inner, viewport) = list_viewport(area, &block);
    frame.render_widget(block, area);
    let list = List::new(items)
        .highlight_symbol(Span::styled("▸ ", Style::new().fg(theme.primary)))
        .highlight_style(Style::new().bg(theme.selection_bg).bold());
    frame.render_stateful_widget(
        list,
        viewport,
        view.state_for(Some(app.view().folder_selected)),
    );
    view.record(viewport, rows);
    let help = help_row(inner);
    frame.render_widget(
        Paragraph::new(keymap::hints(FOLDERS)).style(Style::new().fg(theme.muted)),
        help,
    );
}

fn draw_text_popup(
    frame: &mut Frame,
    title: &str,
    lines: Vec<&str>,
    width: u16,
    height: u16,
    theme: &Theme,
) {
    let area = centered(frame.area(), width, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines.join("\n"))
            .style(Style::new().fg(theme.primary))
            .wrap(Wrap { trim: false })
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .border_style(Style::new().fg(theme.border))
                    .title(Span::styled(title, Style::new().fg(theme.primary).bold())),
            ),
        area,
    );
}

/// 帮助弹层。终端不够高时可以滚动，底边提示下面还有几行。
/// 行按宽度截断而不折行，滚动偏移才和行数一一对应。返回最多能往下滚几行。
fn draw_help(frame: &mut Frame, app: &App, theme: &Theme, view: &mut ListView) -> usize {
    let lines = keymap::help_lines();
    let area = centered(frame.area(), 62, lines.len() as u16 + 2);
    let mut block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(theme.border))
        .title(Span::styled(
            format!(" 快捷键帮助 · {} ", keymap::hints(HELP)),
            Style::new().fg(theme.primary).bold(),
        ));
    let inner = block.inner(area);
    let max_scroll = lines.len().saturating_sub(usize::from(inner.height));
    let offset = app.view().help_scroll.min(max_scroll);
    if max_scroll > 0 {
        let below = max_scroll - offset;
        let note = if below > 0 {
            format!(" ↑↓ 滚动 · 下面还有 {below} 行 ")
        } else {
            " ↑↓ 滚动 · 已到末尾 ".to_owned()
        };
        block =
            block.title_bottom(Line::styled(note, Style::new().fg(theme.muted)).right_aligned());
    }
    let width = usize::from(inner.width);
    let text: Vec<Line> = lines
        .iter()
        .skip(offset)
        .map(|line| Line::raw(truncate_display(line, width)))
        .collect();
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .style(Style::new().fg(theme.primary))
            .block(block),
        area,
    );
    // 没有可点的行，记下区域只是为了让滚轮在弹层上生效。
    view.record(inner, 0);
    max_scroll
}

/// 弹层内容区，以及扣掉底部提示行之后真正留给列表的视口。
fn list_viewport(area: Rect, block: &Block<'_>) -> (Rect, Rect) {
    let inner = block.inner(area);
    let viewport = Rect {
        height: inner.height.saturating_sub(1),
        ..inner
    };
    (inner, viewport)
}

fn help_row(inner: Rect) -> Rect {
    Rect::new(
        inner.x + 1,
        inner.bottom().saturating_sub(1),
        inner.width.saturating_sub(1),
        1,
    )
}

fn centered(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = max_width.min(area.width.saturating_sub(4)).max(10);
    let height = max_height.min(area.height.saturating_sub(2)).max(5);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}
