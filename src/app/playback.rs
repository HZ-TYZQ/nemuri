//! 播放、临时队列、历史与随机播放行为。

use std::path::Path;
use std::time::Duration;

use crate::config::AppConfig;
use crate::player::{ChannelMix, MAX_BALANCE, PlayState};
use crate::playlist::Saved;
use crate::track::{RepeatMode, fmt_duration};

use super::playlists::saved_message;
use super::progress::Opening;
use super::scope::{Cursor, Origin, Played, ScopeMark};
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

/// 配置里的平衡和互换，交给播放后端的样子。
pub(super) fn channel_mix(config: &AppConfig) -> ChannelMix {
    ChannelMix {
        balance: config.balance,
        swapped: config.swap_channels,
    }
}

pub(super) fn file_label(path: &Path) -> String {
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

    /// 在曲库里播放选中的曲目，回到曲库范围。
    pub(super) fn play_selected(&mut self) {
        if self.output_blocks_playback() {
            return;
        }
        let Some(index) = self.selected_track_index() else {
            return;
        };
        match self.play_index(index, true, BagUpdate::Reanchor) {
            Ok(()) => self.enter_library(),
            Err(skip) => self.view.message = Some(skip.into_message()),
        }
    }

    /// 按曲库播放一首：用录音的起点、终点和收听进度。不改变播放范围。
    pub(super) fn play_index(
        &mut self,
        index: usize,
        remember_current: bool,
        bag: BagUpdate,
    ) -> Result<(), Skip> {
        self.play_item(index, Origin::Library, remember_current, bag)
    }

    /// 装载曲库里的第 `index` 首，按 `origin` 播放。`bag` 管曲库的随机袋。
    ///
    /// 切歌失败的原因不写进 `message`，而是交回调用方：
    /// 直接播放要立刻报错，自动跳过则要等落到能播的曲目后再汇总。
    pub(super) fn play_item(
        &mut self,
        index: usize,
        origin: Origin,
        remember_current: bool,
        bag: BagUpdate,
    ) -> Result<(), Skip> {
        let Some(track) = self.catalog.tracks().get(index) else {
            return Err(Skip::new(format!("#{}", index + 1), "不在曲库中"));
        };
        let path = track.path.clone();
        let title = match &origin {
            Origin::Entry {
                name: Some(name), ..
            } => format!("{} · {name}", track.display_title()),
            Origin::Entry { .. } | Origin::Library => track.display_title().to_owned(),
        };
        // 文件被剪短了，片段的开头已经在文件之外：不放，也不退回 0:00。
        if let Origin::Entry { range, .. } = &origin
            && track
                .duration
                .is_some_and(|duration| range.start >= duration)
        {
            return Err(Skip::new(
                title,
                format!("片段开头 {} 超出文件时长", fmt_duration(range.start)),
            ));
        }
        let previous = remember_current
            .then(|| self.current_played())
            .flatten()
            .filter(|current| current.path != path || current.origin != origin);
        // 先记下正在放的这一条停在哪里，再换；换的若是同一条，就接着放。
        self.note_progress();
        let autoplay = self.playback.player.state() != PlayState::Paused;
        // 列表里的条目不续播，也不用录音的起点，从条目自己的开头开始；
        // 从开头装载不成时不退回 0:00，开头之前不是这一条的内容。
        let (opening, exact) = match &origin {
            Origin::Library => (self.opening(index), false),
            Origin::Entry { range, .. } => (Opening::Clip(range.start), true),
        };
        match self.load_track(&path, autoplay, opening.position(), exact, &origin) {
            Ok(opened_at) => {
                self.playback.loads += 1;
                self.start_playthrough();
                // 只有切歌成功后才改动历史与频谱状态；加载失败时旧曲仍是当前曲目。
                if let Some(previous) = previous {
                    self.playback.history.push(previous);
                }
                // 保留已收敛的 sensitivity，重新进入 fast-adapt（见 spectrum.rs）。
                self.playback.spectrum.on_track_change();
                self.playback.playing_index = Some(index);
                self.playback.origin = origin;
                self.apply_stop();
                // 从记下的位置或起点开不成时退回了 0:00，那就不用说明。
                self.view.message = opened_at.and_then(|_| opening.message());
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
            Err(error) => Err(Skip::new(title, short_reason(&error, &path))),
        }
    }

    /// 正在放的这一项在历史里的记法，连同现在的播放范围。
    fn current_played(&self) -> Option<Played> {
        Some(Played {
            path: self.current_track()?.path.clone(),
            origin: self.playback.origin.clone(),
            scope: self.scope_mark(),
        })
    }

    /// 装载曲目：`start` 有值时从那里开始，播放中切歌时接着播放、暂停中只装载。
    /// 从中间装载失败（例如文件被剪短）时退回从头装载；`exact` 时不退回，直接报错。
    /// 返回实际开始的位置，从头时为 None。
    ///
    /// 总是先暂停着装载，再按实际开始的位置设上新的一项（来自 `origin`）截断处的增益，
    /// 然后才开始放。播放器打开和解码新文件时旧的还在出声，装载完成时才换下它；
    /// 先设增益的话，旧的那一条会突然变响或没声。装载失败时增益不变，旧的照常放。
    fn load_track(
        &mut self,
        path: &Path,
        autoplay: bool,
        start: Option<Duration>,
        exact: bool,
        origin: &Origin,
    ) -> Result<Option<Duration>, String> {
        let player = &mut self.playback.player;
        let opened = match start {
            Some(position) => match player.open_at(path, position) {
                Ok(()) => Some(position),
                Err(error) if exact => return Err(error),
                Err(_) => {
                    player.open(path)?;
                    None
                }
            },
            None => {
                player.open(path)?;
                None
            }
        };
        // 曲库播放从起点或记下的位置开始时淡入，退回 0:00 的不淡；片段按自己的开头淡入。
        self.playback.fade_in_from = match origin {
            Origin::Library => opened,
            Origin::Entry { .. } => None,
        };
        self.set_clip_gain(self.starting_clip_gain(origin));
        if autoplay {
            self.playback.player.resume();
        }
        Ok(opened)
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
            .scope_len()
            .saturating_add(self.playback.queue.len())
            .max(1);
        let mut library_cursor = self
            .playback
            .playing_index
            .or_else(|| self.selected_track_index());
        let mut natural_end = natural_end;
        for _ in 0..max_attempts {
            // 队列里的是插播，按曲库播放，不改变播放范围。
            if let Some(path) = self.take_queue_head() {
                match self.play_path(&path, true, BagUpdate::Leave) {
                    Ok(()) => {
                        self.report_skipped(&skipped);
                        return;
                    }
                    Err(skip) => skipped.push(skip),
                }
                continue;
            }
            let played = match self.in_playlist() {
                Some(playlist) => {
                    let Some(entry) = self.next_entry(natural_end) else {
                        self.stop_at_scope_end(natural_end, &skipped);
                        return;
                    };
                    let played = self.play_entry(playlist, entry, true);
                    // 放不成也移过去，下一次接着往后试。
                    self.move_cursor_to(entry);
                    played
                }
                None => {
                    let Some(next) = self.next_library_index_from(library_cursor, natural_end)
                    else {
                        self.stop_at_scope_end(natural_end, &skipped);
                        return;
                    };
                    // 加载失败不会改变 playing_index，因此用局部游标继续向后尝试。
                    library_cursor = Some(next);
                    self.play_index(next, true, BagUpdate::Leave)
                }
            };
            match played {
                Ok(()) => {
                    self.report_skipped(&skipped);
                    return;
                }
                Err(skip) => skipped.push(skip),
            }
            // 单曲循环在自然结束后优先重载当前曲；若重载失败，
            // 后续尝试应按“播放错误”处理，不再锁定同一曲。
            natural_end = false;
        }
        self.note_progress();
        self.playback.player.stop();
        self.playback.playing_index = None;
        self.view.message = Some(no_playable_message("下一首", skipped.len()));
    }

    /// 当前范围放完了：停下并说明原因。
    fn stop_at_scope_end(&mut self, natural_end: bool, skipped: &[Skip]) {
        self.note_progress();
        self.playback.player.stop();
        self.playback.playing_index = None;
        let repeat = self.playback_mode().repeat;
        // 默认的循环方式会在每首播完时停下，说明原因，免得以为出了故障。
        if natural_end && repeat == RepeatMode::Stop {
            self.view.message = Some("已播完，播放停止（按 z 切换循环方式）".to_owned());
        } else if let Some((name, _, _)) = self.scope_summary() {
            self.view.message = Some(format!("播放列表“{name}”已播完"));
        }
        self.report_skipped(skipped);
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
        let mode = self.library_mode();
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
        while let Some(played) = self.playback.history.pop() {
            match self.replay(played) {
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

    /// 退回历史里的一项，连同放它时的播放范围。
    fn replay(&mut self, played: Played) -> Result<(), Skip> {
        match played.origin {
            Origin::Entry {
                playlist, entry, ..
            } if self.entry_exists(playlist, entry) => {
                self.play_entry(playlist, entry, false)?;
                self.enter_playlist(playlist, Cursor::At(entry));
                Ok(())
            }
            // 列表或这一条已经删掉：按当时的范围放一次，当作插播。
            origin @ Origin::Entry { .. } => {
                let index = self
                    .index_for_path(&played.path)
                    .ok_or_else(|| Skip::new(file_label(&played.path), "不在曲库中"))?;
                self.play_item(index, origin, false, BagUpdate::Leave)
            }
            Origin::Library => {
                let bag = match played.scope {
                    ScopeMark::Library => BagUpdate::Reanchor,
                    ScopeMark::Playlist { .. } => BagUpdate::Leave,
                };
                self.play_path(&played.path, false, bag)?;
                self.restore_scope(played.scope);
                Ok(())
            }
        }
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

    /// `,` / `.`：平衡往左或往右挪，到头为止。
    pub(super) fn shift_balance(&mut self, delta: i8) {
        let balance = self
            .config
            .balance
            .saturating_add(delta)
            .clamp(-MAX_BALANCE, MAX_BALANCE);
        self.config.balance = balance;
        self.playback
            .player
            .set_channel_mix(channel_mix(&self.config));
        self.view.message = Some(match balance {
            0 => "平衡：居中".to_owned(),
            1.. => format!("平衡：偏右 {balance}（左声道 {}%）", 100 - balance),
            _ => format!(
                "平衡：偏左 {}（右声道 {}%）",
                balance.unsigned_abs(),
                100 + balance
            ),
        });
    }

    pub(super) fn toggle_channel_swap(&mut self) {
        self.config.swap_channels = !self.config.swap_channels;
        self.playback
            .player
            .set_channel_mix(channel_mix(&self.config));
        self.view.message = Some(
            if self.config.swap_channels {
                "已互换左右声道"
            } else {
                "左右声道已恢复"
            }
            .to_owned(),
        );
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

    /// 系统媒体控制改当前范围的循环方式。系统只有“不循环”，分不出播完停止和顺序播放：
    /// 已经是这两种之一时保持不变；从循环切过来时用这个范围的默认值，
    /// 曲库是播完停止，播放列表是顺序。
    pub(super) fn set_repeat(&mut self, repeat: RepeatMode) {
        let current = self.playback_mode().repeat;
        let looping = matches!(current, RepeatMode::All | RepeatMode::One);
        let not_looping = match self.in_playlist() {
            Some(_) => RepeatMode::None,
            None => RepeatMode::Stop,
        };
        let repeat = match repeat {
            RepeatMode::Stop | RepeatMode::None if !looping => current,
            RepeatMode::Stop | RepeatMode::None => not_looping,
            looping @ (RepeatMode::All | RepeatMode::One) => looping,
        };
        if repeat == current {
            return;
        }
        // 切换本身在系统那边看得到，只在列表没能写盘时说明。
        if let Some((name, saved @ Saved::No(_))) = self.apply_repeat(repeat) {
            self.view.message = Some(saved_message(
                format!("列表“{name}”循环：{}", repeat.label()),
                saved,
            ));
        }
    }

    /// `z`：切换当前范围的循环方式。列表范围里切的是这个列表的，改了就写盘；配置里曲库的不变。
    pub(super) fn cycle_repeat(&mut self) {
        let repeat = self.playback_mode().repeat.next();
        let message = match self.apply_repeat(repeat) {
            Some((name, saved)) => {
                saved_message(format!("列表“{name}”循环：{}", repeat.label()), saved)
            }
            None => format!("循环：{}", repeat.label()),
        };
        self.view.message = Some(message);
    }

    /// 把循环方式设到当前范围上。列表范围交回列表名和写盘的结果，曲库范围为 None。
    fn apply_repeat(&mut self, repeat: RepeatMode) -> Option<(String, Saved)> {
        let Some(id) = self.in_playlist() else {
            self.config.repeat = repeat;
            return None;
        };
        let saved = self.playlists.set_repeat(id, repeat).ok()?;
        let name = self.playlists.get(id)?.name.clone();
        Some((name, saved))
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
        self.reanchor_playlist_bag();
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
                // 列表范围里从当前条目放；列表播完停下时就是最后一条。
                if let Some((playlist, entry)) = self.entry_to_resume() {
                    if let Err(skip) = self.play_entry(playlist, entry, false) {
                        self.view.message = Some(skip.into_message());
                    }
                    return;
                }
                let Some(index) = self
                    .playback
                    .playing_index
                    .or_else(|| self.selected_track_index())
                else {
                    return;
                };
                match self.play_index(index, false, BagUpdate::Reanchor) {
                    Ok(()) => self.enter_library(),
                    Err(skip) => self.view.message = Some(skip.into_message()),
                }
            }
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
