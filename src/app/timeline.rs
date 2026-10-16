//! 当前播放项的时间轴：文件时间和显示时间的换算。
//!
//! 播放器、跳转点、歌词和收听进度都用文件时间。界面和系统媒体控制看到的是显示时间，
//! 即文件时间减去播放项的开头。曲库播放从 0:00 开始，显示时间就是文件时间；
//! 播放列表里的片段从片段自己的开头算起。

use std::time::Duration;

use super::App;
use super::scope::Origin;

/// 当前播放项在文件里占的一段，用文件时间。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Span {
    pub(super) start: Duration,
    /// 结尾；不知道文件时长时为 None。
    pub(super) end: Option<Duration>,
}

impl App {
    /// 当前曲目文件的时长：解码器报告的优先，其次是扫描时读到的。
    pub fn effective_duration(&self) -> Option<Duration> {
        self.playback
            .player
            .duration()
            .or_else(|| self.current_track().and_then(|track| track.duration))
    }

    /// 当前播放项在文件里占的一段：曲库播放是整个文件，播放列表的条目是它自己的范围。
    /// 片段的结尾超出文件时长（文件被剪短了）时，放到文件结尾为止。
    pub(super) fn span(&self) -> Span {
        let duration = self.effective_duration();
        match &self.playback.origin {
            Origin::Entry { range, .. } if self.current_track().is_some() => Span {
                start: range.start,
                end: match (range.end, duration) {
                    (Some(end), Some(duration)) => Some(end.min(duration)),
                    (end, duration) => end.or(duration),
                },
            },
            Origin::Entry { .. } | Origin::Library => Span {
                start: Duration::ZERO,
                end: duration,
            },
        }
    }

    /// 显示时间里的这个位置是不是已经越过了当前这一项的结尾。曲库播放跳到正好结尾时让它自己播完；
    /// 播放列表的条目在结尾上就算越过：片段的结尾是硬的，而播放器的停止位置只在一路播到时才生效，
    /// 跳到它上面会一直放到文件结尾。
    pub(super) fn beyond_end(&self, shown: Duration) -> bool {
        let Some(duration) = self.shown_duration() else {
            return false;
        };
        match self.playback.origin {
            Origin::Library => shown > duration,
            Origin::Entry { .. } => shown >= duration,
        }
    }

    /// 当前这一项在界面和系统媒体控制里的名字：文件名；播放列表的条目有名字时接在后面。
    pub fn item_title(&self) -> Option<String> {
        let title = self.current_track()?.display_title();
        Some(match &self.playback.origin {
            Origin::Entry {
                name: Some(name), ..
            } => format!("{title} · {name}"),
            Origin::Entry { .. } | Origin::Library => title.to_owned(),
        })
    }

    /// 文件时间换成显示时间；开头之前的算作 0:00。
    pub fn shown(&self, file_time: Duration) -> Duration {
        file_time.saturating_sub(self.span().start)
    }

    /// 显示时间换回文件时间。
    pub(super) fn file_time(&self, shown: Duration) -> Duration {
        self.span().start + shown
    }

    /// 显示用的当前位置。
    pub fn shown_position(&self) -> Duration {
        self.shown(self.playback.player.position())
    }

    /// 显示用的总时长：从开头到结尾。不知道时长时为 None。
    pub fn shown_duration(&self) -> Option<Duration> {
        let span = self.span();
        span.end.map(|end| end.saturating_sub(span.start))
    }

    /// 显示用的播放进度，0.0–1.0；不知道总时长时为 0。
    pub fn shown_ratio(&self) -> f64 {
        self.shown_duration()
            .filter(|duration| !duration.is_zero())
            .map(|duration| self.shown_position().as_secs_f64() / duration.as_secs_f64())
            .unwrap_or_default()
            .clamp(0.0, 1.0)
    }
}
