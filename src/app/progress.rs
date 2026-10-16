//! 把正在播放的长录音的位置记进收听进度，开始播放时从记下的位置接着放，并按需写盘。
//!
//! 位置每个 tick 记一次，但最多每 30 秒写一次盘；停下来（暂停、定时到点、播完、出错）
//! 或刚记下“听完”时在下一个 tick 立刻写，不必等满 30 秒。

use std::path::Path;
use std::time::{Duration, Instant};

use crate::player::PlayState;
use crate::progress::{self, Listened, ProgressStore};
use crate::track::Track;

use super::App;

/// 记录有变化时，两次写盘至少间隔这么久。
const SAVE_INTERVAL: Duration = Duration::from_secs(30);

/// 收听进度和写盘节奏。
pub(super) struct Progress {
    pub(super) store: ProgressStore,
    /// 有没写盘的变化。
    dirty: bool,
    /// 下一个 tick 就写，不等间隔。
    urgent: bool,
    /// 上一个 tick 是否在播放；从播放变成不播放时要立刻写盘。
    was_playing: bool,
    /// 不早于这个时间再写盘。
    next_save: Instant,
    /// 当前这一遍已经记为听完。之后切走或停下时不再按位置记录，
    /// 免得把“听完”盖回成跳过结尾之前的位置；换上下一遍播放时清除。
    finished_playthrough: bool,
}

impl Progress {
    pub(super) fn new(store: ProgressStore, now: Instant) -> Self {
        Self {
            store,
            dirty: false,
            urgent: false,
            was_playing: false,
            next_save: now + SAVE_INTERVAL,
            finished_playthrough: false,
        }
    }
}

impl App {
    /// 当前曲目是长录音时，记下它现在的位置。停止状态没有位置可记；
    /// 这一遍已经记为听完时也不记。
    pub(super) fn note_progress(&mut self) {
        if self.progress.finished_playthrough || self.playback.player.state() == PlayState::Stopped
        {
            return;
        }
        let Some(duration) = self.effective_duration() else {
            return;
        };
        let Some(track) = self
            .playback
            .playing_index
            .and_then(|index| self.catalog.get(index))
            .filter(|track| progress::is_long(track))
        else {
            return;
        };
        let position = self.playback.player.position();
        let bounds = self.current_bounds();
        let start = bounds.start.unwrap_or_default();
        let end = bounds.end.unwrap_or(duration);
        if self.progress.store.record(track, position, start, end) {
            self.progress.dirty = true;
        }
    }

    /// 当前曲目听完了：自然播完，或者往后跳过了结尾。
    ///
    /// 往后跳过结尾时播放器还停在跳转前的位置，接着切歌或停止又会先记一次当前位置；
    /// 所以这一遍从此不再记录，直到换上下一遍播放。
    pub(super) fn mark_finished(&mut self) {
        self.progress.finished_playthrough = true;
        let Some(track) = self
            .playback
            .playing_index
            .and_then(|index| self.catalog.get(index))
            .filter(|track| progress::is_long(track))
        else {
            return;
        };
        if self.progress.store.finish(track) {
            self.progress.dirty = true;
            self.progress.urgent = true;
        }
    }

    /// 换上了新的一遍播放（包括同一条重新开始），又可以按位置记录了。
    pub(super) fn start_playthrough(&mut self) {
        self.progress.finished_playthrough = false;
    }

    /// 曲库里显示的收听进度；只有长录音才有。
    pub fn listened(&self, track: &Track) -> Option<Listened> {
        if !progress::is_long(track) {
            return None;
        }
        self.progress.store.get(&track.path)
    }

    /// 这一首该从哪里开始：听到一半的长录音从记下的位置继续，其余从头。
    pub(super) fn resume_point(&self, index: usize) -> Option<Duration> {
        let track = self
            .catalog
            .get(index)
            .filter(|track| progress::is_long(track))?;
        match self.progress.store.get(&track.path)? {
            Listened::InProgress(position) => Some(position),
            Listened::Finished => None,
        }
    }

    /// 扫描后把改了名的文件的进度跟过去。
    pub(super) fn follow_renamed_progress(&mut self) {
        if self
            .progress
            .store
            .follow_renames(self.catalog.tracks(), &self.library_dir)
        {
            self.progress.dirty = true;
            self.progress.urgent = true;
        }
    }

    pub(super) fn tick_progress(&mut self) {
        let playing = self.playback.player.state() == PlayState::Playing;
        // 刚停下的这个 tick 也记一次，拿到暂停时的准确位置。
        if playing || self.progress.was_playing {
            self.note_progress();
        }
        if self.progress.was_playing && !playing {
            self.progress.urgent = true;
        }
        self.progress.was_playing = playing;
        if !self.progress.dirty || (!self.progress.urgent && self.now < self.progress.next_save) {
            return;
        }
        self.progress.urgent = false;
        self.progress.next_save = self.now + SAVE_INTERVAL;
        match self.progress.store.save(&self.paths.progress_file) {
            Ok(()) => self.progress.dirty = false,
            // 保持 dirty，等下一个间隔再试。
            Err(error) => self.view.message = Some(save_error(&self.paths.progress_file, &error)),
        }
    }

    /// 退出时记下当前位置并写盘。
    pub fn save_progress(&mut self) -> Result<(), String> {
        self.note_progress();
        if !self.progress.dirty {
            return Ok(());
        }
        self.progress
            .store
            .save(&self.paths.progress_file)
            .map_err(|error| save_error(&self.paths.progress_file, &error))?;
        self.progress.dirty = false;
        Ok(())
    }
}

fn save_error(path: &Path, error: &std::io::Error) -> String {
    format!("无法保存收听进度 {}: {error}", path.display())
}
