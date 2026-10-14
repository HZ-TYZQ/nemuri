//! 鼠标输入：按上一帧的布局做命中测试，翻译成 [`Action`]。

use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Position;

use crate::app::{Action, App, Overlay};

use super::ViewLayout;

/// 同一位置两次按下在此间隔内算双击。crossterm 不上报双击，只能自己判定。
const DOUBLE_CLICK_WINDOW: Duration = Duration::from_millis(400);
/// 滚轮一格移动的行数。
const SCROLL_STEP: usize = 3;

/// 跨事件的鼠标状态：只记上一次按下，用来判定双击。
#[derive(Debug, Default)]
pub struct MouseInput {
    last_click: Option<(Instant, u16, u16)>,
}

impl MouseInput {
    pub fn actions(&mut self, event: MouseEvent, view: &ViewLayout, app: &App) -> Vec<Action> {
        let (column, row) = (event.column, event.row);
        match event.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                let double = self.register_click(column, row);
                click(column, row, double, view, app).into_iter().collect()
            }
            // 滚轮移动选中项而不是单独移动视口：ratatui 渲染时总会把选中项拉回
            // 可视区域，单独改偏移会在下一帧被撤销。
            MouseEventKind::ScrollUp if hovers_focused_list(column, row, view, app) => {
                vec![Action::CursorUp; SCROLL_STEP]
            }
            MouseEventKind::ScrollDown if hovers_focused_list(column, row, view, app) => {
                let steps = if app.view().overlay == Overlay::Help {
                    SCROLL_STEP.min(view.help_room(app))
                } else {
                    SCROLL_STEP
                };
                vec![Action::CursorDown; steps]
            }
            _ => Vec::new(),
        }
    }

    /// 记录这次按下，并回答它是不是双击的第二下。
    fn register_click(&mut self, column: u16, row: u16) -> bool {
        let now = Instant::now();
        let double = self.last_click.is_some_and(|(at, last_column, last_row)| {
            (last_column, last_row) == (column, row)
                && now.duration_since(at) <= DOUBLE_CLICK_WINDOW
        });
        // 双击之后重新计时，三击不会被当成又一次双击。
        self.last_click = (!double).then_some((now, column, row));
        double
    }
}

/// 弹层打开时只有弹层里的列表响应；否则依次看进度条和曲库。
fn click(column: u16, row: u16, double: bool, view: &ViewLayout, app: &App) -> Option<Action> {
    let row_action = |index| {
        if double {
            Action::ActivateRow(index)
        } else {
            Action::SelectRow(index)
        }
    };
    if app.view().overlay != Overlay::None {
        return view
            .overlay
            .index_at(row)
            .filter(|_| view.overlay.contains(column, row))
            .map(row_action);
    }
    if let Some(progress) = view.progress
        && progress.contains(Position::new(column, row))
    {
        let ratio = f64::from(column - progress.x) / f64::from(progress.width);
        return Some(Action::SeekToRatio(ratio));
    }
    view.library
        .index_at(row)
        .filter(|_| view.library.contains(column, row))
        .map(row_action)
}

fn hovers_focused_list(column: u16, row: u16, view: &ViewLayout, app: &App) -> bool {
    if app.view().overlay == Overlay::None {
        view.library.contains(column, row)
    } else {
        view.overlay.contains(column, row)
    }
}
