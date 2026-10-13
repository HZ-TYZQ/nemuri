//! 播放、临时队列、历史与随机播放行为。

use std::path::Path;

use crate::player::PlayState;
use crate::track::{PlaybackMode, RepeatMode};

use super::{App, BagUpdate};

/// 一次切歌失败的可读原因。它不直接写进 `message`，
/// 因为自动跳过时要等落到能播的曲目后才汇总，否则会被成功切歌清掉。
#[derive(Debug)]
pub(super) struct Skip {
    name: String,
    reason: String,
}

impl Skip {
    pub(super) fn new(name: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            reason: reason.into(),
        }
    }

    /// 用户直接点播失败时的提示：这不是“跳过”，而是这一次没播成。
    pub(super) fn into_message(self) -> String {
        format!("无法播放“{}”：{}", self.name, self.reason)
    }
}

/// 播放器的错误串里带绝对路径，写日志合适，但提示只有一行，
/// 路径会把真正的原因挤出屏幕；提示已经点名了曲目，这里按已知路径裁掉。
pub(super) fn short_reason(error: &str, path: &Path) -> String {
    error
        .replace(&path.display().to_string(), "")
        .replace(" :", ":")
        .trim()
        .trim_end_matches(':')
        .trim_end()
        .to_owned()
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(str::to_owned)
        .unwrap_or_else(|| path.display().to_string())
}

impl App {
    pub(super) fn output_blocks_playback(&mut self) -> bool {
        if !self.playback.player.output_unavailable() {
            return false;
        }
        self.view.message = Some("音频输出中断，已保留进度；按空格重试".to_owned());
        true
    }

    pub(super) fn play_selected(&mut self) {
        if self.output_blocks_playback() {
            return;
        }
        if let Some(index) = self.selected_track_index()
            && let Err(skip) = self.play_index(index, true, BagUpdate::Reanchor)
        {
            self.view.message = Some(skip.into_message());
        }
    }

    /// 切歌失败的原因不写进 `message`，而是交回调用方：
    /// 直接播放要立刻报错，自动跳过则要等落到能播的曲目后再汇总。
    pub(super) fn play_index(
        &mut self,
        index: usize,
        remember_current: bool,
        bag: BagUpdate,
    ) -> Result<(), Skip> {
        let Some(track) = self.catalog.tracks().get(index) else {
            return Err(Skip::new(format!("#{}", index + 1), "不在曲库中"));
        };
        let path = track.path.clone();
        let previous = remember_current
            .then(|| self.current_track().map(|track| track.path.clone()))
            .flatten()
            .filter(|current| *current != path);
        let autoplay = self.playback.player.state() != PlayState::Paused;
        let result = if autoplay {
            self.playback.player.play(&path)
        } else {
            self.playback.player.open(&path)
        };
        match result {
            Ok(()) => {
                // 只有切歌成功后才改动历史与频谱状态；加载失败时旧曲仍是当前曲目。
                if let Some(previous) = previous {
                    self.playback.history.push(previous);
                }
                // 保留已收敛的 sensitivity，重新进入 fast-adapt（见 spectrum.rs）。
                self.playback.spectrum.on_track_change();
                self.playback.playing_index = Some(index);
                self.view.message = None;
                if let Some(visible) = self
                    .visible_indices()
                    .iter()
                    .position(|visible_index| *visible_index == index)
                {
                    self.view.selected = visible;
                }
                match bag {
                    BagUpdate::Reanchor if self.config.shuffle => self.reanchor_shuffle_bag(index),
                    BagUpdate::Reanchor | BagUpdate::Leave => {}
                }
                Ok(())
            }
            Err(error) => Err(Skip::new(
                self.catalog.tracks()[index].display_title().to_owned(),
                short_reason(&error, &path),
            )),
        }
    }

    pub(super) fn play_path(
        &mut self,
        path: &Path,
        remember_current: bool,
        bag: BagUpdate,
    ) -> Result<(), Skip> {
        match self.index_for_path(path) {
            Some(index) => self.play_index(index, remember_current, bag),
            None => Err(Skip::new(file_label(path), "不在曲库中")),
        }
    }

    pub(super) fn play_next(&mut self, natural_end: bool) {
        self.advance(natural_end, Vec::new());
    }

    /// `skipped` 让调用方带入这一轮已经发生的失败（例如播放途中解码出错），
    /// 与后续自动跳过合并成同一条提示，而不是被下一次成功切歌清掉。
    pub(super) fn advance(&mut self, natural_end: bool, mut skipped: Vec<Skip>) {
        if self.output_blocks_playback() {
            return;
        }
        if self.catalog.tracks().is_empty() {
            self.playback.playing_index = None;
            self.report_skipped(&skipped);
            return;
        }
        let max_attempts = self
            .catalog
            .scope()
            .len()
            .saturating_add(self.playback.queue.len())
            .max(1);
        let mut library_cursor = self
            .playback
            .playing_index
            .or_else(|| self.selected_track_index());
        let mut natural_end = natural_end;
        for _ in 0..max_attempts {
            if let Some(path) = self.playback.queue.pop_front() {
                match self.play_path(&path, true, BagUpdate::Leave) {
                    Ok(()) => {
                        self.report_skipped(&skipped);
                        return;
                    }
                    Err(skip) => skipped.push(skip),
                }
                continue;
            }
            let Some(next) = self.next_library_index_from(library_cursor, natural_end) else {
                self.playback.player.stop();
                self.playback.playing_index = None;
                // 默认的循环方式会在每首播完时停下，说明原因，免得以为出了故障。
                if natural_end && self.config.repeat == RepeatMode::Stop {
                    self.view.message = Some("已播完，播放停止（按 z 切换循环方式）".to_owned());
                }
                self.report_skipped(&skipped);
                return;
            };
            match self.play_index(next, true, BagUpdate::Leave) {
                Ok(()) => {
                    self.report_skipped(&skipped);
                    return;
                }
                Err(skip) => skipped.push(skip),
            }
            // 加载失败不会改变 playing_index，因此用局部游标继续向后尝试。
            // 单曲循环在自然结束后优先重载当前曲；若重载失败，
            // 后续尝试应按“播放错误”处理，不再锁定同一曲。
            library_cursor = Some(next);
            natural_end = false;
        }
        self.playback.player.stop();
        self.playback.playing_index = None;
        self.view.message = Some(no_playable_message("下一首", skipped.len()));
    }

    /// 成功切歌后汇总这一轮跳过的曲目。没有跳过就不碰 `message`，
    /// 让 `play_index` 的清屏行为保持原样。
    fn report_skipped(&mut self, skipped: &[Skip]) {
        let Some(first) = skipped.first() else {
            return;
        };
        self.view.message = Some(match skipped.len() {
            1 => format!("已跳过“{}”：{}", first.name, first.reason),
            count => format!(
                "已跳过 {count} 首无法播放的歌曲，其中“{}”：{}",
                first.name, first.reason
            ),
        });
    }

    #[cfg(test)]
    pub(super) fn next_library_index(&mut self, natural_end: bool) -> Option<usize> {
        let current = self
            .playback
            .playing_index
            .or_else(|| self.selected_track_index());
        self.next_library_index_from(current, natural_end)
    }

    fn next_library_index_from(
        &mut self,
        current: Option<usize>,
        natural_end: bool,
    ) -> Option<usize> {
        let mode = self.playback_mode();
        self.playback
            .order
            .next(self.catalog.scope(), current, natural_end, mode)
    }

    /// 随机袋在播放范围内重新洗牌；`current` 在范围里时作为本轮第一首。
    pub(super) fn reanchor_shuffle_bag(&mut self, current: usize) {
        self.playback
            .order
            .reanchor(self.catalog.scope(), Some(current));
    }

    pub(super) fn play_previous(&mut self) {
        if self.output_blocks_playback() {
            return;
        }
        let mut skipped = Vec::new();
        while let Some(path) = self.playback.history.pop() {
            match self.play_path(&path, false, BagUpdate::Reanchor) {
                Ok(()) => {
                    self.report_skipped(&skipped);
                    return;
                }
                Err(skip) => skipped.push(skip),
            }
        }
        self.view.message = Some(if skipped.is_empty() {
            "没有上一首播放记录".to_owned()
        } else {
            no_playable_message("上一首", skipped.len())
        });
    }

    pub(super) fn enqueue_selected(&mut self, next: bool) {
        let Some(path) = self.selected_track().map(|track| track.path.clone()) else {
            return;
        };
        if next {
            self.playback.queue.push_front(path);
            self.view.message = Some("已设为下一首".to_owned());
        } else {
            self.playback.queue.push_back(path);
            self.view.message = Some(format!(
                "已加入队列，队列中共 {} 首",
                self.playback.queue.len()
            ));
        }
    }

    pub(super) fn change_volume(&mut self, delta: i8) {
        let volume = if delta.is_negative() {
            self.playback
                .player
                .volume()
                .saturating_sub(delta.unsigned_abs())
        } else {
            self.playback
                .player
                .volume()
                .saturating_add(delta as u8)
                .min(100)
        };
        self.playback.player.set_volume(volume);
        self.config.volume = volume;
        self.view.message = Some(format!("音量 {volume}%"));
    }

    pub(super) fn toggle_mute(&mut self) {
        let muted = !self.playback.player.is_muted();
        self.playback.player.set_muted(muted);
        self.config.muted = muted;
        self.view.message = Some(
            if muted {
                "已静音"
            } else {
                "已取消静音"
            }
            .to_owned(),
        );
    }

    /// 系统媒体控制只有“不循环”，分不出播完停止和顺序播放：
    /// 已经是这两种之一时保持不变，从循环切过来时用播完停止。
    pub(super) fn set_repeat(&mut self, repeat: RepeatMode) {
        let looping = matches!(self.config.repeat, RepeatMode::All | RepeatMode::One);
        self.config.repeat = match repeat {
            RepeatMode::Stop | RepeatMode::None if !looping => self.config.repeat,
            RepeatMode::Stop | RepeatMode::None => RepeatMode::Stop,
            looping @ (RepeatMode::All | RepeatMode::One) => looping,
        };
    }

    pub(super) fn cycle_repeat(&mut self) {
        self.config.repeat = self.config.repeat.next();
        self.view.message = Some(format!("循环：{}", self.config.repeat.label()));
    }

    pub(super) fn toggle_shuffle(&mut self) {
        self.config.shuffle = !self.config.shuffle;
        if self.config.shuffle {
            if let Some(current) = self
                .playback
                .playing_index
                .or_else(|| self.selected_track_index())
            {
                self.reanchor_shuffle_bag(current);
            }
            self.view.message = Some("已开启随机播放".to_owned());
        } else {
            self.playback.order.clear();
            self.view.message = Some("已关闭随机播放".to_owned());
        }
    }

    pub(super) fn toggle_or_start(&mut self) {
        match self.playback.player.state() {
            PlayState::Playing => self.playback.player.pause(),
            PlayState::Paused | PlayState::Stopped => self.play_or_resume(),
        }
    }

    pub(super) fn play_or_resume(&mut self) {
        if self.playback.player.output_unavailable() {
            self.playback.player.resume();
            self.view.message = Some("正在恢复音频输出…".to_owned());
            return;
        }
        match self.playback.player.state() {
            PlayState::Playing => {}
            PlayState::Paused => self.playback.player.resume(),
            PlayState::Stopped => {
                if let Some(index) = self
                    .playback
                    .playing_index
                    .or_else(|| self.selected_track_index())
                    && let Err(skip) = self.play_index(index, false, BagUpdate::Reanchor)
                {
                    self.view.message = Some(skip.into_message());
                }
            }
        }
    }

    pub fn playback_mode(&self) -> PlaybackMode {
        PlaybackMode {
            repeat: self.config.repeat,
            shuffle: self.config.shuffle,
        }
    }

    pub(super) fn toggle_visualizer(&mut self) {
        self.config.visualizer_enabled = !self.config.visualizer_enabled;
        self.playback
            .player
            .set_spectrum_enabled(self.config.visualizer_enabled);
        if self.config.visualizer_enabled {
            self.view.message = Some("已开启音频频谱".to_owned());
        } else {
            self.playback.spectrum.reset_output();
            self.view.message = Some("已关闭音频频谱".to_owned());
        }
    }
}

fn no_playable_message(direction: &str, skipped: usize) -> String {
    if skipped == 0 {
        format!("没有可播放的{direction}歌曲")
    } else {
        format!("没有可播放的{direction}歌曲（已跳过 {skipped} 首）")
    }
}
