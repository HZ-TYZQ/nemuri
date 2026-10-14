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
                            " Music Player ",
                            Style::new().fg(theme.primary).bold(),
                        )),
                ),
            area,
        );
        return;
    }

    let visualizer_height = visualizer_height(area.height, app.config().visualizer_enabled);
    if visualizer_height == 0 {
        let chunks = Layout::vertical([
            Constraint::Min(6),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(area);
        let pane = draw_top(frame, app, chunks[0], theme, view);
        view.progress = draw_now_playing(frame, app, chunks[1], theme, pane);
        draw_footer(frame, app, chunks[2], theme);
    } else {
        let chunks = Layout::vertical([
            Constraint::Min(6),
            Constraint::Length(visualizer_height),
            Constraint::Length(4),
            Constraint::Length(1),
        ])
        .split(area);
        let pane = draw_top(frame, app, chunks[0], theme, view);
        draw_visualizer(frame, app, chunks[1], theme);
        view.progress = draw_now_playing(frame, app, chunks[2], theme, pane);
        draw_footer(frame, app, chunks[3], theme);
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

fn draw_footer(frame: &mut Frame, app: &App, area: Rect, theme: &Theme) {
    let line = if app.view().search_active {
        Line::from(vec![
            Span::styled(" / ", Style::new().fg(theme.primary).bold()),
            Span::styled(app.catalog().query(), Style::new().fg(theme.primary)),
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
    } else if let Some(message) = &app.view().message {
        Line::from(vec![
            Span::styled(" • ", Style::new().fg(theme.primary)),
            Span::styled(message.clone(), Style::new().fg(theme.primary)),
        ])
    } else {
        Line::from(Span::styled(
            format!(" {}", keymap::library_hints()),
            Style::new().fg(theme.muted),
        ))
    };
    frame.render_widget(Paragraph::new(line), area);
}
