//! 睡眠定时：到点前把音量淡出，到点后暂停，位置保留，按空格就能接着听。
//!
//! 定时按墙上时间走：手动暂停不会让它停下，到点时已经暂停就只关掉定时。
//! 淡出通过后端的淡出增益实现，用户音量和配置都不受影响。

use std::time::{Duration, Instant};

use crate::player::PlayState;

use super::App;

/// 按 t 依次切换的定时长度（分钟），之后回到关闭。
const PRESETS: [u64; 5] = [15, 30, 60, 90, 120];
/// 到点前用多长时间把音量降到零。
const FADE: Duration = Duration::from_secs(30);

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SleepTimer {
    #[default]
    Off,
    /// 到 `deadline` 时暂停；`minutes` 是所选的预设，用来决定下一个预设。
    After { minutes: u64, deadline: Instant },
}

impl App {
    pub(super) fn cycle_sleep_timer(&mut self) {
        let next = match self.playback.sleep {
            SleepTimer::Off => Some(PRESETS[0]),
            SleepTimer::After { minutes, .. } => {
                PRESETS.iter().copied().find(|preset| *preset > minutes)
            }
        };
        self.playback.sleep = match next {
            Some(minutes) => SleepTimer::After {
                minutes,
                deadline: self.now + Duration::from_secs(minutes * 60),
            },
            None => SleepTimer::Off,
        };
        // 重新计时意味着还醒着，淡出到一半也恢复原来的音量。
        self.set_fade_gain(1.0);
        self.view.message = Some(match next {
            Some(minutes) => format!("睡眠定时：{minutes} 分钟后淡出并暂停（t 切换）"),
            None => "睡眠定时已关闭".to_owned(),
        });
    }

    /// 离定时到点还有多久，向上取整到秒：刚按下时显示整数分钟。没有定时时为 None。
    pub fn sleep_remaining(&self) -> Option<Duration> {
        let SleepTimer::After { deadline, .. } = self.playback.sleep else {
            return None;
        };
        let remaining = deadline.saturating_duration_since(self.now);
        Some(Duration::from_secs(
            remaining.as_millis().div_ceil(1000) as u64
        ))
    }

    /// 每个 tick 调整淡出增益；到点时暂停并恢复增益。
    pub(super) fn tick_sleep(&mut self) {
        let SleepTimer::After { deadline, .. } = self.playback.sleep else {
            return;
        };
        let remaining = deadline.saturating_duration_since(self.now);
        if !remaining.is_zero() {
            self.set_fade_gain(fade_gain(remaining, FADE));
            return;
        }
        let playing = self.playback.player.state() == PlayState::Playing;
        // 先暂停再恢复增益：Rodio 在同一次回调里读取暂停和音量，不会漏出一下原音量。
        self.playback.player.pause();
        self.playback.sleep = SleepTimer::Off;
        self.set_fade_gain(1.0);
        self.view.message = Some(
            if playing {
                "睡眠定时到，已暂停（按空格继续）"
            } else {
                "睡眠定时已到"
            }
            .to_owned(),
        );
    }

    fn set_fade_gain(&mut self, gain: f32) {
        if self.playback.fade_gain != gain {
            self.playback.fade_gain = gain;
            self.playback.player.set_fade_gain(gain);
        }
    }
}

/// 剩余时间进入淡出段后，增益按剩余比例的平方下降：线性降低音量时，
/// 听感上前半段几乎不变、最后几秒才突然变小，平方曲线听起来更均匀。
fn fade_gain(remaining: Duration, fade: Duration) -> f32 {
    if fade.is_zero() || remaining >= fade {
        return 1.0;
    }
    let fraction = remaining.as_secs_f32() / fade.as_secs_f32();
    fraction * fraction
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fade_starts_at_full_volume_and_ends_silent() {
        let fade = Duration::from_secs(30);
        assert_eq!(fade_gain(Duration::from_secs(60), fade), 1.0);
        assert_eq!(fade_gain(fade, fade), 1.0);
        assert_eq!(fade_gain(Duration::from_secs(15), fade), 0.25);
        assert_eq!(fade_gain(Duration::ZERO, fade), 0.0);
        assert_eq!(fade_gain(Duration::from_secs(1), Duration::ZERO), 1.0);
    }
}
