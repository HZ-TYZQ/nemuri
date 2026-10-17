//! 测试用的假后端：不解码、不出声。
//!
//! 文件存在、非空且以 `RIFF` 开头就当作能播放；位置只在跳转时改变；
//! 曲目播完（包括播到停止位置）、解码出错和输出中断都由测试通过 [`FakeControl`] 注入。
//! 输出中断后的恢复与 Rodio 后端一致：`resume` 发起，下一次取事件时完成。

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use super::{ChannelMix, PlayState, PlaybackBackend, PlayerEvent};

/// 测试端也要改动的状态，放在共享单元里。
struct FakeState {
    state: PlayState,
    position: Duration,
    stop_at: Option<Duration>,
    volume: u8,
    muted: bool,
    fade_gain: f32,
    /// 最近一次开始装载、最近一次从暂停开始播放时的淡出增益。
    gain_when_loading: Option<f32>,
    gain_when_resumed: Option<f32>,
    channel_mix: ChannelMix,
    output_down: bool,
    recovering: bool,
    events: Vec<PlayerEvent>,
}

pub(crate) struct FakeBackend {
    shared: Rc<RefCell<FakeState>>,
    current: Option<PathBuf>,
}

/// 测试一侧持有的句柄，用来查看和操纵装进 App 的假后端。
#[derive(Clone)]
pub(crate) struct FakeControl(Rc<RefCell<FakeState>>);

impl FakeBackend {
    pub(crate) fn new() -> (Self, FakeControl) {
        let state = Rc::new(RefCell::new(FakeState {
            state: PlayState::Stopped,
            position: Duration::ZERO,
            stop_at: None,
            volume: 100,
            muted: false,
            fade_gain: 1.0,
            gain_when_loading: None,
            gain_when_resumed: None,
            channel_mix: ChannelMix::default(),
            output_down: false,
            recovering: false,
            events: Vec::new(),
        }));
        let backend = Self {
            shared: Rc::clone(&state),
            current: None,
        };
        (backend, FakeControl(state))
    }

    fn load(&mut self, path: &Path, target: PlayState, position: Duration) -> Result<(), String> {
        let mut state = self.shared.borrow_mut();
        // 真的后端打开和解码新文件时，旧的还在出声。
        state.gain_when_loading = Some(state.fade_gain);
        if state.output_down {
            return Err("音频输出中断，请按空格重试".to_owned());
        }
        let bytes = fs::read(path).map_err(|_| format!("音频文件不存在: {}", path.display()))?;
        if !bytes.starts_with(b"RIFF") {
            return Err(format!("无法识别或解码 {}", path.display()));
        }
        state.state = target;
        state.position = position;
        state.stop_at = None;
        self.current = Some(path.to_path_buf());
        Ok(())
    }
}

impl FakeControl {
    /// 最近一次设置的淡出增益：睡眠定时和片段淡入淡出里较小的那个。
    pub(crate) fn fade_gain(&self) -> f32 {
        self.0.borrow().fade_gain
    }

    /// 最近一次设置的左右平衡与声道互换。
    /// 最近一次开始装载时的淡出增益：装载期间旧曲目听到的音量。
    pub(crate) fn gain_when_loading(&self) -> Option<f32> {
        self.0.borrow().gain_when_loading
    }

    /// 最近一次从暂停开始播放时的淡出增益：新曲目出声时的音量。
    pub(crate) fn gain_when_resumed(&self) -> Option<f32> {
        self.0.borrow().gain_when_resumed
    }

    pub(crate) fn channel_mix(&self) -> ChannelMix {
        self.0.borrow().channel_mix
    }

    /// App 最近一次设的停止位置；装载新曲目和停止时清掉。
    pub(crate) fn stop_at(&self) -> Option<Duration> {
        self.0.borrow().stop_at
    }

    /// 当前曲目自然播完。
    pub(crate) fn finish_track(&self) {
        let mut state = self.0.borrow_mut();
        state.state = PlayState::Stopped;
        state.events.push(PlayerEvent::EndOfStream);
    }

    /// 当前曲目播放途中解码出错。
    pub(crate) fn fail_track(&self, error: &str) {
        let mut state = self.0.borrow_mut();
        state.state = PlayState::Stopped;
        state.events.push(PlayerEvent::Error(error.to_owned()));
    }

    /// 音频输出设备中断：暂停并保留位置，直到恢复成功。
    pub(crate) fn break_output(&self, error: &str) {
        let mut state = self.0.borrow_mut();
        state.output_down = true;
        if state.state == PlayState::Playing {
            state.state = PlayState::Paused;
        }
        state
            .events
            .push(PlayerEvent::OutputError(error.to_owned()));
    }
}

impl PlaybackBackend for FakeBackend {
    fn state(&self) -> PlayState {
        self.shared.borrow().state
    }

    fn current_path(&self) -> Option<&Path> {
        self.current.as_deref()
    }

    fn play(&mut self, path: &Path) -> Result<(), String> {
        self.load(path, PlayState::Playing, Duration::ZERO)
    }

    fn open(&mut self, path: &Path) -> Result<(), String> {
        self.load(path, PlayState::Paused, Duration::ZERO)
    }

    fn open_at(&mut self, path: &Path, position: Duration) -> Result<(), String> {
        self.load(path, PlayState::Paused, position)
    }

    fn pause(&mut self) {
        let mut state = self.shared.borrow_mut();
        state.recovering = false;
        if state.state == PlayState::Playing {
            state.state = PlayState::Paused;
        }
    }

    fn resume(&mut self) {
        let mut state = self.shared.borrow_mut();
        if state.output_down {
            state.recovering = true;
        } else if state.state == PlayState::Paused {
            state.state = PlayState::Playing;
            state.gain_when_resumed = Some(state.fade_gain);
        }
    }

    fn stop(&mut self) {
        let mut state = self.shared.borrow_mut();
        state.recovering = false;
        state.state = PlayState::Stopped;
        state.position = Duration::ZERO;
        state.stop_at = None;
        self.current = None;
    }

    fn position(&self) -> Duration {
        self.shared.borrow().position
    }

    fn duration(&self) -> Option<Duration> {
        // 不解码就不知道时长；App 会退回扫描时记下的曲目时长。
        None
    }

    fn seek_to(&mut self, position: Duration) -> bool {
        let mut state = self.shared.borrow_mut();
        if state.state == PlayState::Stopped || state.output_down {
            return false;
        }
        state.position = position;
        true
    }

    fn set_stop_at(&mut self, position: Option<Duration>) {
        self.shared.borrow_mut().stop_at = position;
    }

    fn volume(&self) -> u8 {
        self.shared.borrow().volume
    }

    fn set_volume(&mut self, percent: u8) {
        self.shared.borrow_mut().volume = percent.min(100);
    }

    fn is_muted(&self) -> bool {
        self.shared.borrow().muted
    }

    fn set_muted(&mut self, muted: bool) {
        self.shared.borrow_mut().muted = muted;
    }

    fn set_fade_gain(&mut self, gain: f32) {
        self.shared.borrow_mut().fade_gain = gain;
    }

    fn set_channel_mix(&mut self, mix: ChannelMix) {
        self.shared.borrow_mut().channel_mix = mix;
    }

    fn set_spectrum_enabled(&mut self, _enabled: bool) {}

    fn output_unavailable(&self) -> bool {
        self.shared.borrow().output_down
    }

    fn drain_events(&mut self) -> Vec<PlayerEvent> {
        let mut state = self.shared.borrow_mut();
        let mut events = std::mem::take(&mut state.events);
        if state.recovering {
            state.recovering = false;
            state.output_down = false;
            state.state = PlayState::Playing;
            events.push(PlayerEvent::OutputRecovered);
        }
        events
    }
}
