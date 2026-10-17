//! 长录音的跳转点：在当前位置添加、跳到上一个或下一个，以及跳转点面板、改名和起点终点。
//!
//! 跳转点只对正在播放或暂停中的长录音生效；停止状态没有位置可记、可跳，只给提示。
//! 改动后立刻写盘；写不了时提示，下次改动和退出时再写。
//!
//! 粘贴框把一段时间轴（每行“03:21 掏耳”）一次加成带名字的跳转点，识别规则见 `timeline_text`。
//!
//! 起点和终点是标了角色的跳转点。起点在装载时用（见 `progress.rs` 的 `opening`）；
//! 终点交给播放器作停止位置，播到那里就和自然播完一样接下一首。在面板里给选中的跳转点设
//! （`s` / `S`），或者在曲库里按 `(` / `)` 在当前位置设。
//!
//! 放播放列表里的片段时，只用片段以内的跳转点，时间按片段内的时间显示；
//! 录音的起点和终点在列表里不起作用，也不能在这时设。

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::marks::{self, AddError, Bounds, Mark, MarkStore, Role, RoleChange, RoleError};
use crate::player::PlayState;
use crate::playlist::Range;
use crate::progress;
use crate::timeline_text::{self, Parsed};
use crate::track::{Track, fmt_duration};

use super::playlists::NewClip;
use super::scope::Origin;
use super::{App, Overlay};

const NOT_PLAYING: &str = "先开始播放，才能使用跳转点";
const NOT_LONG: &str = "只有 10 分钟以上的录音能设跳转点";
const NO_MARKS: &str = "这首还没有跳转点：b 在当前位置添加，或在 B 里粘贴时间轴";
const NO_ROLES_IN_PLAYLIST: &str = "播放列表里不用录音的起点和终点，从曲库播放时再设";
const UNSUPPORTED_PATH: &str = "文件路径含有无法保存的字符，不能设跳转点";
const READ_ONLY: &str = "启动时跳转点文件没能读取或保留，本次运行不能添加跳转点";
const NOTHING_RECOGNIZED: &str = "没有认出时间，每行写成 03:21 名字";
/// 粘贴框最多收这么多字节，再多的不要了。
const PASTE_LIMIT: usize = 64 * 1024;

/// 粘贴框里的时间轴按现有的跳转点分一分。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PasteSummary {
    /// 认出的时间个数。
    pub recognized: usize,
    /// 会加上的。
    pub new: usize,
    /// 离已有的跳转点不到 1 秒的。
    pub existing: usize,
    /// 不在录音时长以内的。
    pub beyond: usize,
}

impl PasteSummary {
    /// 不加的那些怎么说，例如 ["2 个已有", "1 个超出时长"]；都没有时为空。
    pub fn skipped(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.existing > 0 {
            parts.push(format!("{} 个已有", self.existing));
        }
        if self.beyond > 0 {
            parts.push(format!("{} 个超出时长", self.beyond));
        }
        parts
    }
}

/// 粘贴框里认出的一个算哪一类。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteKind {
    New,
    Existing,
    Beyond,
}

/// 确定添加时要用的：加到哪首，加哪些。
struct PastePlan {
    track: Track,
    new: Vec<(Duration, Option<String>)>,
    summary: PasteSummary,
}

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

/// 设上或取消起点、终点之后的提示；`at` 是这个跳转点的称呼。
fn role_message(change: RoleChange, at: &str) -> String {
    match (change.role, change.was) {
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
                self.view.message = Some(UNSUPPORTED_PATH.to_owned());
            }
            Err(AddError::ReadOnly) => self.view.message = Some(READ_ONLY.to_owned()),
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

    /// 打开跳转点面板，光标落在当前所在的那一段。还没有跳转点时也打开，好在里面粘贴时间轴。
    pub(super) fn open_marks(&mut self) {
        match self.mark_target() {
            Ok(_) => {
                self.view.marks_selected = self.current_section().unwrap_or(0);
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
        // 删光了面板也留着，变成空面板。
        let last = self.current_marks().is_empty();
        self.view.marks_selected = self
            .view
            .marks_selected
            .min(self.current_marks().len().saturating_sub(1));
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
        match self
            .marks
            .store
            .toggle_role(track, self.view.marks_selected, role, duration)
        {
            Ok(change) => {
                self.save_marks_now(role_message(change, &label(&mark)));
                self.apply_stop();
            }
            Err(error) => {
                if let Some(reason) = self.role_refusal(error) {
                    self.view.message = Some(reason);
                }
            }
        }
    }

    /// `(` / `)`：在当前位置设起点或终点，已经是了就取消。1 秒以内已有跳转点时用它，
    /// 没有时在这里加一个，加点和设角色一次写盘。能用的条件和 `b` 一样；
    /// 放播放列表的条目时不能设，和 `s` / `S` 一样。
    ///
    /// 这一遍不跳转、不改音量。终点设在当前位置时，播放器只在一路播到终点时停，
    /// 这一遍会一直放到文件结尾。
    pub(super) fn toggle_role_here(&mut self, role: Role) {
        let index = match self.mark_target() {
            Ok(index) => index,
            Err(reason) => {
                self.view.message = Some(reason.to_owned());
                return;
            }
        };
        if !self.plays_from_library() {
            self.view.message = Some(NO_ROLES_IN_PLAYLIST.to_owned());
            return;
        }
        let Some(duration) = self.effective_duration() else {
            return;
        };
        let track = &self.catalog.tracks()[index];
        let position = self.playback.player.position();
        match self
            .marks
            .store
            .toggle_role_at(track, position, role, duration)
        {
            Ok(set) => {
                let marks = self.marks.store.marks(&track.path);
                let mut message = role_message(set.change, &label(&marks[set.index]));
                // 面板里看得见角色挪到了哪里，按键时看不见，要说出来。
                if let Some(old) = set
                    .change
                    .replaced
                    .and_then(|old| marks.iter().find(|mark| mark.position == old))
                {
                    let name = match set.change.role {
                        Role::Start => "起点",
                        Role::End | Role::Plain => "终点",
                    };
                    message.push_str(&format!("（原来的{name} {} 留作普通跳转点）", label(old)));
                }
                self.save_marks_now(message);
                self.apply_stop();
            }
            Err(error) => {
                if let Some(reason) = self.role_refusal(error) {
                    self.view.message = Some(reason);
                }
            }
        }
    }

    /// 设起点终点没设成时怎么说；没有这个跳转点时不用说。
    fn role_refusal(&self, error: RoleError) -> Option<String> {
        Some(match error {
            RoleError::Missing => return None,
            RoleError::StartNotBeforeEnd(end) => {
                format!("起点要在终点 {} 之前", self.role_label(Role::End, end))
            }
            RoleError::EndNotAfterStart(Some(start)) => {
                format!("终点要在起点 {} 之后", self.role_label(Role::Start, start))
            }
            RoleError::EndNotAfterStart(None) => "终点要在 0:00 之后".to_owned(),
            RoleError::UnsupportedPath => UNSUPPORTED_PATH.to_owned(),
            RoleError::ReadOnly => READ_ONLY.to_owned(),
        })
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
    /// 新曲目用不了跳转点时关掉面板；只是没有跳转点时是空面板。
    /// 粘贴框不跟着换：时间轴加到打开它时的那一首上。
    pub(super) fn sync_marks_panel(&mut self) {
        if !matches!(self.view.overlay, Overlay::Marks | Overlay::MarkName) {
            return;
        }
        if self.mark_target().is_err() {
            self.view.overlay = Overlay::None;
            return;
        }
        let current = self.marks_owner();
        if current != self.view.marks_for {
            self.view.marks_for = current;
            self.view.marks_selected = self.current_section().unwrap_or(0);
            self.view.overlay = Overlay::Marks;
        }
        self.view.marks_selected = self
            .view
            .marks_selected
            .min(self.current_marks().len().saturating_sub(1));
    }

    /// 面板里的 `v`：打开粘贴框，时间轴加到现在这一首上。
    pub(super) fn start_pasting_marks(&mut self) {
        if self.mark_target().is_err() {
            return;
        }
        self.edit_paste_text(String::clear);
        self.view.paste_for = self.marks_owner();
        self.view.overlay = Overlay::PasteMarks;
    }

    /// 粘贴框里的 Esc：回到面板，内容不留。
    pub(super) fn cancel_pasting_marks(&mut self) {
        self.edit_paste_text(String::clear);
        self.view.overlay = Overlay::Marks;
    }

    /// 接在粘贴框的末尾；超过 [`PASTE_LIMIT`] 的部分不要，并说明。
    pub(super) fn push_paste_text(&mut self, text: &str) {
        let room = PASTE_LIMIT.saturating_sub(self.view.paste_text.len());
        let cut = if text.len() <= room {
            text.len()
        } else {
            self.view.message = Some("内容太长，只保留了前面的部分".to_owned());
            text.char_indices()
                .map(|(index, character)| index + character.len_utf8())
                .take_while(|end| *end <= room)
                .last()
                .unwrap_or(0)
        };
        self.edit_paste_text(|pasted| pasted.push_str(&text[..cut]));
    }

    /// 改动粘贴框的内容并重新识别。识别结果存下来，界面每一帧只数一数，不重新识别。
    pub(super) fn edit_paste_text(&mut self, edit: impl FnOnce(&mut String)) {
        edit(&mut self.view.paste_text);
        self.view.paste_parsed = timeline_text::parse(&self.view.paste_text);
    }

    /// 粘贴框里认出了几个，会加几个，跳过几个。界面每一帧都用，所以只数存下的识别结果；
    /// 已有的跳转点和时长每次现查，它们变了也跟着变。
    pub fn paste_summary(&self) -> PasteSummary {
        let Some((_, kinds)) = self.classified_paste() else {
            return PasteSummary::default();
        };
        let mut summary = PasteSummary {
            recognized: self.view.paste_parsed.len(),
            ..PasteSummary::default()
        };
        for (_, kind) in kinds {
            match kind {
                PasteKind::New => summary.new += 1,
                PasteKind::Existing => summary.existing += 1,
                PasteKind::Beyond => summary.beyond += 1,
            }
        }
        summary
    }

    /// 粘贴框里的时间轴要加到的那一首；已经不在曲库里时为 None。
    pub fn paste_track(&self) -> Option<&Track> {
        let (path, _) = self.view.paste_for.as_ref()?;
        self.catalog.tracks().get(self.index_for_path(path)?)
    }

    /// 空面板里的说明；放片段时说的是这一段。
    pub fn empty_marks_note(&self) -> &'static str {
        if self.plays_from_library() {
            "还没有跳转点：b 在当前位置添加，v 粘贴时间轴"
        } else {
            "这一段里没有跳转点：b 在当前位置添加，v 粘贴时间轴"
        }
    }

    /// 粘贴框里认出的每一个算新的、已有的还是超出时长的，连同要加到的那一首。
    /// 那首已经不在曲库里、或者不知道时长时为 None。
    fn classified_paste(&self) -> Option<(&Track, impl Iterator<Item = (&Parsed, PasteKind)>)> {
        let track = self.paste_track()?;
        // 还在放这一首时用播放器的时长，和面板一致；否则用扫描到的。
        let duration = match self.current_track() {
            Some(current) if current.path == track.path => self.effective_duration(),
            _ => track.duration,
        }?;
        let existing = self.marks.store.marks(&track.path);
        let kinds = self.view.paste_parsed.iter().map(move |parsed| {
            let kind = if parsed.position >= duration {
                PasteKind::Beyond
            } else if marks::has_mark_near(existing, parsed.position) {
                PasteKind::Existing
            } else {
                PasteKind::New
            };
            (parsed, kind)
        });
        Some((track, kinds))
    }

    /// 确定添加时：要加到哪首、加哪些。那首已经不在曲库里时为 None。
    fn paste_plan(&self) -> Option<PastePlan> {
        let (track, kinds) = self.classified_paste()?;
        let new = kinds
            .filter(|(_, kind)| *kind == PasteKind::New)
            .map(|(parsed, _)| (parsed.position, parsed.name.clone()))
            .collect();
        Some(PastePlan {
            track: track.clone(),
            new,
            summary: self.paste_summary(),
        })
    }

    /// 粘贴框里的 Ctrl+S：把认出的新跳转点一次加上，回到面板，光标落在新加的最早的一个上。
    /// 时间轴写的是整个录音的时间，放片段时也按文件时间加，并说明有几个在这一段之外。
    /// 一个也加不了时留在粘贴框里，说明原因。
    pub(super) fn add_pasted_marks(&mut self) {
        let Some(plan) = self.paste_plan() else {
            self.view.message = Some("这首已经不在曲库中，不能添加跳转点".to_owned());
            return;
        };
        if plan.summary.recognized == 0 {
            self.view.message = Some(NOTHING_RECOGNIZED.to_owned());
            return;
        }
        if plan.new.is_empty() {
            self.view.message = Some(format!(
                "没有新的跳转点：{}",
                plan.summary.skipped().join("，")
            ));
            return;
        }
        let added = match self.marks.store.add_all(&plan.track, &plan.new) {
            Ok(added) => added.added,
            Err(AddError::UnsupportedPath) => {
                self.view.message = Some(UNSUPPORTED_PATH.to_owned());
                return;
            }
            Err(AddError::ReadOnly) => {
                self.view.message = Some(READ_ONLY.to_owned());
                return;
            }
            // 批量添加不会因为离得近而失败，只是不加。
            Err(AddError::TooClose(_)) => return,
        };
        let mut details = Vec::new();
        if let Some((_, Origin::Entry { range, .. })) = &self.view.paste_for {
            let outside = added
                .iter()
                .filter(|position| {
                    **position < range.start || range.end.is_some_and(|end| **position >= end)
                })
                .count();
            if outside > 0 {
                details.push(format!("其中 {outside} 个在这一段之外"));
            }
        }
        details.extend(plan.summary.skipped());
        let mut message = format!("已添加 {} 个跳转点", added.len());
        if !details.is_empty() {
            message.push_str(&format!("（{}）", details.join("，")));
        }
        self.edit_paste_text(String::clear);
        self.view.overlay = Overlay::Marks;
        if let Some(row) = added.iter().min().and_then(|first| {
            self.current_marks()
                .iter()
                .position(|mark| mark.position == *first)
        }) {
            self.view.marks_selected = row;
        }
        self.save_marks_now(message);
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
