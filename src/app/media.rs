//! 跳转、Seeked 通知与系统媒体会话快照。

use std::time::Duration;

use crate::media::{MediaEvent, MediaSnapshot};
use crate::player::PlayState;

use super::App;

/// 隐私模式下交给系统媒体控制的标题。
const PRIVATE_TITLE: &str = "正在播放";

impl App {
    pub(super) fn seek_rel_micros(&mut self, offset: i64) {
        if self.output_blocks_playback() {
            return;
        }
        if self.playback.player.state() == PlayState::Stopped {
            return;
        }
        // 按显示时间算：往前退到开头为止，往后越过结尾就是下一首。
        let current = duration_as_micros(self.shown_position());
        let requested = current.saturating_add(offset);
        if requested < 0 {
            let start = self.file_time(Duration::ZERO);
            if self.playback.player.seek_to(start) {
                self.seeked(start);
            }
            return;
        }
        if self.beyond_end(Duration::from_micros(requested as u64)) {
            // 往后跳过了结尾，当作这一条已经听完；播放列表的条目直接放下一条。
            self.mark_finished();
            self.play_next(false);
            return;
        }
        let target = self.file_time(Duration::from_micros(requested as u64));
        if self.playback.player.seek_to(target) {
            self.seeked(self.playback.player.position());
        }
    }

    /// 按进度条比例跳转，比例按显示的总时长算。没有时长就无从换算，直接忽略。
    /// 播放列表的条目点到最右端就是下一条。
    pub(super) fn seek_to_ratio(&mut self, ratio: f64) {
        let Some(duration) = self.shown_duration() else {
            return;
        };
        let target = duration.mul_f64(ratio.clamp(0.0, 1.0));
        if self.beyond_end(target) {
            if !self.output_blocks_playback() && self.playback.player.state() != PlayState::Stopped
            {
                self.play_next(false);
            }
            return;
        }
        self.seek_to_requested(self.file_time(target), None);
    }

    /// 系统媒体控制的 SetPosition：位置是显示时间。
    pub(super) fn seek_to_shown(&mut self, position: Duration, track_id: Option<&str>) -> bool {
        self.seek_to_requested(self.file_time(position), track_id)
    }

    /// 跳到文件里的绝对位置；跳成了返回 true。
    pub(super) fn seek_to_requested(&mut self, position: Duration, track_id: Option<&str>) -> bool {
        if self.output_blocks_playback() {
            return false;
        }
        if self.playback.player.state() == PlayState::Stopped {
            return false;
        }
        if let Some(expected) = track_id
            && expected != self.current_track_id()
        {
            return false;
        }
        if self.beyond_end(self.shown(position)) {
            return false;
        }
        let moved = self.playback.player.seek_to(position);
        if moved {
            self.seeked(self.playback.player.position());
        }
        moved
    }

    /// 跳成了：结束装载后的淡入（跳转不淡入），再告诉系统媒体控制跳到了哪里；
    /// `position` 是文件时间，交出去的是显示时间。
    fn seeked(&mut self, position: Duration) {
        self.end_fade_in();
        let position = self.shown(position);
        self.playback
            .media_events
            .push(MediaEvent::Seeked { position });
    }

    pub fn drain_media_events(&mut self) -> Vec<MediaEvent> {
        std::mem::take(&mut self.playback.media_events)
    }

    /// 隐私模式下只交出通用标题：桌面和锁屏的媒体小组件、连着的手机都看不到
    /// 在听什么。时长、位置和各项控制照常，播放器仍然可以从系统那边操作。
    pub(super) fn toggle_privacy(&mut self) {
        self.config.privacy = !self.config.privacy;
        self.view.message = Some(
            if self.config.privacy {
                "隐私模式已开启：系统媒体控制只显示“正在播放”"
            } else {
                "隐私模式已关闭"
            }
            .to_owned(),
        );
    }

    pub fn media_snapshot(&self) -> MediaSnapshot {
        let track = self.current_track();
        let shown = track.filter(|_| !self.config.privacy);
        MediaSnapshot {
            status: self.playback.player.state(),
            title: match (track, shown) {
                (_, Some(_)) => self.item_title().unwrap_or_default(),
                (Some(_), None) => PRIVATE_TITLE.to_owned(),
                (None, None) => String::new(),
            },
            artist: shown.and_then(|track| track.artist.clone()),
            album: shown.and_then(|track| track.album.clone()),
            path: shown.map(|track| track.path.clone()),
            duration: self.shown_duration(),
            position: self.shown_position(),
            volume: self.playback.player.volume(),
            muted: self.playback.player.is_muted(),
            repeat: self.playback_mode().repeat,
            shuffle: self.config.shuffle,
            can_go_previous: !self.playback.history.is_empty(),
            can_go_next: self.can_go_next(),
            track_id: self.current_track_id(),
        }
    }

    /// 每装载一次换一个 ID：同一个文件的两个片段先后播放时，系统那边才知道换了曲目、刷新长度。
    fn current_track_id(&self) -> String {
        match self.playback.playing_index {
            Some(_) => format!("/org/mpris/MediaPlayer2/Track/{}", self.playback.loads),
            None => "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_owned(),
        }
    }

    fn can_go_next(&self) -> bool {
        !self.playback.queue.is_empty() || self.scope_has_next()
    }
}

fn duration_as_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}
