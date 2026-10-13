//! App 内部按职责分开的两组状态：播放这一侧和界面这一侧。
//!
//! 字段对 crate 内可见，但 App 只把它们以共享引用交出去；
//! 界面能读不能改，修改都要经过 [`App::dispatch`](super::App::dispatch)。

use std::collections::VecDeque;
use std::path::PathBuf;

use crate::lyrics::Lyrics;
use crate::media::MediaEvent;
use crate::player::PlaybackBackend;
use crate::spectrum::SpectrumProcessor;

use super::Overlay;
use super::order::PlaybackOrder;
use super::sleep::SleepTimer;

/// 播放后端、当前曲目、队列与历史、播放顺序，以及跟着当前曲目走的歌词和频谱。
pub struct Playback {
    pub(crate) player: Box<dyn PlaybackBackend>,
    /// 当前曲目在曲库中的下标；曲目不在曲库中或没有在播放时为 None。
    pub(crate) playing_index: Option<usize>,
    pub(crate) queue: VecDeque<PathBuf>,
    /// 播放过的曲目，“上一首”从这里往回取。
    pub(crate) history: Vec<PathBuf>,
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
            queue: VecDeque::new(),
            history: Vec::new(),
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
    /// 新建播放列表时输入的名称。
    pub(crate) name_input: String,
    /// 底栏显示的一行提示。
    pub(crate) message: Option<String>,
    /// 重扫或重排后要重新选中的曲目；等搜索结果就绪再落到光标上。
    pub(super) pending_selected_path: Option<PathBuf>,
}
