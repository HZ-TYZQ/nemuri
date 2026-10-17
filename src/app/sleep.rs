//! 睡眠定时：到点前把音量淡出，到点后暂停，位置保留，按空格就能接着听；
//! 也可以等当前这一条播完再停。
//!
//! 定时按墙上时间走：手动暂停不会让它停下，到点时已经暂停就只关掉定时。
//! 淡出通过后端的淡出增益实现，用户音量和配置都不受影响。

use std::time::{Duration, Instant};

use crate::player::PlayState;

use super::App;
use super::fade::fade_gain;

/// 按 t 依次切换的定时长度（分钟），之后是“本条播完时”，再回到关闭。
/// 淡出时长来自配置（`sleep { fade 30 }`）。
const PRESETS: [u64; 5] = [15, 30, 60, 90, 120];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum SleepTimer {
    #[default]
    Off,
    /// 到 `deadline` 时暂停；`minutes` 是所选的预设，用来决定下一个预设。
    After { minutes: u64, deadline: Instant },
    /// 当前这一条播完时停止，不管循环方式和队列；最后一段淡出。
    /// 中途切歌时跟着新的一条走。
    EndOfTrack,
}

impl App {
    pub(super) fn cycle_sleep_timer(&mut self) {
        let after = |minutes: u64| SleepTimer::After {
            minutes,
            deadline: self.now + Duration::from_secs(minutes * 60),
        };
        self.playback.sleep = match self.playback.sleep {
            SleepTimer::Off => after(PRESETS[0]),
            SleepTimer::After { minutes, .. } => PRESETS
                .iter()
                .copied()
                .find(|preset| *preset > minutes)
                .map_or(SleepTimer::EndOfTrack, after),
            SleepTimer::EndOfTrack => SleepTimer::Off,
        };
        // 重新计时意味着还醒着，淡出到一半也恢复原来的音量。
        self.set_sleep_gain(1.0);
        let fade = self.config.sleep_fade.as_secs();
        self.view.message = Some(match self.playback.sleep {
            SleepTimer::Off => "睡眠定时已关闭".to_owned(),
            SleepTimer::After { minutes, .. } if fade == 0 => {
                format!("睡眠定时：{minutes} 分钟后暂停（t 切换）")
            }
            SleepTimer::After { minutes, .. } => {
                format!("睡眠定时：{minutes} 分钟后淡出并暂停（t 切换）")
            }
            SleepTimer::EndOfTrack if fade == 0 => "睡眠定时：本条播完时停止（t 切换）".to_owned(),
            SleepTimer::EndOfTrack => {
                format!("睡眠定时：本条播完时停止，最后 {fade} 秒淡出（t 切换）")
            }
        });
    }

    /// 定时是否设在当前这一条播完时。
    pub fn sleeps_at_track_end(&self) -> bool {
        self.playback.sleep == SleepTimer::EndOfTrack
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
        let deadline = match self.playback.sleep {
            SleepTimer::Off => return,
            SleepTimer::After { deadline, .. } => deadline,
            SleepTimer::EndOfTrack => {
                // 按这一条剩下的时长淡出；往回跳转时音量随之回升。有终点时在终点停，
                // 按离终点的时间算；跳过了终点就一直放到文件结尾，改按离结尾的时间算。
                // 播放列表的条目在条目的结尾停。不知道总时长就不淡出，播完照样停止。
                let gain = match self.span().end {
                    Some(item_end) if self.playback.player.state() != PlayState::Stopped => {
                        let position = self.playback.player.position();
                        let end = self
                            .current_bounds()
                            .end
                            .filter(|end| position < *end)
                            .unwrap_or(item_end);
                        fade_gain(end.saturating_sub(position), self.config.sleep_fade)
                    }
                    _ => 1.0,
                };
                self.set_sleep_gain(gain);
                return;
            }
        };
        let remaining = deadline.saturating_duration_since(self.now);
        if !remaining.is_zero() {
            self.set_sleep_gain(fade_gain(remaining, self.config.sleep_fade));
            return;
        }
        let playing = self.playback.player.state() == PlayState::Playing;
        // 先暂停再恢复增益：Rodio 在同一次回调里读取暂停和音量，不会漏出一下原音量。
        self.playback.player.pause();
        self.playback.sleep = SleepTimer::Off;
        self.set_sleep_gain(1.0);
        self.view.message = Some(
            if playing {
                "睡眠定时到，已暂停（按空格继续）"
            } else {
                "睡眠定时已到"
            }
            .to_owned(),
        );
    }

    /// “本条播完时”的定时在当前曲目自然播完时生效：停止播放，不接着放队列或下一首。
    pub(super) fn sleep_at_track_end(&mut self) {
        self.playback.player.stop();
        self.playback.playing_index = None;
        self.playback.sleep = SleepTimer::Off;
        self.set_sleep_gain(1.0);
        self.view.message = Some("睡眠定时：本条已播完，播放停止".to_owned());
    }
}
