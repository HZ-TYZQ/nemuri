//! 播放后端公共接口。

use std::path::Path;
use std::time::Duration;

pub const SPECTRUM_BANDS: usize = 512;
pub const SPECTRUM_THRESHOLD_DB: f32 = -72.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    Playing,
    Paused,
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    EndOfStream,
    Error(String),
    OutputError(String),
    OutputWarning(String),
    OutputRecovered,
    StateChanged(PlayState),
    SpectrumFrame {
        magnitudes: Vec<f32>,
        sample_rate: u32,
    },
}

/// App 对播放后端的全部要求：装载单首曲目、控制播放、报告状态与事件。
///
/// 队列、历史、随机和循环都由 App 负责，后端只管当前这一首。
/// 正式实现是基于 Rodio 的 [`Player`]；App 的测试使用不出声的假后端。
pub trait PlaybackBackend {
    fn state(&self) -> PlayState;
    fn current_path(&self) -> Option<&Path>;
    /// 装载并立即播放；失败时之前的曲目保持不变。
    fn play(&mut self, path: &Path) -> Result<(), String>;
    /// 装载但保持暂停。
    fn open(&mut self, path: &Path) -> Result<(), String>;
    /// 同 `open`，但从指定位置开始，用于恢复上次会话。
    fn open_at(&mut self, path: &Path, position: Duration) -> Result<(), String>;
    fn pause(&mut self);
    /// 输出中断时改为在后台重新打开输出设备，结果以事件报告。
    fn resume(&mut self);
    fn stop(&mut self);
    fn position(&self) -> Duration;
    /// 解码器报告的总时长；不知道时为 None。
    fn duration(&self) -> Option<Duration>;
    /// 停止状态或输出中断时返回 false。
    fn seek_to(&mut self, position: Duration) -> bool;
    /// 停止位置，用文件时间：正常播放到这里就当作这一首播完，和自然播完一样报告
    /// [`PlayerEvent::EndOfStream`]。只在一路播到它时生效：跳转落在它上面或越过它，
    /// 就一直放到文件结尾；再跳回它之前，又会在这里停。装载新曲目和停止时清掉。
    fn set_stop_at(&mut self, position: Option<Duration>);
    fn volume(&self) -> u8;
    fn set_volume(&mut self, percent: u8);
    fn is_muted(&self) -> bool;
    fn set_muted(&mut self, muted: bool);
    /// 淡出用的增益（0.0–1.0），与用户音量相乘后输出。它不改变 `volume()`，
    /// 因此不会被写回配置、报告给系统媒体控制或显示在界面上。
    fn set_fade_gain(&mut self, gain: f32);
    /// 左右平衡与声道互换，从正在播放的这一帧之后生效。
    fn set_channel_mix(&mut self, mix: ChannelMix);
    fn set_spectrum_enabled(&mut self, enabled: bool);
    /// 音频输出中断、等待恢复时为 true。
    fn output_unavailable(&self) -> bool;
    fn drain_events(&mut self) -> Vec<PlayerEvent>;
}

mod backend;
mod channels;
#[cfg(test)]
pub(crate) mod fake;
mod spectrum;

pub use self::backend::Player;
pub use self::channels::{ChannelMix, MAX_BALANCE};
