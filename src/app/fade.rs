//! 淡出增益：睡眠定时和声音被截断处的淡入淡出各管一份，交给播放器的是较小的那个。
//! 不相乘：两段淡出重叠时，相乘会比哪一个降得都快。
//!
//! 片段截在录音中间，放到结尾时声音被一刀切断、下一条又从另一段声音中间突然开始。
//! 所以截在中间的结尾淡出（`playback { clip-fade }` 秒），从中间开始的开头用三分之一的时间淡入。
//! 增益按播放位置算：暂停时停在当时的音量，往后跳越过结尾照旧立刻换下一条，
//! 往回跳到开头附近会重新淡入。
//!
//! 曲库播放（包括插播）用录音的起点和终点，时长和曲线与片段相同。终点前同样淡出；
//! 越过了终点就一直放到文件结尾，不淡。从起点或记下的位置装载时淡入：录音起点之前还能放，
//! 所以淡入跟着装载走，不按位置。位置一离开刚装载时的那一段、或者一跳转就结束，
//! 之后一路放过起点、跳到起点都不再淡入，和曲库里的跳转一样立刻出声。

use std::time::Duration;

use crate::marks::Bounds;
use crate::player::PlayState;
use crate::playlist::Range;

use super::App;
use super::scope::Origin;

impl App {
    /// 每个 tick 按当前位置更新截断处那一份增益。停下时恢复，装载后的淡入也就此结束。
    pub(super) fn tick_clip_fade(&mut self) {
        if self.playback.player.state() == PlayState::Stopped {
            self.playback.fade_in_from = None;
            self.set_clip_gain(1.0);
            return;
        }
        let position = self.playback.player.position();
        let fade_out = self.config.clip_fade;
        let gain = match &self.playback.origin {
            Origin::Entry { range, .. } => {
                clip_gain(*range, self.effective_duration(), position, fade_out)
            }
            Origin::Library => {
                let bounds = self.current_bounds();
                let into = self.fade_in_gain(bounds, position);
                end_point_gain(bounds, position, fade_out).min(into)
            }
        };
        self.set_clip_gain(gain);
    }

    /// 曲库播放装载后淡入的增益。位置离开 [开始的位置, 开始的位置 + 淡入时长) 时淡入结束，
    /// 这一遍不再淡入。
    fn fade_in_gain(&mut self, bounds: Bounds, position: Duration) -> f32 {
        let Some(from) = self.playback.fade_in_from else {
            return 1.0;
        };
        let length = fade_in_length(
            bounds,
            self.effective_duration(),
            from,
            self.config.clip_fade,
        );
        if (from..from.saturating_add(length)).contains(&position) {
            fade_gain(position - from, length)
        } else {
            self.playback.fade_in_from = None;
            1.0
        }
    }

    /// 跳转成功之后：跳转不淡入，结束这次装载的淡入，马上按新位置更新增益，不等下一个 tick。
    /// 跳回起点也一样，否则它还在刚装载时的那一段里，会从无声重新淡入。终点前的淡出照旧。
    pub(super) fn end_fade_in(&mut self) {
        if self.playback.fade_in_from.take().is_some() {
            self.tick_clip_fade();
        }
    }

    /// 刚装载、还没开始放时那一份增益：要淡入的是 0，不会先漏出一下原音量；
    /// 不淡入的是 1，不会被上一条淡出到 0 的增益压住。装载成功后、开始放之前设上
    /// （见 `load_track`）。曲库播放按 `fade_in_from` 看这次是不是从中间开始的。
    pub(super) fn starting_clip_gain(&self, origin: &Origin) -> f32 {
        match origin {
            Origin::Entry { range, .. } => {
                clip_gain(*range, None, range.start, self.config.clip_fade)
            }
            Origin::Library if self.playback.fade_in_from.is_some() => {
                fade_gain(Duration::ZERO, self.config.clip_fade / 3)
            }
            Origin::Library => 1.0,
        }
    }

    pub(super) fn set_sleep_gain(&mut self, gain: f32) {
        self.playback.sleep_gain = gain;
        self.apply_fade_gain();
    }

    pub(super) fn set_clip_gain(&mut self, gain: f32) {
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

/// 曲库播放在 `position` 处终点前的增益：在终点之前、离终点不到 `fade_out` 时淡出，
/// 淡出不超过 [起点（没有时是 0:00）, 终点] 的一半。没有终点、越过了终点时不淡；
/// 终点总在文件结尾之前，文件结尾本身不淡出。
fn end_point_gain(bounds: Bounds, position: Duration, fade_out: Duration) -> f32 {
    match bounds.end {
        Some(end) if position < end => {
            let half = end.saturating_sub(bounds.start.unwrap_or_default()) / 2;
            fade_gain(end - position, fade_out.min(half))
        }
        _ => 1.0,
    }
}

/// 曲库播放从 `from` 装载后淡入多久：淡出时长的三分之一，
/// 不超过 [开始的位置, 终点（没有或已在身后时是时长）] 的一半。
fn fade_in_length(
    bounds: Bounds,
    duration: Option<Duration>,
    from: Duration,
    fade_out: Duration,
) -> Duration {
    let half = bounds
        .end
        .filter(|end| from < *end)
        .or(duration)
        .map_or(Duration::MAX, |end| end.saturating_sub(from) / 2);
    (fade_out / 3).min(half)
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

    fn bounds(start: Option<u64>, end: Option<u64>) -> Bounds {
        Bounds {
            start: start.map(|start| SECOND * start as u32),
            end: end.map(|end| SECOND * end as u32),
        }
    }

    #[test]
    fn a_recording_fades_out_before_its_end_point_only() {
        let fade = SECOND * 3;
        let gain = |bounds: Bounds, position: f32| end_point_gain(bounds, at(position), fade);
        let both = bounds(Some(100), Some(200));
        assert_eq!(gain(both, 150.0), 1.0);
        assert_eq!(gain(both, 197.0), 1.0);
        assert_eq!(gain(both, 198.5), 0.25);
        assert!(gain(both, 199.99) < 0.001);
        // 越过了终点（一直放到文件结尾）、没有终点：不淡，文件结尾也不淡。
        assert_eq!(gain(both, 200.0), 1.0);
        assert_eq!(gain(both, 300.0), 1.0);
        assert_eq!(gain(bounds(Some(100), None), 599.0), 1.0);
        // 没有起点时一样淡出。
        assert_eq!(gain(bounds(None, Some(200)), 198.5), 0.25);
        // 关掉时不淡。
        assert_eq!(end_point_gain(both, at(199.0), Duration::ZERO), 1.0);
    }

    #[test]
    fn a_fade_in_after_loading_lasts_a_third_of_the_fade_out() {
        let length = Some(SECOND * 600);
        let fade = SECOND * 3;
        assert_eq!(
            fade_in_length(bounds(Some(100), Some(200)), length, at(100.0), fade),
            SECOND
        );
        // 记下的位置在起点之后、没有终点：一样淡入 1 秒。
        assert_eq!(
            fade_in_length(bounds(Some(100), None), length, at(300.0), fade),
            SECOND
        );
        assert_eq!(
            fade_in_length(bounds(None, None), None, at(300.0), fade),
            SECOND
        );
        assert_eq!(
            fade_in_length(bounds(None, None), length, at(300.0), Duration::ZERO),
            Duration::ZERO
        );
    }

    #[test]
    fn a_short_span_fades_over_at_most_half_of_it_each_way() {
        let length = Some(SECOND * 600);
        let fade = SECOND * 30;
        // 起点 100、终点 104：淡出不超过 2 秒。
        let short = bounds(Some(100), Some(104));
        assert_eq!(end_point_gain(short, at(103.0), fade), 0.25);
        assert_eq!(end_point_gain(short, at(102.0), fade), 1.0);
        // 从 102 开始放，离终点 2 秒：淡入不超过 1 秒。
        assert_eq!(fade_in_length(short, length, at(102.0), fade), SECOND);
        // 终点已在身后时按文件时长算。
        assert_eq!(fade_in_length(short, length, at(598.0), fade), SECOND);
    }
}
