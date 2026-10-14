//! 对 App 的全部操作，以及按当前焦点分派它们。
//!
//! 键盘、鼠标和系统媒体控制都先翻译成 [`Action`]，再交给 [`App::dispatch`]；
//! App 因此不认识任何终端事件类型。光标、选中、确认、返回和文字输入这几种
//! 通用操作作用于当前焦点：打开的弹层，否则是搜索框或曲库。

use std::time::Duration;

use crate::media::MediaCommand;
use crate::track::RepeatMode;

use super::{App, Overlay};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    Quit,

    // 播放
    /// 播放中则暂停，否则开始或继续。
    TogglePause,
    Play,
    Pause,
    Next,
    Previous,
    /// 相对跳转，单位微秒；越过曲尾视为下一首。
    SeekBy(i64),
    /// 按总时长的比例跳转，0.0–1.0。
    SeekToRatio(f64),
    /// 跳到绝对位置；带曲目 ID 时只在它仍是当前曲目时生效。
    SeekTo {
        position: Duration,
        track_id: Option<String>,
    },
    ChangeVolume(i8),
    /// 系统媒体控制设定音量；大于 0 时顺带取消静音。
    SetVolume(u8),
    ToggleMute,
    /// 平衡往右（正）或往左（负）挪，限制在 -100–100。
    ShiftBalance(i8),
    ToggleChannelSwap,
    CycleRepeat,
    SetRepeat(RepeatMode),
    ToggleShuffle,
    SetShuffle(bool),
    /// 睡眠定时切换到下一档：关、15–120 分钟。
    CycleSleepTimer,

    // 显示与曲库
    ToggleVisualizer,
    ToggleLyrics,
    ToggleMouse,
    /// 隐私模式：系统媒体控制只显示通用标题。
    TogglePrivacy,
    CycleSort,
    ToggleSortDirection,
    Rescan,
    StartSearch,
    /// 选中的歌曲加到队尾。
    Enqueue,
    /// 选中的歌曲设为下一首。
    EnqueueNext,
    OpenOverlay(Overlay),

    // 当前焦点的列表
    CursorDown,
    CursorUp,
    SelectRow(usize),
    /// 选中并打开这一行，相当于在该行上按 Enter；用于鼠标双击。
    /// 与 `Activate` 不同，它不会关闭搜索框。
    ActivateRow(usize),
    /// Enter：播放选中曲目、打开选中的播放列表、确认输入等。
    Activate,
    /// Esc：关闭或退回上一层弹层、退出搜索；曲库中清除提示。
    Back,

    // 文字输入（搜索框、新建播放列表）
    InputChar(char),
    InputBackspace,

    // 播放列表与队列弹层
    /// 打开新建播放列表的输入框。
    NewPlaylist,
    /// 选中的歌曲加入选中的播放列表。
    AddToPlaylist,
    /// 打开删除选中播放列表的确认框。
    DeletePlaylist,
    ConfirmDelete,
    RemoveFromPlaylist,
    RemoveFromQueue,
    ClearQueue,
    /// 选中的队列条目上移（负数）或下移（正数）。
    MoveInQueue(isize),
}

impl From<MediaCommand> for Action {
    fn from(command: MediaCommand) -> Self {
        match command {
            MediaCommand::Play => Self::Play,
            MediaCommand::Pause => Self::Pause,
            MediaCommand::Toggle => Self::TogglePause,
            MediaCommand::Next => Self::Next,
            MediaCommand::Previous => Self::Previous,
            MediaCommand::SeekRelMicros(offset) => Self::SeekBy(offset),
            MediaCommand::SeekTo { position, track_id } => Self::SeekTo { position, track_id },
            MediaCommand::SetVolume(volume) => Self::SetVolume(volume),
            MediaCommand::SetRepeat(repeat) => Self::SetRepeat(repeat),
            MediaCommand::SetShuffle(shuffle) => Self::SetShuffle(shuffle),
            MediaCommand::Quit => Self::Quit,
        }
    }
}

impl App {
    pub fn dispatch(&mut self, action: Action) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::TogglePause => self.toggle_or_start(),
            Action::Play => self.play_or_resume(),
            Action::Pause => self.playback.player.pause(),
            Action::Next => self.play_next(false),
            Action::Previous => self.play_previous(),
            Action::SeekBy(offset) => self.seek_rel_micros(offset),
            Action::SeekToRatio(ratio) => self.seek_to_ratio(ratio),
            Action::SeekTo { position, track_id } => {
                self.seek_to_requested(position, track_id.as_deref());
            }
            Action::ChangeVolume(delta) => self.change_volume(delta),
            Action::SetVolume(volume) => {
                if volume > 0 {
                    self.playback.player.set_muted(false);
                    self.config.muted = false;
                }
                self.playback.player.set_volume(volume);
                self.config.volume = volume;
            }
            Action::ToggleMute => self.toggle_mute(),
            Action::ShiftBalance(delta) => self.shift_balance(delta),
            Action::ToggleChannelSwap => self.toggle_channel_swap(),
            Action::CycleRepeat => self.cycle_repeat(),
            Action::SetRepeat(repeat) => self.set_repeat(repeat),
            Action::ToggleShuffle => self.toggle_shuffle(),
            Action::SetShuffle(shuffle) => {
                if shuffle != self.config.shuffle {
                    self.toggle_shuffle();
                }
            }
            Action::CycleSleepTimer => self.cycle_sleep_timer(),
            Action::ToggleVisualizer => self.toggle_visualizer(),
            Action::ToggleLyrics => self.toggle_lyrics(),
            Action::ToggleMouse => self.toggle_mouse(),
            Action::TogglePrivacy => self.toggle_privacy(),
            Action::CycleSort => self.cycle_sort(),
            Action::ToggleSortDirection => self.toggle_sort_direction(),
            Action::Rescan => {
                self.scanner.rescan();
                self.view.message = Some("已请求重新扫描音乐库".to_owned());
            }
            Action::StartSearch => {
                self.view.search_active = true;
                self.view.message = None;
            }
            Action::Enqueue => self.enqueue_selected(false),
            Action::EnqueueNext => self.enqueue_selected(true),
            Action::OpenOverlay(Overlay::Folders) => self.open_folders(),
            Action::OpenOverlay(Overlay::Help) => {
                self.view.help_scroll = 0;
                self.view.overlay = Overlay::Help;
            }
            Action::OpenOverlay(overlay) => self.view.overlay = overlay,
            Action::CursorDown => self.move_cursor(1),
            Action::CursorUp => self.move_cursor(-1),
            Action::SelectRow(row) => self.select_row(row),
            Action::ActivateRow(row) => {
                self.select_row(row);
                match self.view.overlay {
                    Overlay::None => self.play_selected(),
                    _ => self.activate(),
                }
            }
            Action::Activate => self.activate(),
            Action::Back => self.back(),
            Action::InputChar(character) => self.input_char(character),
            Action::InputBackspace => self.input_backspace(),
            Action::NewPlaylist => {
                self.view.name_input.clear();
                self.view.overlay = Overlay::NameInput;
            }
            Action::AddToPlaylist => self.add_selected_to_playlist(),
            Action::DeletePlaylist => {
                if !self.playlists.all().is_empty() {
                    self.view.overlay = Overlay::DeleteConfirm;
                }
            }
            Action::ConfirmDelete => self.delete_selected_playlist(),
            Action::RemoveFromPlaylist => self.remove_playlist_track(),
            Action::RemoveFromQueue => self.remove_queue_selected(),
            Action::ClearQueue => self.clear_queue(),
            Action::MoveInQueue(offset) => self.move_queue_selected(offset),
        }
    }

    /// 光标在焦点列表里移动一格；到头时停住。
    fn move_cursor(&mut self, delta: isize) {
        let step = |selected: usize, len: usize| {
            selected
                .saturating_add_signed(delta)
                .min(len.saturating_sub(1))
        };
        match self.view.overlay {
            Overlay::None if delta.is_negative() => self.select_previous(),
            Overlay::None => self.select_next(),
            Overlay::Playlists => {
                self.view.playlist_selected =
                    step(self.view.playlist_selected, self.playlists.all().len());
            }
            Overlay::PlaylistTracks => {
                let len = self.selected_playlist_len();
                self.view.playlist_track_selected = step(self.view.playlist_track_selected, len);
            }
            Overlay::Queue => {
                self.view.queue_selected = step(self.view.queue_selected, self.playback.queue.len())
            }
            // 第 0 行是“全部”，所以比文件夹数多一行。
            Overlay::Folders => {
                self.view.folder_selected =
                    step(self.view.folder_selected, self.catalog.folders().len() + 1);
            }
            // 只有界面知道弹层多高，往下滚到底时由它不再发出 CursorDown。
            Overlay::Help => {
                self.view.help_scroll = self.view.help_scroll.saturating_add_signed(delta);
            }
            Overlay::NameInput | Overlay::DeleteConfirm => {}
        }
    }

    fn select_row(&mut self, row: usize) {
        match self.view.overlay {
            Overlay::None => {
                self.cancel_pending_selection_restore();
                self.view.selected = row;
            }
            Overlay::Playlists => self.view.playlist_selected = row,
            Overlay::PlaylistTracks => self.view.playlist_track_selected = row,
            Overlay::Queue => self.view.queue_selected = row,
            Overlay::Folders => self.view.folder_selected = row,
            Overlay::Help | Overlay::NameInput | Overlay::DeleteConfirm => {}
        }
    }

    fn activate(&mut self) {
        match self.view.overlay {
            Overlay::None if self.view.search_active => {
                self.cancel_pending_selection_restore();
                self.play_selected();
                self.view.search_active = false;
            }
            Overlay::None => self.play_selected(),
            Overlay::Playlists => {
                if !self.playlists.all().is_empty() {
                    self.view.playlist_track_selected = 0;
                    self.view.overlay = Overlay::PlaylistTracks;
                }
            }
            Overlay::PlaylistTracks => self.play_playlist_from_selected(),
            Overlay::Queue => self.play_queue_selected(),
            Overlay::Folders => self.apply_selected_folder(),
            Overlay::NameInput => self.create_playlist(),
            Overlay::Help | Overlay::DeleteConfirm => {}
        }
    }

    fn back(&mut self) {
        match self.view.overlay {
            Overlay::None if self.view.search_active => self.cancel_search(),
            Overlay::None => self.view.message = None,
            Overlay::Help | Overlay::Playlists | Overlay::Queue | Overlay::Folders => {
                self.view.overlay = Overlay::None
            }
            Overlay::PlaylistTracks | Overlay::NameInput | Overlay::DeleteConfirm => {
                self.view.overlay = Overlay::Playlists;
            }
        }
    }

    fn input_char(&mut self, character: char) {
        match self.view.overlay {
            Overlay::NameInput => self.view.name_input.push(character),
            Overlay::None if self.view.search_active => {
                let mut query = self.catalog.query().to_owned();
                query.push(character);
                self.edit_search(query);
            }
            _ => {}
        }
    }

    fn input_backspace(&mut self) {
        match self.view.overlay {
            Overlay::NameInput => {
                self.view.name_input.pop();
            }
            Overlay::None if self.view.search_active => {
                let mut query = self.catalog.query().to_owned();
                query.pop();
                self.edit_search(query);
            }
            _ => {}
        }
    }

    /// 开启鼠标捕获会接管终端自己的拖拽选区，因此保留一个开关；
    /// 真正的终端模式切换由主循环在下一次迭代时应用。
    fn toggle_mouse(&mut self) {
        self.config.mouse_enabled = !self.config.mouse_enabled;
        self.view.message = Some(
            if self.config.mouse_enabled {
                "鼠标已开启"
            } else {
                "鼠标已关闭，可用终端自身的选区和复制"
            }
            .to_owned(),
        );
    }
}
