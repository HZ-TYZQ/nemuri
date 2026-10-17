//! 淡出增益：睡眠定时和播放列表里片段的切换各管一份，交给播放器的是较小的那个。
//! 不相乘：两段淡出重叠时，相乘会比哪一个降得都快。
//!
//! 片段截在录音中间，放到结尾时声音被一刀切断、下一条又从另一段声音中间突然开始。
//! 所以截在中间的结尾淡出（`playback { clip-fade }` 秒），从中间开始的开头用三分之一的时间淡入。
//! 增益按播放位置算：暂停时停在当时的音量，往后跳越过结尾照旧立刻换下一条，
//! 往回跳到开头附近会重新淡入。曲库播放（包括插播）不淡。

use std::time::Duration;

use crate::player::PlayState;
use crate::playlist::Range;

use super::App;
use super::scope::Origin;

impl App {
    /// 每个 tick 按当前位置更新片段那一份增益。
    pub(super) fn tick_clip_fade(&mut self) {
        let gain = match &self.playback.origin {
            Origin::Entry { range, .. } if self.playback.player.state() != PlayState::Stopped => {
                clip_gain(
                    *range,
                    self.effective_duration(),
                    self.playback.player.position(),
                    self.config.clip_fade,
                )
            }
            Origin::Entry { .. } | Origin::Library => 1.0,
        };
        self.set_clip_gain(gain);
    }

    /// 装载 `origin` 之前先按它的开头设好片段那一份增益：要淡入的从无声开始，不会先漏出一下原音量；
    /// 不淡入的设回 1，不会被上一条淡出到 0 的增益压住。
    pub(super) fn prepare_clip_fade(&mut self, origin: &Origin) {
        let gain = match origin {
            Origin::Entry { range, .. } => {
                clip_gain(*range, None, range.start, self.config.clip_fade)
            }
            Origin::Library => 1.0,
        };
        self.set_clip_gain(gain);
    }

    pub(super) fn set_sleep_gain(&mut self, gain: f32) {
        self.playback.sleep_gain = gain;
        self.apply_fade_gain();
    }

    fn set_clip_gain(&mut self, gain: f32) {
        self.playback.clip_gain = gain;
        self.apply_fade_gain();
    }

    /// 两份里较小的那个交给播放器；没变时不重复设置。
    fn apply_fade_gain(&mut self) {
        let gain = self.playback.sleep_gain.min(self.playback.clip_gain);
        if self.playback.fade_gain != gain {
            self.playback.fade_gain = gain;
            self.playback.player.set_fade_gain(gain);
        }
    }
}

/// 剩余时间进入淡出段后，增益按剩余比例的平方下降：线性降低音量时，
/// 听感上前半段几乎不变、最后几秒才突然变小，平方曲线听起来更均匀。
/// 淡入时把已经放了多久当作 `remaining` 传进来，就是同一条曲线倒过来。
pub(super) fn fade_gain(remaining: Duration, fade: Duration) -> f32 {
    if fade.is_zero() || remaining >= fade {
        return 1.0;
    }
    let fraction = remaining.as_secs_f32() / fade.as_secs_f32();
    fraction * fraction
}

/// 片段在 `position`（文件时间）处的增益。`fade_out` 为 0 时不淡。
///
/// 结尾在文件结尾之前才淡出，录音本来就在文件结尾结束；不知道时长时按条目写的结尾算。
/// 开头不在 0:00 才淡入，淡入时长是淡出的三分之一。片段比两段加起来还短时，各不超过片段的一半。
fn clip_gain(
    range: Range,
    duration: Option<Duration>,
    position: Duration,
    fade_out: Duration,
) -> f32 {
    let cut_end = range
        .end
        .filter(|end| duration.is_none_or(|duration| *end < duration));
    let half = cut_end
        .or(duration)
        .map_or(Duration::MAX, |end| end.saturating_sub(range.start) / 2);
    let out = cut_end.map_or(1.0, |end| {
        fade_gain(end.saturating_sub(position), fade_out.min(half))
    });
    let into = if range.start.is_zero() {
        1.0
    } else {
        fade_gain(
            position.saturating_sub(range.start),
            (fade_out / 3).min(half),
        )
    };
    out.min(into)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECOND: Duration = Duration::from_secs(1);

    fn clip(start: u64, end: Option<u64>) -> Range {
        Range {
            start: SECOND * start as u32,
            end: end.map(|end| SECOND * end as u32),
        }
    }

    fn at(seconds: f32) -> Duration {
        Duration::from_secs_f32(seconds)
    }

    #[test]
    fn the_fade_starts_at_full_volume_and_ends_silent() {
        let fade = Duration::from_secs(30);
        assert_eq!(fade_gain(Duration::from_secs(60), fade), 1.0);
        assert_eq!(fade_gain(fade, fade), 1.0);
        assert_eq!(fade_gain(Duration::from_secs(15), fade), 0.25);
        assert_eq!(fade_gain(Duration::ZERO, fade), 0.0);
        assert_eq!(fade_gain(Duration::from_secs(1), Duration::ZERO), 1.0);
    }

    #[test]
    fn a_clip_cut_from_the_middle_fades_in_and_out() {
        let length = Some(SECOND * 600);
        let fade = SECOND * 3;
        let gain = |position: f32| clip_gain(clip(100, Some(200)), length, at(position), fade);
        assert_eq!(gain(100.0), 0.0);
        assert_eq!(gain(100.5), 0.25);
        assert_eq!(gain(101.0), 1.0);
        assert_eq!(gain(150.0), 1.0);
        assert_eq!(gain(197.0), 1.0);
        assert_eq!(gain(198.5), 0.25);
        assert_eq!(gain(200.0), 0.0);
    }

    #[test]
    fn ends_that_are_the_recording_s_own_are_not_faded() {
        let length = Some(SECOND * 600);
        let fade = SECOND * 3;
        // 从 0:00 开始不淡入；放到文件结尾（没写结尾，或结尾超出了时长）不淡出。
        assert_eq!(clip_gain(clip(0, Some(200)), length, at(0.0), fade), 1.0);
        assert_eq!(clip_gain(clip(100, None), length, at(599.0), fade), 1.0);
        assert_eq!(
            clip_gain(clip(100, Some(700)), length, at(599.0), fade),
            1.0
        );
        assert_eq!(
            clip_gain(clip(100, Some(600)), length, at(599.0), fade),
            1.0
        );
        // 关掉时不淡。
        assert_eq!(
            clip_gain(clip(100, Some(200)), length, at(100.0), Duration::ZERO),
            1.0
        );
    }

    #[test]
    fn a_short_clip_fades_over_at_most_half_its_length_each_way() {
        let length = Some(SECOND * 600);
        let fade = SECOND * 30;
        // 4 秒的片段：淡入淡出各 2 秒，正中间是满音量。
        let gain = |position: f32| clip_gain(clip(100, Some(104)), length, at(position), fade);
        assert_eq!(gain(101.0), 0.25);
        assert_eq!(gain(102.0), 1.0);
        assert_eq!(gain(103.0), 0.25);
    }

    #[test]
    fn before_loading_a_clip_from_the_middle_starts_silent() {
        // 装载前还不知道时长。
        let fade = SECOND * 3;
        assert_eq!(clip_gain(clip(100, Some(200)), None, at(100.0), fade), 0.0);
        assert_eq!(clip_gain(clip(0, Some(2)), None, at(0.0), fade), 1.0);
    }
}
