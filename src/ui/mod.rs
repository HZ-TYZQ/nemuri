//! Ratatui 界面：音乐库、播放状态、搜索和模态弹层。

mod keymap;
mod library;
mod lyrics;
mod mouse;
mod now_playing;
mod overlays;
mod text;
mod visualizer;

#[cfg(test)]
mod tests;

use ratatui::prelude::*;
use ratatui::widgets::{Block, ListState, Paragraph};

use crate::app::{App, Overlay};
use crate::theme::Theme;

pub use keymap::key_action;
use library::draw_library;
use lyrics::{draw_lyrics, lyrics_pane_width};
pub use mouse::MouseInput;
use now_playing::draw_now_playing;
use overlays::draw_overlay;
use text::{truncate_display, wrap_display};
use visualizer::{draw_visualizer, visualizer_height};

/// 上一帧的列表布局与滚动位置。
///
/// 哪一屏幕行对应哪一列表项，只有渲染之后才确定：滚动偏移由 ratatui 在
/// 渲染时按选中项算出并写回 `ListState`。因此鼠标命中测试只能依赖上一帧
/// 留下的结果，不能重新推导。它是纯视图状态，不属于 `App`。
#[derive(Debug, Default)]
pub struct ViewLayout {
    pub(crate) library: ListView,
    /// 同一时刻只会有一个列表型弹层可见，所以共用一份。
    pub(crate) overlay: ListView,
    pub(crate) progress: Option<Rect>,
    /// 帮助弹层最多能往下滚几行；没有显示帮助或放得下时为 0。
    pub(crate) help_max_scroll: usize,
}

impl ViewLayout {
    /// 帮助弹层还能往下滚几行。到底以后按键和滚轮都不再往下滚，
    /// 否则 App 里的偏移会越过末尾，往回滚时要先空按几下。
    pub(crate) fn help_room(&self, app: &App) -> usize {
        if app.view().overlay != Overlay::Help {
            return 0;
        }
        self.help_max_scroll.saturating_sub(app.view().help_scroll)
    }
}

#[derive(Debug, Default)]
pub struct ListView {
    state: ListState,
    area: Option<Rect>,
    len: usize,
    /// 每个列表项占的屏幕行数。
    item_height: u16,
}

impl ListView {
    pub(crate) fn state_for(&mut self, selected: Option<usize>) -> &mut ListState {
        self.state.select(selected);
        &mut self.state
    }

    pub(crate) fn record(&mut self, area: Rect, len: usize) {
        self.record_items(area, len, 1);
    }

    pub(crate) fn record_items(&mut self, area: Rect, len: usize, item_height: u16) {
        self.area = Some(area);
        self.len = len;
        self.item_height = item_height;
    }

    pub(crate) fn clear(&mut self) {
        self.area = None;
        self.len = 0;
    }

    /// 屏幕行 → 列表项下标；落在列表之外或空白行返回 None。
    pub(crate) fn index_at(&self, row: u16) -> Option<usize> {
        let area = self.area?;
        if row < area.y || row >= area.bottom() {
            return None;
        }
        let slot = (row - area.y) / self.item_height;
        // ratatui 只画完整的列表项，底部放不下一整项的零头是空白。
        if slot >= area.height / self.item_height {
            return None;
        }
        let index = self.state.offset() + usize::from(slot);
        (index < self.len).then_some(index)
    }

    pub(crate) fn contains(&self, column: u16, row: u16) -> bool {
        self.area
            .is_some_and(|area| area.contains(Position::new(column, row)))
    }
}

pub fn draw(frame: &mut Frame, app: &App, view: &mut ViewLayout) {
    let theme = &app.config().theme;
    let area = frame.area();
    view.help_max_scroll = 0;
    if area.width < 42 || area.height < 12 {
        // 这一帧什么列表都没画，命中测试必须失效，否则会用上一帧的坐标误判。
        view.library.clear();
        view.overlay.clear();
        view.progress = None;
        frame.render_widget(
            Paragraph::new("终端窗口太小\n请调整到至少 42×12")
                .alignment(Alignment::Center)
                .style(Style::new().fg(theme.muted))
                .block(
                    Block::bordered()
                        .border_style(Style::new().fg(theme.border))
                        .title(Span::styled(
                            " Nemuri ",
                            Style::new().fg(theme.primary).bold(),
                        )),
                ),
            area,
        );
        return;
    }

    // 底栏通常一行；提示长时多占的几行先从频谱那里让出来。
    let footer = footer_lines(app, area, theme);
    let footer_height = footer.len() as u16;
    let visualizer_height = visualizer_height(
        area.height.saturating_sub(footer_height - 1),
        app.config().visualizer_enabled,
    );
    if visualizer_height == 0 {
        let chunks = Layout::vertical([
            Constraint::Min(6),
            Constraint::Length(4),
            Constraint::Length(footer_height),
        ])
        .split(area);
        let pane = draw_top(frame, app, chunks[0], theme, view);
        view.progress = draw_now_playing(frame, app, chunks[1], theme, pane);
        frame.render_widget(Paragraph::new(footer), chunks[2]);
    } else {
        let chunks = Layout::vertical([
            Constraint::Min(6),
            Constraint::Length(visualizer_height),
            Constraint::Length(4),
            Constraint::Length(footer_height),
        ])
        .split(area);
        let pane = draw_top(frame, app, chunks[0], theme, view);
        draw_visualizer(frame, app, chunks[1], theme);
        view.progress = draw_now_playing(frame, app, chunks[2], theme, pane);
        frame.render_widget(Paragraph::new(footer), chunks[3]);
    }
    draw_overlay(frame, app, theme, view);
}

/// 曲库，以及开启歌词时宽终端下它右侧的歌词面板。返回是否画了歌词面板。
fn draw_top(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    theme: &Theme,
    view: &mut ViewLayout,
) -> bool {
    let pane_width = lyrics_pane_width(area.width, app.config().lyrics_enabled);
    if pane_width == 0 {
        draw_library(frame, app, area, theme, &mut view.library);
        return false;
    }
    let columns =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(pane_width)]).split(area);
    draw_library(frame, app, columns[0], theme, &mut view.library);
    draw_lyrics(frame, app, columns[1], theme);
    true
}

/// 提示文字前面的“ • ”占的列数；折行后的续行缩进同样的宽度。
const MESSAGE_INDENT: usize = 3;

/// 底栏的各行。提示按终端宽度折行，几条提示（换行分隔）各占一段，续行缩进到圆点后面；
/// 最多占屏幕高度的三分之一，再放不下时最后一行以省略号结尾。搜索框和按键提示只占一行。
fn footer_lines(app: &App, area: Rect, theme: &Theme) -> Vec<Line<'static>> {
    let Some(message) = app
        .view()
        .message
        .as_ref()
        .filter(|_| !app.view().search_active)
    else {
        return vec![footer_line(app, theme)];
    };
    let width = usize::from(area.width).saturating_sub(MESSAGE_INDENT);
    let mut rows: Vec<(bool, String)> = message
        .split('\n')
        .flat_map(|note| {
            wrap_display(note, width)
                .into_iter()
                .enumerate()
                .map(|(index, row)| (index == 0, row))
        })
        .collect();
    let max_rows = usize::from(area.height / 3).max(1);
    if rows.len() > max_rows {
        rows.truncate(max_rows);
        if let Some((_, last)) = rows.last_mut() {
            *last = truncate_display(&format!("{last}…"), width);
        }
    }
    let style = Style::new().fg(theme.primary);
    rows.into_iter()
        .map(|(first, row)| {
            let lead = if first { " • " } else { "   " };
            Line::from(vec![Span::styled(lead, style), Span::styled(row, style)])
        })
        .collect()
}

/// 只占一行的底栏：搜索框，或者没有提示时的按键提示。
fn footer_line(app: &App, theme: &Theme) -> Line<'static> {
    if app.view().search_active {
        Line::from(vec![
            Span::styled(" / ", Style::new().fg(theme.primary).bold()),
            Span::styled(
                app.catalog().query().to_owned(),
                Style::new().fg(theme.primary),
            ),
            Span::styled("█", Style::new().fg(theme.primary)),
            Span::styled(
                format!(
                    "  {} 个结果 · {}",
                    app.visible_indices().len(),
                    keymap::hints(keymap::SEARCH)
                ),
                Style::new().fg(theme.muted),
            ),
        ])
    } else {
        Line::from(Span::styled(
            format!(" {}", keymap::library_hints()),
            Style::new().fg(theme.muted),
        ))
    }
}
