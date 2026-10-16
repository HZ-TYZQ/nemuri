//! 长录音的跳转点：在当前位置添加、跳到上一个或下一个，以及跳转点面板和改名。
//!
//! 跳转点只对正在播放或暂停中的长录音生效；停止状态没有位置可记、可跳，只给提示。
//! 改动后立刻写盘；写不了时提示，下次改动和退出时再写。

use std::path::Path;
use std::time::Duration;

use crate::marks::{self, AddError, Mark, MarkStore};
use crate::player::PlayState;
use crate::progress;
use crate::track::fmt_duration;

use super::{App, Overlay};

const NOT_PLAYING: &str = "先开始播放，才能使用跳转点";
const NOT_LONG: &str = "只有 10 分钟以上的录音能设跳转点";
const NO_MARKS: &str = "这首还没有跳转点，按 b 在当前位置添加";

/// 跳转点和写盘状态。
pub(super) struct Marks {
    pub(super) store: MarkStore,
    /// 上次写盘没写成，还有改动没存下来。
    dirty: bool,
}

impl Marks {
    pub(super) fn new(store: MarkStore) -> Self {
        Self {
            store,
            dirty: false,
        }
    }
}

/// 提示里怎么称呼一个跳转点：时间，有名字时后接名字。
fn label(mark: &Mark) -> String {
    match &mark.name {
        Some(name) => format!("{} {name}", fmt_duration(mark.position)),
        None => fmt_duration(mark.position),
    }
}

impl App {
    /// 能用跳转点的当前曲目在曲库里的下标：正在播放或暂停中的长录音。不能用时给出原因。
    fn mark_target(&self) -> Result<usize, &'static str> {
        if self.playback.player.state() == PlayState::Stopped {
            return Err(NOT_PLAYING);
        }
        let index = self.playback.playing_index.ok_or(NOT_PLAYING)?;
        let track = self.catalog.get(index).ok_or(NOT_PLAYING)?;
        if progress::is_long(track) {
            Ok(index)
        } else {
            Err(NOT_LONG)
        }
    }

    /// 当前曲目时长以内的跳转点；用不了跳转点时为空。
    /// 面板、进度条和段名都按它显示，面板的行号也就是跳转点在列表里的位置。
    pub fn current_marks(&self) -> &[Mark] {
        let (Ok(index), Some(duration)) = (self.mark_target(), self.effective_duration()) else {
            return &[];
        };
        marks::within(
            self.marks.store.marks(&self.catalog.tracks()[index].path),
            duration,
        )
    }

    /// 当前位置所在的那一段：第几个跳转点；还没到第一个时为 None。
    pub fn current_section(&self) -> Option<usize> {
        marks::section_at(self.current_marks(), self.playback.player.position())
    }

    /// 有跳转点可跳时交出它们，否则交出提示。
    fn marks_in_reach(&self) -> Result<&[Mark], &'static str> {
        self.mark_target()?;
        match self.current_marks() {
            [] => Err(NO_MARKS),
            marks => Ok(marks),
        }
    }

    pub(super) fn add_mark(&mut self) {
        let index = match self.mark_target() {
            Ok(index) => index,
            Err(reason) => {
                self.view.message = Some(reason.to_owned());
                return;
            }
        };
        let track = &self.catalog.tracks()[index];
        let position = self.playback.player.position();
        match self.marks.store.add(track, position) {
            Ok(added) => {
                let at = self.marks.store.marks(&track.path)[added].position;
                self.save_marks_now(format!("已添加跳转点 {}（B 管理）", fmt_duration(at)));
            }
            Err(AddError::TooClose(at)) => {
                self.view.message = Some(format!("这里已有跳转点 {}", fmt_duration(at)));
            }
            Err(AddError::UnsupportedPath) => {
                self.view.message = Some("文件路径含有无法保存的字符，不能设跳转点".to_owned());
            }
            Err(AddError::ReadOnly) => {
                self.view.message =
                    Some("启动时跳转点文件没能读取或保留，本次运行不能添加跳转点".to_owned());
            }
        }
    }

    pub(super) fn jump_to_previous_mark(&mut self) {
        let jump = self.marks_in_reach().map(|marks| {
            match marks::previous_target(marks, self.playback.player.position()) {
                Some(index) => (
                    marks[index].position,
                    format!("跳到 {}", label(&marks[index])),
                ),
                None => (Duration::ZERO, "跳到开头".to_owned()),
            }
        });
        match jump {
            Ok((position, message)) => self.jump_to(position, message),
            Err(reason) => self.view.message = Some(reason.to_owned()),
        }
    }

    pub(super) fn jump_to_next_mark(&mut self) {
        let jump = self.marks_in_reach().and_then(|marks| {
            marks::next_target(marks, self.playback.player.position())
                .map(|index| {
                    (
                        marks[index].position,
                        format!("跳到 {}", label(&marks[index])),
                    )
                })
                .ok_or("后面没有跳转点")
        });
        match jump {
            Ok((position, message)) => self.jump_to(position, message),
            Err(reason) => self.view.message = Some(reason.to_owned()),
        }
    }

    /// 跳过去并说明跳到了哪里。走和系统媒体控制相同的跳转，收听进度照常记录。
    fn jump_to(&mut self, position: Duration, message: String) {
        if self.output_blocks_playback() {
            return;
        }
        if self.seek_to_requested(position, None) {
            self.view.message = Some(message);
        }
    }

    /// 打开跳转点面板，光标落在当前所在的那一段。
    pub(super) fn open_marks(&mut self) {
        let opened = self
            .marks_in_reach()
            .map(|marks| marks::section_at(marks, self.playback.player.position()).unwrap_or(0));
        match opened {
            Ok(selected) => {
                self.view.marks_selected = selected;
                self.view.marks_path = self.current_track().map(|track| track.path.clone());
                self.view.overlay = Overlay::Marks;
            }
            Err(reason) => self.view.message = Some(reason.to_owned()),
        }
    }

    /// 面板里的 Enter：跳到选中的跳转点并关闭面板。
    pub(super) fn jump_to_selected_mark(&mut self) {
        let Some(mark) = self.current_marks().get(self.view.marks_selected) else {
            return;
        };
        let (position, message) = (mark.position, format!("跳到 {}", label(mark)));
        self.view.overlay = Overlay::None;
        self.jump_to(position, message);
    }

    pub(super) fn remove_selected_mark(&mut self) {
        let Ok(index) = self.mark_target() else {
            return;
        };
        let track = &self.catalog.tracks()[index];
        let Some(removed) = self.marks.store.remove(track, self.view.marks_selected) else {
            return;
        };
        let message = if self.current_marks().is_empty() {
            self.view.overlay = Overlay::None;
            format!("已删除最后一个跳转点 {}", label(&removed))
        } else {
            self.view.marks_selected = self.view.marks_selected.min(self.current_marks().len() - 1);
            format!("已删除跳转点 {}", label(&removed))
        };
        self.save_marks_now(message);
    }

    /// 打开改名输入框，预填现有的名字。
    pub(super) fn start_renaming_mark(&mut self) {
        let Some(mark) = self.current_marks().get(self.view.marks_selected) else {
            return;
        };
        self.view.name_input = mark.name.clone().unwrap_or_default();
        self.view.overlay = Overlay::MarkName;
    }

    /// 改名输入框里的 Enter：留空就清除名字，然后回到面板。
    pub(super) fn rename_selected_mark(&mut self) {
        self.view.overlay = Overlay::Marks;
        let Ok(index) = self.mark_target() else {
            return;
        };
        let track = &self.catalog.tracks()[index];
        if self
            .marks
            .store
            .rename(track, self.view.marks_selected, &self.view.name_input)
        {
            self.save_marks_now(String::new());
        }
    }

    /// 面板开着时换了曲目（自然播完接下一首、系统媒体控制切歌）：
    /// 换成新曲目的跳转点，光标回到当前所在段，正在进行的改名作废。
    /// 新曲目用不了跳转点或没有跳转点时关掉面板。
    pub(super) fn sync_marks_panel(&mut self) {
        if !matches!(self.view.overlay, Overlay::Marks | Overlay::MarkName) {
            return;
        }
        let count = self.current_marks().len();
        if count == 0 {
            self.view.overlay = Overlay::None;
            return;
        }
        let current = self.current_track().map(|track| track.path.clone());
        if current != self.view.marks_path {
            self.view.marks_path = current;
            self.view.marks_selected = self.current_section().unwrap_or(0);
            self.view.overlay = Overlay::Marks;
        }
        self.view.marks_selected = self.view.marks_selected.min(count - 1);
    }

    /// 扫描后把改了名的文件的跳转点跟过去。
    pub(super) fn follow_renamed_marks(&mut self) {
        if self
            .marks
            .store
            .follow_renames(self.catalog.tracks(), &self.library_dir)
        {
            self.save_marks_now(String::new());
        }
    }

    /// 改动后立刻写盘，写成了显示 `message`（为空时不动底栏）。
    /// 写不了时提示原因，留到下次改动或退出时再写。
    fn save_marks_now(&mut self, message: String) {
        match self.marks.store.save(&self.paths.marks_file) {
            Ok(()) => {
                self.marks.dirty = false;
                if !message.is_empty() {
                    self.view.message = Some(message);
                }
            }
            Err(error) => {
                self.marks.dirty = true;
                self.view.message = Some(save_error(&self.paths.marks_file, &error));
            }
        }
    }

    /// 退出时把还没写成的改动再写一次。
    pub fn save_marks(&mut self) -> Result<(), String> {
        if !self.marks.dirty {
            return Ok(());
        }
        self.marks
            .store
            .save(&self.paths.marks_file)
            .map_err(|error| save_error(&self.paths.marks_file, &error))?;
        self.marks.dirty = false;
        Ok(())
    }
}

fn save_error(path: &Path, error: &std::io::Error) -> String {
    format!("无法保存跳转点 {}: {error}", path.display())
}
