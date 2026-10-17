//! 应用状态和所有可观察的播放行为。

mod action;
mod catalog;
mod fade;
mod folders;
mod lyrics;
mod marks;
mod media;
mod order;
mod playback;
mod playlists;
mod progress;
mod queue;
mod scope;
mod selection;
mod session;
mod sleep;
mod sorting;
mod state;
mod timeline;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use crate::config::{AppConfig, AppPaths};
use crate::library::{LibraryEvent, LibraryWorker};
use crate::marks::MarkStore;
use crate::player::{PlayState, PlaybackBackend, Player, PlayerEvent};
use crate::playlist::PlaylistStore;
use crate::progress::ProgressStore;
use crate::session::Session;
use crate::track::Track;

pub use action::Action;
pub use catalog::Catalog;
use marks::Marks;
pub use marks::PasteSummary;
use playback::Skip;
pub use playlists::{Bound, BoundChoice, NewClip};
use progress::Progress;
use sleep::SleepTimer;
pub use state::{Playback, ViewState};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Overlay {
    #[default]
    None,
    Help,
    Playlists,
    PlaylistTracks,
    Queue,
    Folders,
    NameInput,
    DeleteConfirm,
    /// 当前曲目的跳转点。
    Marks,
    /// 给面板里选中的跳转点改名。
    MarkName,
    /// 把时间轴粘贴进来，一次加好跳转点。
    PasteMarks,
    /// 把跳转点面板里选中的一段加到哪个播放列表。
    PickPlaylist,
    /// 给列表条目挑开头或结尾。
    EntryBound,
    /// 给列表条目改名。
    EntryName,
}

/// 曲库变动前记下的曲目，变动后按路径找回它们的新下标。
struct Positions {
    playing: Option<PathBuf>,
    selected: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BagUpdate {
    Reanchor,
    Leave,
}

/// 应用核心：曲库、播放、界面状态，以及配置、播放列表和后台扫描。
///
/// 字段都是私有的。界面通过下面的只读访问器读取，一切修改经由
/// [`App::dispatch`] 和 [`App::on_tick`]。
pub struct App {
    catalog: Catalog,
    playback: Playback,
    view: ViewState,
    playlists: PlaylistStore,
    progress: Progress,
    marks: Marks,
    config: AppConfig,
    /// 启动时的设置。退出时与 `config` 比较，只把改过的项写回配置文件。
    config_at_start: AppConfig,
    library_dir: PathBuf,
    scanner: LibraryWorker,
    /// 正在后台扫描时为 Some((已扫描, 已找到))。
    scan_progress: Option<(usize, usize)>,
    paths: AppPaths,
    should_quit: bool,
    save_config_on_exit: bool,
    /// 上次退出时的曲目与位置，等第一次扫描完成、曲库就绪后再恢复。
    pending_session: Option<Session>,
    /// 启动时的提示：配置写错了、刚复制了旧目录、播放列表、进度或跳转点读不了。
    /// 第一次扫描完成时的提示会盖掉它；那时它还没被别的操作换掉，就放回底栏。
    startup_note: Option<String>,
    /// 最近一次 tick 的时间。按键在两次 tick 之间处理，定时以它为准，
    /// 误差不超过一个 tick（几十毫秒）。
    now: Instant,
}

impl App {
    pub fn new(
        library_dir: PathBuf,
        paths: AppPaths,
        config: AppConfig,
        initial_warning: Option<String>,
        save_config_on_exit: bool,
    ) -> Result<Self, String> {
        Self::with_player(
            Box::new(Player::new()?),
            library_dir,
            paths,
            config,
            initial_warning,
            save_config_on_exit,
        )
    }

    fn with_player(
        mut player: Box<dyn PlaybackBackend>,
        library_dir: PathBuf,
        paths: AppPaths,
        config: AppConfig,
        initial_warning: Option<String>,
        save_config_on_exit: bool,
    ) -> Result<Self, String> {
        player.set_volume(config.volume);
        player.set_muted(config.muted);
        player.set_channel_mix(playback::channel_mix(&config));
        player.set_spectrum_enabled(config.visualizer_enabled);
        let playlists = PlaylistStore::load(paths.playlists_dir.clone())
            .map_err(|error| format!("无法打开播放列表目录: {error}"))?;
        let playlist_warning = playlists.warnings().first().cloned();
        let (progress, progress_warning) = ProgressStore::load(&paths.progress_file);
        let (marks, marks_warning) = MarkStore::load(&paths.marks_file);
        let now = Instant::now();
        let scanner = LibraryWorker::start(library_dir.clone(), paths.cache_db.clone());
        let pending_session =
            Session::load(&paths.session_file).filter(|session| session.library_dir == library_dir);
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos() as u64;
        // 读不了的播放列表、收听进度和跳转点排在最前面；几条提示都要显示，不能只留第一条，
        // 否则升级时总会出现的“已复制旧目录”会把它们挤掉。
        let startup_note = join_notes([
            playlist_warning,
            progress_warning,
            marks_warning,
            initial_warning,
        ]);
        Ok(Self {
            catalog: Catalog::new(),
            playback: Playback::new(player, seed),
            view: ViewState {
                message: startup_note.clone(),
                ..ViewState::default()
            },
            playlists,
            progress: Progress::new(progress, now),
            marks: Marks::new(marks),
            config_at_start: config.clone(),
            config,
            library_dir,
            scanner,
            scan_progress: None,
            paths,
            should_quit: false,
            save_config_on_exit,
            pending_session,
            startup_note,
            now,
        })
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn playback(&self) -> &Playback {
        &self.playback
    }

    pub fn player(&self) -> &dyn PlaybackBackend {
        self.playback.player.as_ref()
    }

    pub fn view(&self) -> &ViewState {
        &self.view
    }

    pub fn config(&self) -> &AppConfig {
        &self.config
    }

    pub fn playlists(&self) -> &PlaylistStore {
        &self.playlists
    }

    pub fn library_dir(&self) -> &Path {
        &self.library_dir
    }

    /// 正在后台扫描时返回 (已扫描, 已找到)。
    pub fn scan_progress(&self) -> Option<(usize, usize)> {
        self.scan_progress
    }

    pub fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// 没有别的提示占着底栏时才显示这条，不覆盖更要紧的消息。
    pub fn notify_if_idle(&mut self, message: String) {
        if self.view.message.is_none() {
            self.view.message = Some(message);
        }
    }

    pub fn visible_indices(&self) -> &[usize] {
        self.catalog.visible()
    }

    pub fn selected_track_index(&self) -> Option<usize> {
        self.visible_indices().get(self.view.selected).copied()
    }

    pub fn selected_track(&self) -> Option<&Track> {
        self.selected_track_index()
            .and_then(|index| self.catalog.get(index))
    }

    pub fn current_track(&self) -> Option<&Track> {
        self.playback
            .playing_index
            .and_then(|index| self.catalog.get(index))
    }

    pub fn on_tick(&mut self, now: Instant) {
        self.drain_library_events();
        self.sync_lyrics();
        self.catalog.tick_search();
        self.restore_pending_selection();
        self.clamp_selections();
        self.drain_player_events();
        self.sync_marks_panel();
        self.tick_timers(now);

        if self.config.visualizer_enabled && self.playback.player.state() != PlayState::Playing {
            self.playback.spectrum.fade_step();
        }
    }

    /// 随时间推进的状态：睡眠定时、片段淡入淡出和收听进度。与扫描、搜索分开，测试可以只推进时间。
    pub(super) fn tick_timers(&mut self, now: Instant) {
        self.now = now;
        self.tick_sleep();
        self.tick_clip_fade();
        self.tick_progress();
    }

    fn drain_library_events(&mut self) {
        for event in self.scanner.drain_events() {
            match event {
                LibraryEvent::ScanStarted => self.scan_progress = Some((0, 0)),
                LibraryEvent::Progress { scanned, found } => {
                    self.scan_progress = Some((scanned, found));
                }
                LibraryEvent::ScanFinished { tracks, warnings } => {
                    self.apply_scan_finished(tracks, warnings);
                }
                LibraryEvent::Warning(warning) => self.view.message = Some(warning),
                LibraryEvent::Error(error) => {
                    self.scan_progress = None;
                    self.view.message = Some(error);
                }
            }
        }
    }

    /// 处理播放后端的事件：自然播完、播放出错、输出中断与恢复、频谱帧。
    pub(super) fn drain_player_events(&mut self) {
        for event in self.playback.player.drain_events() {
            match event {
                PlayerEvent::EndOfStream => {
                    self.mark_finished();
                    if self.playback.sleep == SleepTimer::EndOfTrack {
                        self.sleep_at_track_end();
                    } else {
                        self.play_next(true);
                    }
                }
                PlayerEvent::OutputError(error) => {
                    self.playback.spectrum.reset_output();
                    self.view.message =
                        Some(format!("音频输出中断，已保留进度；按空格重试：{error}"));
                }
                PlayerEvent::OutputWarning(message) => self.view.message = Some(message),
                PlayerEvent::OutputRecovered => {
                    self.view.message = Some("音频输出已恢复".to_owned());
                }
                PlayerEvent::Error(error) => {
                    let name = self
                        .current_track()
                        .map(|track| track.display_title().to_owned())
                        .unwrap_or_else(|| "当前曲目".to_owned());
                    // 播放错误不是自然结束；单曲循环也应先尝试后续歌曲。
                    // 原因随这一轮带下去，成功切歌后才汇总，避免被清屏抹掉。
                    self.advance(false, vec![Skip::new(name, format!("播放中断: {error}"))]);
                }
                PlayerEvent::StateChanged(_) => {}
                PlayerEvent::SpectrumFrame {
                    magnitudes,
                    sample_rate,
                } => {
                    // 逐帧处理：一次 drain 中的多帧都经过完整管线，
                    // 避免 last-wins 丢失瞬态峰值。
                    if self.config.visualizer_enabled {
                        self.playback
                            .spectrum
                            .process_frame(&magnitudes, sample_rate);
                    }
                }
            }
        }
    }

    /// 当前可视 bar 高度（0.0..=1.0），供 UI 绘制。
    pub fn spectrum_bars(&self) -> &[f32] {
        self.playback.spectrum.bars()
    }

    /// 换掉整个曲目集合，并按路径找回正在播放和选中的曲目。
    pub(super) fn replace_tracks(&mut self, tracks: Vec<Track>) {
        let remembered = self.remember_positions();
        self.catalog.replace(tracks, self.config.sort);
        self.restore_positions(remembered);
    }

    /// 按当前排序重排曲库，正在播放和选中的曲目跟着移动。
    pub(super) fn resort_tracks(&mut self) {
        let remembered = self.remember_positions();
        self.catalog.resort(self.config.sort);
        self.restore_positions(remembered);
    }

    /// 曲库变动前记下正在播放与选中的曲目路径。必须在动曲库之前取：
    /// 之后旧下标就对不上了。
    fn remember_positions(&self) -> Positions {
        Positions {
            playing: self.playback.player.current_path().map(Path::to_path_buf),
            selected: self.selected_track().map(|track| track.path.clone()),
        }
    }

    /// 曲库变动后按路径重建所有派生下标：`playing_index`、随机袋和光标。
    /// 光标要等搜索结果就绪才能落定，因此先记成待恢复的路径。
    fn restore_positions(&mut self, remembered: Positions) {
        self.playback.playing_index = remembered
            .playing
            .as_ref()
            .and_then(|path| self.index_for_path(path));
        if self.config.shuffle {
            if let Some(current) = self.playback.playing_index {
                self.reanchor_shuffle_bag(current);
            } else {
                self.playback.order.clear();
            }
        }
        self.view.pending_selected_path = remembered.selected;
        self.restore_pending_selection();
    }

    /// 应用一次完整扫描结果，并按路径尽量保留正在播放与选中的歌曲位置。
    fn apply_scan_finished(&mut self, tracks: Vec<Track>, warnings: Vec<String>) {
        let filtered = self.catalog.folder().cloned();
        let startup_note = self
            .startup_note
            .take()
            .filter(|note| self.view.message.as_ref() == Some(note));
        self.replace_tracks(tracks);
        self.follow_renamed_progress();
        self.follow_renamed_marks();
        let playlist_note = self.follow_renamed_playlists();
        // 重扫往往是因为刚放进了 .lrc 文件，下一个 tick 重新读取歌词。
        self.playback.lyrics = None;
        self.scan_progress = None;
        let count = self.catalog.len();
        self.view.message = if warnings.is_empty() {
            Some(format!("扫描完成，共 {count} 首歌曲"))
        } else {
            Some(format!(
                "扫描完成，共 {count} 首歌曲；{} 个文件或目录无法读取",
                warnings.len()
            ))
        };
        self.restore_session();
        // 启动时的提示比扫描完成、恢复播放更要紧；曲目数在标题栏，恢复的曲目在播放区域都看得到。
        if let Some(note) = startup_note {
            self.view.message = Some(note);
        }
        // 放在恢复会话之后，这两条说明才不会被恢复提示覆盖。
        let folder_note = filtered
            .filter(|_| self.catalog.folder().is_none())
            .map(|folder| format!("文件夹“{}”已不存在，改为显示全部", folder.label()));
        for note in [folder_note, playlist_note].into_iter().flatten() {
            self.view.message = Some(match self.view.message.take() {
                Some(message) => format!("{message}；{note}"),
                None => note,
            });
        }
    }

    pub fn save_settings(&mut self) -> Result<(), String> {
        if !self.save_config_on_exit {
            return Ok(());
        }
        self.config.volume = self.playback.player.volume();
        self.config.muted = self.playback.player.is_muted();
        self.config
            .save_changes(&self.config_at_start, &self.paths.config_file)
    }

    pub(super) fn index_for_path(&self, path: &Path) -> Option<usize> {
        self.catalog.index_of(path)
    }
}

/// 把几条启动提示按顺序接在一起，每条另起一行，底栏会逐条显示；没有提示时为 None。
pub fn join_notes(notes: impl IntoIterator<Item = Option<String>>) -> Option<String> {
    let notes: Vec<String> = notes.into_iter().flatten().collect();
    (!notes.is_empty()).then(|| notes.join("\n"))
}
