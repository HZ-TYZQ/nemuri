//! 长录音的跳转点：在当前位置添加、跳到上一个或下一个，以及跳转点面板、改名和起点终点。
//!
//! 跳转点只对正在播放或暂停中的长录音生效；停止状态没有位置可记、可跳，只给提示。
//! 改动后立刻写盘；写不了时提示，下次改动和退出时再写。
//!
//! 起点和终点是标了角色的跳转点。起点在装载时用（见 `progress.rs` 的 `opening`）；
//! 终点交给播放器作停止位置，播到那里就和自然播完一样接下一首。
//!
//! 放播放列表里的片段时，只用片段以内的跳转点，时间按片段内的时间显示；
//! 录音的起点和终点在列表里不起作用，也不能在这时设。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::marks::{self, AddError, Bounds, Mark, MarkStore, Role, RoleError};
use crate::player::PlayState;
use crate::playlist::Range;
use crate::progress;
use crate::track::{Track, fmt_duration};

use super::playlists::NewClip;
use super::scope::Origin;
use super::{App, Overlay};

const NOT_PLAYING: &str = "先开始播放，才能使用跳转点";
const NOT_LONG: &str = "只有 10 分钟以上的录音能设跳转点";
const NO_MARKS: &str = "这首还没有跳转点，按 b 在当前位置添加";
const NO_ROLES_IN_PLAYLIST: &str = "播放列表里不用录音的起点和终点，从曲库播放时再设";

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
pub(super) fn label(mark: &Mark) -> String {
    match &mark.name {
        Some(name) => format!("{} {name}", fmt_duration(mark.position)),
        None => fmt_duration(mark.position),
    }
}

impl App {
    /// 提示里怎么称呼当前这一项的跳转点：显示时间，有名字时后接名字。
    fn shown_label(&self, mark: &Mark) -> String {
        let at = fmt_duration(self.shown(mark.position));
        match &mark.name {
            Some(name) => format!("{at} {name}"),
            None => at,
        }
    }

    /// 跳到一个跳转点时的提示；起点和终点点明身份，例如“跳到起点 1:30 正片”。
    fn jumped_to(&self, mark: &Mark) -> String {
        let at = self.shown_label(mark);
        match mark.role {
            Role::Plain => format!("跳到 {at}"),
            Role::Start => format!("跳到起点 {at}"),
            Role::End => format!("跳到终点 {at}"),
        }
    }

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

    /// 当前这一项以内的跳转点：曲库播放是时长以内的，片段是片段开头（含）到结尾（不含）之间的。
    /// 用不了跳转点时为空。面板、进度条和段名都按它显示；面板的第几行加上
    /// [`marks_offset`](Self::marks_offset) 才是跳转点在录音里的位置。
    pub fn current_marks(&self) -> &[Mark] {
        let span = self.span();
        let (Ok(index), Some(end)) = (self.mark_target(), span.end) else {
            return &[];
        };
        let marks = marks::within(
            self.marks.store.marks(&self.catalog.tracks()[index].path),
            end,
        );
        &marks[marks.partition_point(|mark| mark.position < span.start)..]
    }

    /// 片段开头之前还有几个跳转点：面板里不列出来，按行号改动跳转点时要加上。
    fn marks_offset(&self) -> usize {
        let Ok(index) = self.mark_target() else {
            return 0;
        };
        let start = self.span().start;
        self.marks
            .store
            .marks(&self.catalog.tracks()[index].path)
            .partition_point(|mark| mark.position < start)
    }

    /// 当前曲目的起点和终点（时长以内的）；用不了跳转点时都没有。
    /// 播放列表里的条目不用录音的起点和终点，也都没有。
    pub(super) fn current_bounds(&self) -> Bounds {
        if !self.plays_from_library() {
            return Bounds::default();
        }
        marks::bounds(self.current_marks())
    }

    /// 曲库里一首曲目的跳转点，按扫描时的时长截取；装载之前用。不是长录音时为空。
    pub(super) fn marks_of(&self, track: &Track) -> &[Mark] {
        match track.duration {
            Some(duration) if progress::is_long(track) => {
                marks::within(self.marks.store.marks(&track.path), duration)
            }
            _ => &[],
        }
    }

    /// 把当前曲目的终点交给播放器，播到那里就当作播完；没有终点时清掉。
    /// 装载之后和终点可能变了之后调用。跳转之后不用管：播放器只在一路播到终点时停，
    /// 跳到终点或终点之后就一直放到文件结尾，跳回终点之前又会在终点停。
    ///
    /// 播放列表的条目交的是条目自己的结尾。片段的结尾是硬的：App 不让跳转落在它上面或之后，
    /// 所以播放器总是一路播到它。
    pub(super) fn apply_stop(&mut self) {
        let end = match &self.playback.origin {
            Origin::Library => self.current_bounds().end,
            Origin::Entry { range, .. } => range.end,
        };
        self.playback.player.set_stop_at(end);
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
                let at = fmt_duration(self.shown(at));
                self.save_marks_now(format!("已添加跳转点 {at}（B 管理）"));
            }
            Err(AddError::TooClose(at)) => {
                let at = fmt_duration(self.shown(at));
                self.view.message = Some(format!("这里已有跳转点 {at}"));
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
                Some(index) => (marks[index].position, self.jumped_to(&marks[index])),
                // 片段回到片段的开头。
                None => (self.span().start, "跳到开头".to_owned()),
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
                .map(|index| (marks[index].position, self.jumped_to(&marks[index])))
                .ok_or("后面没有跳转点")
        });
        match jump {
            Ok((position, message)) => self.jump_to(position, message),
            Err(reason) => self.view.message = Some(reason.to_owned()),
        }
    }

    /// `0`：有起点时先回到起点；已经在起点后 3 秒以内或起点之前时回到 0:00，
    /// 所以连按两次总是回到 0:00。没有起点时和以前一样回到 0:00，不提示。
    /// 播放列表里的条目不用录音的起点，回到条目开头。
    pub(super) fn seek_to_start(&mut self) {
        let start = self
            .current_marks()
            .iter()
            .find(|mark| mark.role == Role::Start)
            .filter(|_| self.plays_from_library())
            .cloned();
        let Some(start) = start else {
            self.seek_to_ratio(0.0);
            return;
        };
        if start.position + marks::PREVIOUS_GRACE <= self.playback.player.position() {
            let message = format!("回到起点 {}（再按 0 从 0:00 播放）", label(&start));
            self.jump_to(start.position, message);
        } else {
            self.jump_to(Duration::ZERO, "跳到开头".to_owned());
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
                self.view.marks_for = self.marks_owner();
                self.view.overlay = Overlay::Marks;
            }
            Err(reason) => self.view.message = Some(reason.to_owned()),
        }
    }

    /// 面板里的 `a`：从选中的跳转点到下一个跳转点（后面没有了就到文件结尾）这一段，
    /// 接着挑加到哪个播放列表。片段名用这个跳转点的名字。下一个跳转点按整个文件算：
    /// 正在放片段时面板只列片段以内的，加的这一段可以延伸到片段之外。
    pub(super) fn start_adding_clip(&mut self) {
        let Ok(index) = self.mark_target() else {
            return;
        };
        let (Some(mark), Some(duration)) = (
            self.current_marks().get(self.view.marks_selected).cloned(),
            self.effective_duration(),
        ) else {
            return;
        };
        let path = self.catalog.tracks()[index].path.clone();
        let end = marks::within(self.marks.store.marks(&path), duration)
            .iter()
            .map(|other| other.position)
            .find(|position| *position > mark.position);
        self.view.clip = Some(NewClip {
            path,
            range: Range {
                start: mark.position,
                end,
            },
            name: mark.name,
        });
        // 光标先落在这次运行里上一次加过的列表上。
        if let Some(position) = self.view.clip_list.and_then(|id| {
            self.playlists
                .all()
                .iter()
                .position(|playlist| playlist.id == id)
        }) {
            self.view.playlist_selected = position;
        }
        self.view.playlist_selected = self
            .view
            .playlist_selected
            .min(self.playlists.all().len().saturating_sub(1));
        self.view.overlay = Overlay::PickPlaylist;
    }

    /// 面板里的 Enter：跳到选中的跳转点并关闭面板。
    pub(super) fn jump_to_selected_mark(&mut self) {
        let Some(mark) = self.current_marks().get(self.view.marks_selected) else {
            return;
        };
        let (position, message) = (mark.position, self.jumped_to(mark));
        self.view.overlay = Overlay::None;
        self.jump_to(position, message);
    }

    pub(super) fn remove_selected_mark(&mut self) {
        let Ok(index) = self.mark_target() else {
            return;
        };
        let selected = self.view.marks_selected + self.marks_offset();
        let track = &self.catalog.tracks()[index];
        let Some(removed) = self.marks.store.remove(track, selected) else {
            return;
        };
        let last = self.current_marks().is_empty();
        if last {
            self.view.overlay = Overlay::None;
        } else {
            self.view.marks_selected = self.view.marks_selected.min(self.current_marks().len() - 1);
        }
        let at = self.shown_label(&removed);
        let message = match removed.role {
            Role::Start => format!("已删除起点 {at}，以后从 0:00 开始"),
            Role::End => format!("已删除终点 {at}，以后播到文件结尾"),
            Role::Plain if last => format!("已删除最后一个跳转点 {at}"),
            Role::Plain => format!("已删除跳转点 {at}"),
        };
        self.save_marks_now(message);
        self.apply_stop();
    }

    /// 面板里的 `s` / `S`：把选中的跳转点设为起点或终点，已经是了就取消。
    /// 放播放列表的条目时不能设：列表里不用录音的起点和终点，面板显示的又是条目内的时间。
    pub(super) fn toggle_selected_mark_role(&mut self, role: Role) {
        let Ok(index) = self.mark_target() else {
            return;
        };
        if !self.plays_from_library() {
            self.view.message = Some(NO_ROLES_IN_PLAYLIST.to_owned());
            return;
        }
        let Some(mark) = self.current_marks().get(self.view.marks_selected).cloned() else {
            return;
        };
        // 有跳转点可选就有时长，和面板一样按它截取。
        let Some(duration) = self.effective_duration() else {
            return;
        };
        let track = &self.catalog.tracks()[index];
        let at = label(&mark);
        match self
            .marks
            .store
            .toggle_role(track, self.view.marks_selected, role, duration)
        {
            Ok(change) => {
                let message = match (change.role, change.was) {
                    (Role::Start, Role::End) => {
                        format!("已把终点 {at} 改为起点，以后从这里开始，播到文件结尾")
                    }
                    (Role::End, Role::Start) => {
                        format!("已把起点 {at} 改为终点，以后从 0:00 开始，播到这里就算播完")
                    }
                    (Role::Start, _) => format!("已设为起点 {at}，以后从头播放时从这里开始"),
                    (Role::End, _) => format!("已设为终点 {at}，以后播到这里就算播完"),
                    (Role::Plain, Role::Start) => "已取消起点，以后从 0:00 开始".to_owned(),
                    (Role::Plain, _) => "已取消终点，以后播到文件结尾".to_owned(),
                };
                self.save_marks_now(message);
                self.apply_stop();
            }
            Err(RoleError::Missing) => {}
            Err(RoleError::StartNotBeforeEnd(end)) => {
                let end = self.role_label(Role::End, end);
                self.view.message = Some(format!("起点要在终点 {end} 之前"));
            }
            Err(RoleError::EndNotAfterStart(Some(start))) => {
                let start = self.role_label(Role::Start, start);
                self.view.message = Some(format!("终点要在起点 {start} 之后"));
            }
            Err(RoleError::EndNotAfterStart(None)) => {
                self.view.message = Some("终点要在 0:00 之后".to_owned());
            }
        }
    }

    /// 当前曲目的起点或终点在提示里的称呼；找不到时只写时间。
    fn role_label(&self, role: Role, position: Duration) -> String {
        self.current_marks()
            .iter()
            .find(|mark| mark.role == role)
            .map_or_else(|| fmt_duration(position), label)
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
        let selected = self.view.marks_selected + self.marks_offset();
        let track = &self.catalog.tracks()[index];
        if self
            .marks
            .store
            .rename(track, selected, &self.view.name_input)
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
        let current = self.marks_owner();
        if current != self.view.marks_for {
            self.view.marks_for = current;
            self.view.marks_selected = self.current_section().unwrap_or(0);
            self.view.overlay = Overlay::Marks;
        }
        self.view.marks_selected = self.view.marks_selected.min(count - 1);
    }

    /// 跳转点面板是为哪一项打开的：文件和来源。同一个文件的另一个片段也算换了一项。
    fn marks_owner(&self) -> Option<(PathBuf, Origin)> {
        let track = self.current_track()?;
        Some((track.path.clone(), self.playback.origin.clone()))
    }

    /// 扫描后把改了名的文件的跳转点跟过去。正在放的曲目的跳转点或时长可能随之变了，
    /// 终点重新交给播放器。
    pub(super) fn follow_renamed_marks(&mut self) {
        if self
            .marks
            .store
            .follow_renames(self.catalog.tracks(), &self.library_dir)
        {
            self.save_marks_now(String::new());
        }
        self.apply_stop();
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
