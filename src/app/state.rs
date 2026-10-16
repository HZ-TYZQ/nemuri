//! App 内部按职责分开的两组状态：播放这一侧和界面这一侧。
//!
//! 字段对 crate 内可见，但 App 只把它们以共享引用交出去；
//! 界面能读不能改，修改都要经过 [`App::dispatch`](super::App::dispatch)。

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::lyrics::Lyrics;
use crate::media::MediaEvent;
use crate::player::PlaybackBackend;
use crate::playlist::PlaylistId;
use crate::spectrum::SpectrumProcessor;

use super::Overlay;
use super::order::PlaybackOrder;
use super::playlists::{Bound, NewClip};
use super::scope::{Origin, Played, Scope};
use super::sleep::SleepTimer;

/// 播放后端、当前曲目、队列与历史、播放范围和顺序，以及跟着当前曲目走的歌词和频谱。
pub struct Playback {
    pub(crate) player: Box<dyn PlaybackBackend>,
    /// 当前曲目在曲库中的下标；曲目不在曲库中或没有在播放时为 None。
    /// 播放列表里的条目也有：曲库里的 ▶、跳转点和歌词都按它找文件。
    pub(crate) playing_index: Option<usize>,
    /// 当前这一项从哪里来：曲库，还是播放列表里的哪一条。
    pub(crate) origin: Origin,
    /// 装载过几次，用作系统媒体控制的曲目 ID。
    pub(super) loads: u64,
    pub(crate) queue: VecDeque<PathBuf>,
    /// 播放过的项目，“上一首”从这里往回取。
    pub(crate) history: Vec<Played>,
    /// 当前的播放范围：曲库，或者一个播放列表。
    pub(crate) scope: Scope,
    /// 曲库范围的播放顺序；列表范围有自己的。
    pub(super) order: PlaybackOrder,
    pub(super) spectrum: SpectrumProcessor,
    /// 歌词所属的曲目路径与结果（None 表示这首没有歌词）。
    /// 路径与播放器当前曲目不一致时在下一个 tick 重新加载。
    pub(super) lyrics: Option<(PathBuf, Option<Lyrics>)>,
    /// 等待交给系统媒体会话的事件。
    pub(super) media_events: Vec<MediaEvent>,
    pub(super) sleep: SleepTimer,
    /// 最近一次交给后端的淡出增益，没变时不重复设置。
    pub(super) fade_gain: f32,
}

impl Playback {
    pub(super) fn new(player: Box<dyn PlaybackBackend>, seed: u64) -> Self {
        Self {
            player,
            playing_index: None,
            origin: Origin::Library,
            loads: 0,
            queue: VecDeque::new(),
            history: Vec::new(),
            scope: Scope::Library,
            order: PlaybackOrder::new(seed),
            spectrum: SpectrumProcessor::new(),
            lyrics: None,
            media_events: Vec::new(),
            sleep: SleepTimer::Off,
            fade_gain: 1.0,
        }
    }
}

/// 光标、弹层、输入框和底栏提示。
#[derive(Debug, Default)]
pub struct ViewState {
    /// 曲库光标：指向当前搜索结果中的第几条。
    pub(crate) selected: usize,
    pub(crate) overlay: Overlay,
    pub(crate) search_active: bool,
    pub(crate) playlist_selected: usize,
    pub(crate) playlist_track_selected: usize,
    pub(crate) queue_selected: usize,
    /// 文件夹面板的光标：第 0 行是“全部”，之后依次是各个文件夹。
    pub(crate) folder_selected: usize,
    /// 跳转点面板的光标：当前曲目的第几个跳转点。
    pub(crate) marks_selected: usize,
    /// 跳转点面板是为哪一项打开的：文件和来源。换了一项（包括同一个文件的另一个片段）时，
    /// 光标要重新落到当前所在段。
    pub(crate) marks_for: Option<(PathBuf, Origin)>,
    /// 帮助弹层往下滚了几行。终端够高时界面不使用它。
    pub(crate) help_scroll: usize,
    /// 新建播放列表、给跳转点或列表条目改名时输入的名称。
    pub(crate) name_input: String,
    /// 正要加进播放列表的片段：“加到播放列表”面板和在那里新建列表时用。
    pub(crate) clip: Option<NewClip>,
    /// 这次运行里上一次把片段加进了哪个列表，下次光标先落在它上面。
    pub(crate) clip_list: Option<PlaylistId>,
    /// 条目的开头和结尾选择面板改的是哪一个，光标在第几行。
    pub(crate) bound: Bound,
    pub(crate) bound_selected: usize,
    /// 底栏显示的一行提示。
    pub(crate) message: Option<String>,
    /// 重扫或重排后要重新选中的曲目；等搜索结果就绪再落到光标上。
    pub(super) pending_selected_path: Option<PathBuf>,
}
