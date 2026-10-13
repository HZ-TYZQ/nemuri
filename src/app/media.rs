//! 跳转、Seeked 通知与系统媒体会话快照。

use std::time::Duration;

use crate::media::{MediaEvent, MediaSnapshot};
use crate::player::PlayState;

use super::App;

impl App {
    pub(super) fn seek_rel_micros(&mut self, offset: i64) {
        if self.output_blocks_playback() {
            return;
        }
        if self.playback.player.state() == PlayState::Stopped {
            return;
        }
        let current = duration_as_micros(self.playback.player.position());
        let requested = current.saturating_add(offset);
        if requested < 0 {
            if self.playback.player.seek_to(Duration::ZERO) {
                self.push_seeked(Duration::ZERO);
            }
            return;
        }
        if let Some(duration) = self.effective_duration()
            && requested > duration_as_micros(duration)
        {
            self.play_next(false);
            return;
        }
        let target = Duration::from_micros(requested as u64);
        if self.playback.player.seek_to(target) {
            self.push_seeked(self.playback.player.position());
        }
    }

    /// 按进度条比例跳转。没有时长就无从换算，直接忽略。
    pub(super) fn seek_to_ratio(&mut self, ratio: f64) {
        let Some(duration) = self.effective_duration() else {
            return;
        };
        let ratio = ratio.clamp(0.0, 1.0);
        self.seek_to_requested(duration.mul_f64(ratio), None);
    }

    pub(super) fn seek_to_requested(&mut self, position: Duration, track_id: Option<&str>) {
        if self.output_blocks_playback() {
            return;
        }
        if self.playback.player.state() == PlayState::Stopped {
            return;
        }
        if let Some(expected) = track_id
            && expected != self.current_track_id()
        {
            return;
        }
        if let Some(duration) = self.effective_duration()
            && position > duration
        {
            return;
        }
        if self.playback.player.seek_to(position) {
            self.push_seeked(self.playback.player.position());
        }
    }

    pub(super) fn effective_duration(&self) -> Option<Duration> {
        self.playback
            .player
            .duration()
            .or_else(|| self.current_track().and_then(|track| track.duration))
    }

    fn push_seeked(&mut self, position: Duration) {
        self.playback
            .media_events
            .push(MediaEvent::Seeked { position });
    }

    pub fn drain_media_events(&mut self) -> Vec<MediaEvent> {
        std::mem::take(&mut self.playback.media_events)
    }

    pub fn media_snapshot(&self) -> MediaSnapshot {
        let track = self.current_track();
        MediaSnapshot {
            status: self.playback.player.state(),
            title: track
                .map(|track| track.display_title().to_owned())
                .unwrap_or_default(),
            artist: track.and_then(|track| track.artist.clone()),
            album: track.and_then(|track| track.album.clone()),
            path: track.map(|track| track.path.clone()),
            duration: self.effective_duration(),
            position: self.playback.player.position(),
            volume: self.playback.player.volume(),
            muted: self.playback.player.is_muted(),
            repeat: self.config.repeat,
            shuffle: self.config.shuffle,
            can_go_previous: !self.playback.history.is_empty(),
            can_go_next: self.can_go_next(),
            track_id: self.current_track_id(),
        }
    }

    fn current_track_id(&self) -> String {
        match self.playback.playing_index {
            Some(index) => format!("/org/mpris/MediaPlayer2/Track/{index}"),
            None => "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_owned(),
        }
    }

    fn can_go_next(&self) -> bool {
        if !self.playback.queue.is_empty() {
            return true;
        }
        self.playback.order.has_next(
            self.catalog.scope(),
            self.playback.playing_index,
            self.playback_mode(),
        )
    }
}

fn duration_as_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}
