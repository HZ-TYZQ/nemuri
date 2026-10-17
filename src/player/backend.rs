//! 基于 Rodio 0.22（内置 Symphonia 0.5.5）的播放后端。

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use lofty::file::FileType;
use lofty::probe::Probe;
use rodio::decoder::DecoderBuilder;
use rodio::mixer::mixer;
use rodio::{DeviceSinkBuilder, MixerDeviceSink, Source};

use super::channels::{ChannelMix, MixControl, Mixed};
use super::spectrum::{InternalSpectrumEvent, PcmBatch, PcmTap, SpectrumWorker};
use super::{PlayState, PlaybackBackend, PlayerEvent};

const EARLY_EOS_TOLERANCE: Duration = Duration::from_secs(1);
const SEEK_END_GUARD: Duration = Duration::from_millis(50);
const POSITION_TRUST_WINDOW: Duration = Duration::from_millis(100);
/// Rodio 的计时通常几毫秒内就赶上；过了这么久还对不上，就说明估计本身不对，以 Rodio 为准。
const PENDING_POSITION_LIMIT: Duration = Duration::from_secs(1);
const HEADLESS_CHANNELS: u16 = 2;
const HEADLESS_SAMPLE_RATE: u32 = 44_100;

/// cpal 的 AAudio 后端打开设备前要经 JNI 问 Java 的 AudioManager（缓冲区大小、设备列表），
/// 上下文取自 `ndk-context`；Termux 里运行的是普通可执行文件，没有 JVM，没人初始化它，
/// 一取就 panic。这里填一个空上下文：cpal 用空指针建 `JavaVM` 时得到错误，
/// 退回默认设备和未知的缓冲区大小，音频流照样由原生的 AAudio 打开。
#[cfg(target_os = "android")]
fn provide_empty_android_context() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    // SAFETY: 只调用一次；空指针只会交给 `JavaVM::from_raw`，它检查空指针并返回错误。
    ONCE.call_once(|| unsafe {
        ndk_context::initialize_android_context(std::ptr::null_mut(), std::ptr::null_mut());
    });
}

enum InternalEvent {
    SourceEnded {
        generation: u64,
        /// 解码 Source 自己耗尽时的已播位置。不能在事件到达后再读
        /// `Player::position()`：队列此时已切到后续静音源，`get_pos()` 常为 0。
        position: Duration,
        /// 停在了停止位置，而不是文件真的放完了。
        at_stop: bool,
    },
    DeviceError(rodio::cpal::StreamError),
    RecoveryError(String),
}

enum Output {
    /// 必须与 `backend` 一起存活，否则 CPAL 输出流会被关闭。
    Device(#[allow(dead_code)] MixerDeviceSink),
    Headless {
        shutdown: Arc<AtomicBool>,
        pump: Option<JoinHandle<()>>,
    },
}

pub struct Player {
    _output: Output,
    backend: rodio::Player,
    events: Receiver<InternalEvent>,
    event_tx: Sender<InternalEvent>,
    spectrum_events: Receiver<InternalSpectrumEvent>,
    state: PlayState,
    current_path: Option<PathBuf>,
    current_duration: Option<Duration>,
    generation: Arc<AtomicU64>,
    logical_volume: AtomicU8,
    muted: AtomicBool,
    /// 睡眠定时淡出时的增益，与用户音量相乘。
    fade_gain: f32,
    channel_mix: ChannelMix,
    /// 与正在播放的音频共用，改平衡或互换时不用重新装载。
    mix_control: Arc<MixControl>,
    spectrum_enabled: Arc<AtomicBool>,
    spectrum_live: Arc<AtomicBool>,
    is_playing: AtomicBool,
    pcm_slot: Arc<Mutex<Option<PcmBatch>>>,
    /// App 设的停止位置；输出中断恢复时交给新的播放器。
    stop_at: Option<Duration>,
    /// 与正在播放的这一首共用，改停止位置时不用重新装载；没有曲目时为 None。
    stop_point: Option<Arc<StopPoint>>,
    /// 装载、跳转或停止之后，Rodio 的计时还没赶上时用的估计值。
    pending_position: Mutex<Option<PendingPosition>>,
    // 恢复时先 seek 解码器，Rodio 的计时从接入时的 0 开始。
    // 后续正常 seek 会把 Rodio 的计时设为绝对位置，此时清除偏移。
    position_offset: Mutex<Duration>,
    _spectrum: SpectrumWorker,
    output_fault: Option<Duration>,
    recovery: Option<Receiver<Result<Box<Player>, String>>>,
    underrun_reported: bool,
}

impl Player {
    pub fn new() -> Result<Self, String> {
        #[cfg(target_os = "android")]
        provide_empty_android_context();
        let (event_tx, events) = mpsc::channel();
        let error_tx = event_tx.clone();
        let mut sink = DeviceSinkBuilder::from_default_device()
            .map_err(|error| format!("无法打开默认音频输出设备: {error}"))?
            .with_error_callback(move |error| {
                let _ = error_tx.send(InternalEvent::DeviceError(error));
            })
            .open_stream()
            .map_err(|error| format!("无法打开音频输出流: {error}"))?;
        sink.log_on_drop(false);
        let backend = rodio::Player::connect_new(sink.mixer());
        Ok(Self::assemble(
            Output::Device(sink),
            backend,
            event_tx,
            events,
        ))
    }

    /// 创建不依赖系统音频设备的播放器，供自动化测试使用。
    #[doc(hidden)]
    pub fn new_for_tests() -> Result<Self, String> {
        let (event_tx, events) = mpsc::channel();
        let (mix, mut output) = mixer(
            rodio::ChannelCount::new(HEADLESS_CHANNELS)
                .expect("headless channel count is non-zero"),
            rodio::SampleRate::new(HEADLESS_SAMPLE_RATE).expect("headless sample rate is non-zero"),
        );
        let backend = rodio::Player::connect_new(&mix);
        let shutdown = Arc::new(AtomicBool::new(false));
        let thread_shutdown = Arc::clone(&shutdown);
        let pump = thread::Builder::new()
            .name("rodio-test-pump".to_owned())
            .spawn(move || {
                let samples_per_ms =
                    u64::from(HEADLESS_SAMPLE_RATE) * u64::from(HEADLESS_CHANNELS) / 1_000;
                let mut consumed = 0_u64;
                while !thread_shutdown.load(Ordering::Relaxed) {
                    if output.next().is_none() {
                        break;
                    }
                    consumed += 1;
                    if consumed >= samples_per_ms {
                        consumed = 0;
                        thread::sleep(Duration::from_millis(1));
                    }
                }
            })
            .map_err(|error| format!("无法启动测试音频泵: {error}"))?;
        Ok(Self::assemble(
            Output::Headless {
                shutdown,
                pump: Some(pump),
            },
            backend,
            event_tx,
            events,
        ))
    }

    fn assemble(
        output: Output,
        backend: rodio::Player,
        event_tx: Sender<InternalEvent>,
        events: Receiver<InternalEvent>,
    ) -> Self {
        let generation = Arc::new(AtomicU64::new(0));
        let spectrum_enabled = Arc::new(AtomicBool::new(true));
        let spectrum_live = Arc::new(AtomicBool::new(false));
        let pcm_slot = Arc::new(Mutex::new(None));
        let (spectrum_tx, spectrum_events) = mpsc::channel();
        let spectrum =
            SpectrumWorker::spawn(spectrum_tx, Arc::clone(&generation), Arc::clone(&pcm_slot));

        Self {
            _output: output,
            backend,
            events,
            event_tx,
            spectrum_events,
            state: PlayState::Stopped,
            current_path: None,
            current_duration: None,
            generation,
            logical_volume: AtomicU8::new(100),
            muted: AtomicBool::new(false),
            fade_gain: 1.0,
            channel_mix: ChannelMix::default(),
            mix_control: Arc::new(MixControl::new(ChannelMix::default())),
            spectrum_enabled,
            spectrum_live,
            is_playing: AtomicBool::new(false),
            pcm_slot,
            stop_at: None,
            stop_point: None,
            pending_position: Mutex::new(None),
            position_offset: Mutex::new(Duration::ZERO),
            _spectrum: spectrum,
            output_fault: None,
            recovery: None,
            underrun_reported: false,
        }
    }

    fn load(&mut self, path: &Path, target: PlayState) -> Result<(), String> {
        self.load_at(path, target, Duration::ZERO)
    }

    fn load_at(
        &mut self,
        path: &Path,
        target: PlayState,
        position: Duration,
    ) -> Result<(), String> {
        if self.output_unavailable() {
            return Err("音频输出中断，请按空格重试".to_owned());
        }
        if !matches!(target, PlayState::Playing | PlayState::Paused) {
            return Err("内部错误: load 只能进入 Playing 或 Paused".to_owned());
        }
        if !path.is_file() {
            return Err(format!("音频文件不存在: {}", path.display()));
        }
        let absolute = path
            .canonicalize()
            .map_err(|error| format!("无法解析音频路径 {}: {error}", path.display()))?;
        reject_unsupported(&absolute)?;

        let file = File::open(&absolute)
            .map_err(|error| format!("无法打开 {}: {error}", absolute.display()))?;
        let byte_len = file
            .metadata()
            .map_err(|error| format!("无法读取 {} 的大小: {error}", absolute.display()))?
            .len();
        if byte_len == 0 {
            return Err(format!("空文件: {}", absolute.display()));
        }

        let mut builder = DecoderBuilder::new()
            .with_data(file)
            .with_byte_len(byte_len)
            .with_seekable(true);
        if let Some(extension) = absolute
            .extension()
            .and_then(|extension| extension.to_str())
        {
            builder = builder.with_hint(extension);
        }
        let mut decoder = builder
            .build()
            .map_err(|error| format!("无法识别或解码 {}: {error}", absolute.display()))?;
        let duration = decoder.total_duration();
        // 在接入音频线程之前恢复位置，失败时不出声，也不退回曲首。
        if !position.is_zero() {
            decoder
                .try_seek(position)
                .map_err(|error| format!("无法恢复播放位置: {error}"))?;
        }
        let channels = u64::from(decoder.channels().get());
        let rate = u64::from(decoder.sample_rate().get());
        let initial_samples = duration_to_samples(position, channels, rate);
        let stop_point = Arc::new(StopPoint::new(channels, rate));

        if target == PlayState::Paused {
            self.backend.pause();
        }
        let generation = self.advance_generation();
        self.backend.stop();
        if target == PlayState::Paused {
            self.backend.pause();
        }

        let tap = PcmTap::new(
            decoder,
            generation,
            Arc::clone(&self.pcm_slot),
            Arc::clone(&self.spectrum_live),
        );
        // 平衡和互换包在最外层：EndSignal 仍按解码出的声道数计算位置，
        // 频谱看到的也是调整前的声音。
        self.backend.append(Mixed::new(
            EndSignal {
                inner: tap,
                generation,
                events: self.event_tx.clone(),
                samples: initial_samples,
                stop: Arc::clone(&stop_point),
                reached_stop: false,
                signaled: false,
            },
            Arc::clone(&self.mix_control),
        ));

        self.apply_volume();
        if target == PlayState::Playing {
            self.backend.play();
        } else {
            self.backend.pause();
        }
        self.state = target;
        // 记录调用方交来的路径，而不是 canonicalize 之后的那个：规范化只是打开
        // 文件的手段。Windows 上它会带 \\?\ 前缀，Linux 上会展开软链接根目录，
        // 两种情况都会让上层按路径找不回正在播放的曲目。
        self.current_path = Some(path.to_path_buf());
        self.current_duration = duration;
        self.stop_at = None;
        self.stop_point = Some(stop_point);
        *self.position_offset.lock().unwrap() = position;
        self.set_pending_position(position);
        self.is_playing
            .store(target == PlayState::Playing, Ordering::Relaxed);
        self.refresh_spectrum_live();
        Ok(())
    }

    pub fn toggle_pause(&mut self) {
        match self.state {
            PlayState::Playing => self.pause(),
            PlayState::Paused => self.resume(),
            PlayState::Stopped => {}
        }
    }

    pub fn seek_relative(&mut self, offset_seconds: i64) {
        let _ = self.seek_relative_micros(offset_seconds.saturating_mul(1_000_000));
    }

    /// 相对当前进度偏移。正值前进，负值后退。成功 seek 返回 `Some(新位置)`。
    /// 调用方负责把越过曲尾的情况当成 Next；本方法只 saturate 到 0 并 clamp 末端。
    pub fn seek_relative_micros(&mut self, offset_micros: i64) -> Option<Duration> {
        if self.state == PlayState::Stopped {
            return None;
        }
        let current = self.position();
        let requested = if offset_micros.is_negative() {
            current.saturating_sub(Duration::from_micros(offset_micros.unsigned_abs()))
        } else {
            current.saturating_add(Duration::from_micros(offset_micros as u64))
        };
        self.seek_to(requested).then_some(self.position())
    }

    fn start_recovery(&mut self) {
        if self.recovery.is_some() {
            return;
        }
        let position = self.output_fault.unwrap_or_default();
        let path = self.current_path.clone();
        let (tx, rx) = mpsc::channel();
        // 新播放器拥有独立事件通道，旧输出流迟到的回调不会影响恢复后的播放。
        let result = thread::Builder::new()
            .name("audio-recovery".to_owned())
            .spawn(move || {
                let result = (|| {
                    let mut player = Self::new()?;
                    if let Some(path) = path {
                        player.load_at(&path, PlayState::Paused, position)?;
                    }
                    player.drain_events();
                    if player.output_unavailable() {
                        return Err("新音频输出流不可用".to_owned());
                    }
                    Ok(Box::new(player))
                })();
                let _ = tx.send(result);
            });
        match result {
            Ok(_) => self.recovery = Some(rx),
            Err(error) => {
                let _ = self
                    .event_tx
                    .send(InternalEvent::RecoveryError(error.to_string()));
            }
        }
    }

    fn advance_generation(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        self.clear_pcm();
        generation
    }

    fn mark_stopped(&mut self) {
        self.state = PlayState::Stopped;
        self.is_playing.store(false, Ordering::Relaxed);
        self.spectrum_live.store(false, Ordering::Relaxed);
    }

    fn apply_volume(&self) {
        self.backend.set_volume(output_volume(
            self.logical_volume.load(Ordering::Relaxed),
            self.muted.load(Ordering::Relaxed),
            self.fade_gain,
        ));
    }

    fn refresh_spectrum_live(&self) {
        let live = self.spectrum_enabled.load(Ordering::Relaxed)
            && self.is_playing.load(Ordering::Relaxed);
        self.spectrum_live.store(live, Ordering::Relaxed);
        if !live {
            self.clear_pcm();
        }
    }

    fn clear_pcm(&self) {
        if let Ok(mut slot) = self.pcm_slot.lock() {
            *slot = None;
        }
    }

    /// 在 `state` 已经改好之后调用：播放中的估计值随时间往前走。
    fn set_pending_position(&self, position: Duration) {
        *self.pending() = Some(PendingPosition::new(
            position,
            self.state == PlayState::Playing,
            Instant::now(),
        ));
    }

    fn pending(&self) -> std::sync::MutexGuard<'_, Option<PendingPosition>> {
        self.pending_position
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

impl PlaybackBackend for Player {
    fn state(&self) -> PlayState {
        self.state
    }

    fn current_path(&self) -> Option<&Path> {
        self.current_path.as_deref()
    }

    fn play(&mut self, path: &Path) -> Result<(), String> {
        self.load(path, PlayState::Playing)
    }

    /// 解码并装上曲目，保持暂停。Rodio 的 `append` 会解除 `stopped`，
    /// 必须在 append 之前把 Player 置为 paused，否则会直接出声。
    fn open(&mut self, path: &Path) -> Result<(), String> {
        self.load(path, PlayState::Paused)
    }

    /// 同 `open`，但从指定位置开始，用于恢复上次会话。
    fn open_at(&mut self, path: &Path, position: Duration) -> Result<(), String> {
        self.load_at(path, PlayState::Paused, position)
    }

    fn pause(&mut self) {
        // 恢复期间收到 Pause，取消本次恢复；后台创建的播放器始终保持暂停。
        self.recovery = None;
        if self.state == PlayState::Playing {
            self.backend.pause();
            if let Some(pending) = self.pending().as_mut() {
                pending.pause(Instant::now());
            }
            self.state = PlayState::Paused;
            self.is_playing.store(false, Ordering::Relaxed);
            self.refresh_spectrum_live();
        }
    }

    fn resume(&mut self) {
        if self.output_unavailable() {
            self.start_recovery();
            return;
        }
        if self.state == PlayState::Paused {
            self.backend.play();
            if let Some(pending) = self.pending().as_mut() {
                pending.resume(Instant::now());
            }
            self.state = PlayState::Playing;
            self.is_playing.store(true, Ordering::Relaxed);
            self.refresh_spectrum_live();
        }
    }

    fn stop(&mut self) {
        self.recovery = None;
        if self.output_fault.is_some() {
            self.output_fault = Some(Duration::ZERO);
        }
        let _ = self.advance_generation();
        self.backend.stop();
        self.state = PlayState::Stopped;
        self.current_path = None;
        self.current_duration = None;
        self.stop_at = None;
        self.stop_point = None;
        *self.position_offset.lock().unwrap() = Duration::ZERO;
        self.set_pending_position(Duration::ZERO);
        self.is_playing.store(false, Ordering::Relaxed);
        self.refresh_spectrum_live();
    }

    fn position(&self) -> Duration {
        if let Some(position) = self.output_fault {
            return position;
        }
        let backend = self.backend.get_pos() + *self.position_offset.lock().unwrap();
        let now = Instant::now();
        let mut pending = self.pending();
        match *pending {
            Some(estimate) if !estimate.settled_by(backend, now) => estimate.expected(now),
            _ => {
                *pending = None;
                backend
            }
        }
    }

    fn duration(&self) -> Option<Duration> {
        self.current_duration
    }

    fn seek_to(&mut self, pos: Duration) -> bool {
        if self.state == PlayState::Stopped || self.output_unavailable() {
            return false;
        }
        let target = clamp_seek_target(pos, self.current_duration);
        if self.backend.try_seek(target).is_ok() {
            *self.position_offset.lock().unwrap() = Duration::ZERO;
            self.set_pending_position(target);
            true
        } else {
            false
        }
    }

    fn set_stop_at(&mut self, position: Option<Duration>) {
        self.stop_at = position;
        if let Some(stop_point) = &self.stop_point {
            stop_point.set(position);
        }
    }

    fn volume(&self) -> u8 {
        self.logical_volume.load(Ordering::Relaxed)
    }

    fn set_volume(&mut self, percent: u8) {
        self.logical_volume
            .store(percent.min(100), Ordering::Relaxed);
        self.apply_volume();
    }

    fn is_muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    fn set_muted(&mut self, muted: bool) {
        self.muted.store(muted, Ordering::Relaxed);
        self.apply_volume();
    }

    fn set_fade_gain(&mut self, gain: f32) {
        self.fade_gain = gain;
        self.apply_volume();
    }

    fn set_channel_mix(&mut self, mix: ChannelMix) {
        self.channel_mix = mix;
        self.mix_control.set(mix);
    }

    fn set_spectrum_enabled(&mut self, enabled: bool) {
        self.spectrum_enabled.store(enabled, Ordering::Relaxed);
        self.refresh_spectrum_live();
    }

    fn output_unavailable(&self) -> bool {
        self.output_fault.is_some()
    }

    fn drain_events(&mut self) -> Vec<PlayerEvent> {
        if let Some(recovery) = &self.recovery {
            match recovery.try_recv() {
                Ok(Ok(mut player)) => {
                    player.set_volume(self.volume());
                    player.set_muted(self.is_muted());
                    player.set_fade_gain(self.fade_gain);
                    player.set_channel_mix(self.channel_mix);
                    player.set_stop_at(self.stop_at);
                    player.set_spectrum_enabled(self.spectrum_enabled.load(Ordering::Relaxed));
                    player.resume();
                    *self = *player;
                    return vec![PlayerEvent::OutputRecovered];
                }
                Ok(Err(error)) => {
                    self.recovery = None;
                    return vec![PlayerEvent::OutputError(format!("恢复失败: {error}"))];
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.recovery = None;
                    return vec![PlayerEvent::OutputError("恢复音频输出失败".to_owned())];
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        let events: Vec<_> = self.events.try_iter().collect();
        // 同一批事件中设备故障优先，避免先处理曲尾而意外推进队列。
        if !self.output_unavailable()
            && let Some(message) = events.iter().find_map(|event| match event {
                InternalEvent::DeviceError(rodio::cpal::StreamError::BufferUnderrun) => None,
                InternalEvent::DeviceError(error) => Some(error.to_string()),
                _ => None,
            })
        {
            let position = self.position();
            self.backend.pause();
            self.output_fault = Some(position);
            self.state = PlayState::Paused;
            self.is_playing.store(false, Ordering::Relaxed);
            self.refresh_spectrum_live();
            self.advance_generation();
            return vec![PlayerEvent::OutputError(message)];
        }
        let current = self.generation.load(Ordering::Relaxed);
        let mut drained = Vec::new();
        for event in events {
            match event {
                InternalEvent::SourceEnded {
                    generation,
                    position,
                    at_stop,
                } => {
                    if generation != current || self.output_unavailable() {
                        continue;
                    }
                    let kind = if at_stop {
                        SourceEndKind::EndOfStream
                    } else {
                        classify_source_end(position, self.current_duration)
                    };
                    self.mark_stopped();
                    drained.push(match kind {
                        SourceEndKind::EndOfStream => PlayerEvent::EndOfStream,
                        SourceEndKind::Error => {
                            PlayerEvent::Error("音频可能损坏或解码提前终止".to_owned())
                        }
                    });
                }
                InternalEvent::DeviceError(rodio::cpal::StreamError::BufferUnderrun)
                    if !self.output_unavailable() && !self.underrun_reported =>
                {
                    self.underrun_reported = true;
                    drained.push(PlayerEvent::OutputWarning(
                        "音频缓冲不足，可能短暂卡顿".to_owned(),
                    ));
                }
                InternalEvent::DeviceError(_) => {}
                InternalEvent::RecoveryError(error) => {
                    drained.push(PlayerEvent::OutputError(error))
                }
            }
        }
        while let Ok(frame) = self.spectrum_events.try_recv() {
            if frame.generation != current || !self.spectrum_enabled.load(Ordering::Relaxed) {
                continue;
            }
            drained.push(frame.into_player_event());
        }
        drained
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        let _ = self.advance_generation();
        self.is_playing.store(false, Ordering::Relaxed);
        self.spectrum_live.store(false, Ordering::Relaxed);
        self.backend.stop();
        if let Output::Headless { shutdown, pump } = &mut self._output {
            shutdown.store(true, Ordering::Relaxed);
            if let Some(handle) = pump.take() {
                let _ = handle.join();
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceEndKind {
    EndOfStream,
    Error,
}

/// 停止位置，换算成这一首的采样数后与播放线程共用；没有时为 [`NO_STOP`]。
/// 换算放在改动时做，播放线程每个采样只多比较一次。
struct StopPoint {
    samples: AtomicU64,
    channels: u64,
    rate: u64,
}

const NO_STOP: u64 = u64::MAX;

impl StopPoint {
    fn new(channels: u64, rate: u64) -> Self {
        Self {
            samples: AtomicU64::new(NO_STOP),
            channels,
            rate,
        }
    }

    fn set(&self, position: Option<Duration>) {
        let samples = position.map_or(NO_STOP, |position| {
            duration_to_samples(position, self.channels, self.rate)
        });
        self.samples.store(samples, Ordering::Relaxed);
    }

    fn samples(&self) -> u64 {
        self.samples.load(Ordering::Relaxed)
    }
}

/// 解码 Source 耗尽或播到停止位置时发出结束事件，并带上按采样计数的播放位置。
struct EndSignal<S> {
    inner: S,
    generation: u64,
    events: Sender<InternalEvent>,
    samples: u64,
    stop: Arc<StopPoint>,
    /// 一路播到了停止位置，下一个采样起就当作耗尽；跳转会把它清掉。
    reached_stop: bool,
    signaled: bool,
}

impl<S: Source<Item = f32>> EndSignal<S> {
    fn elapsed(&self) -> Duration {
        let channels = u64::from(self.inner.channels().get());
        let rate = u64::from(self.inner.sample_rate().get());
        if channels == 0 || rate == 0 {
            return Duration::ZERO;
        }
        let frames = self.samples / channels;
        Duration::from_nanos(frames.saturating_mul(1_000_000_000) / rate)
    }
}

impl<S: Source<Item = f32>> Iterator for EndSignal<S> {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        if !self.reached_stop
            && let Some(sample) = self.inner.next()
        {
            self.samples += 1;
            // 计数一个一个往上走才会正好等于停止位置；跳转直接把计数设到它或它之后时不会，
            // 于是越过停止位置的跳转一直放到文件结尾。
            self.reached_stop = self.samples == self.stop.samples();
            return Some(sample);
        }
        if !self.signaled {
            self.signaled = true;
            let _ = self.events.send(InternalEvent::SourceEnded {
                generation: self.generation,
                position: self.elapsed(),
                at_stop: self.reached_stop,
            });
        }
        None
    }
}

impl<S: Source<Item = f32>> Source for EndSignal<S> {
    fn current_span_len(&self) -> Option<usize> {
        if self.reached_stop {
            return Some(0);
        }
        self.inner.current_span_len()
    }

    fn is_exhausted(&self) -> bool {
        self.reached_stop || self.inner.is_exhausted()
    }

    fn channels(&self) -> rodio::ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), rodio::source::SeekError> {
        self.inner.try_seek(pos)?;
        let channels = u64::from(self.inner.channels().get());
        let rate = u64::from(self.inner.sample_rate().get());
        self.samples = duration_to_samples(pos, channels, rate);
        self.reached_stop = false;
        Ok(())
    }
}

/// 装载、跳转或停止之后，Rodio 的计时可能还停在旧值上（新曲目还没被音频线程接上，
/// 或者跳转没有真正执行）。这段时间里按设定的位置和此后实际播放的时长估计当前位置。
///
/// 只拿 Rodio 的读数和设定的位置比，播放中头一次读位置晚于信任窗口时两者就永远对不上，
/// 位置会一直停在设定值上；估计值随播放往前走，才不会这样。
#[derive(Debug, Clone, Copy)]
struct PendingPosition {
    /// 设定的位置，加上之前几段播放走过的时间。
    position: Duration,
    /// 播放中时这一段开始的时刻；暂停时为 None。
    playing_since: Option<Instant>,
    set_at: Instant,
}

impl PendingPosition {
    fn new(position: Duration, playing: bool, now: Instant) -> Self {
        Self {
            position,
            playing_since: playing.then_some(now),
            set_at: now,
        }
    }

    fn expected(&self, now: Instant) -> Duration {
        self.position
            + self
                .playing_since
                .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }

    fn pause(&mut self, now: Instant) {
        self.position = self.expected(now);
        self.playing_since = None;
    }

    fn resume(&mut self, now: Instant) {
        if self.playing_since.is_none() {
            self.playing_since = Some(now);
        }
    }

    /// Rodio 的读数和估计值相差在信任窗口内，或者已经等得够久，就改用 Rodio 的读数。
    fn settled_by(&self, backend: Duration, now: Instant) -> bool {
        backend.abs_diff(self.expected(now)) <= POSITION_TRUST_WINDOW
            || now.saturating_duration_since(self.set_at) >= PENDING_POSITION_LIMIT
    }
}

/// 位置换算成采样数，落在整帧上：停止位置因此总在一帧结束处，不会只放半帧。
fn duration_to_samples(position: Duration, channels: u64, rate: u64) -> u64 {
    if channels == 0 || rate == 0 {
        return 0;
    }
    let frames = position.as_nanos().saturating_mul(rate as u128) / 1_000_000_000;
    u64::try_from(frames.saturating_mul(channels as u128)).unwrap_or(u64::MAX)
}

fn classify_source_end(position: Duration, duration: Option<Duration>) -> SourceEndKind {
    match duration {
        Some(duration) if position + EARLY_EOS_TOLERANCE < duration => SourceEndKind::Error,
        _ => SourceEndKind::EndOfStream,
    }
}

/// 交给 Rodio 的输出音量：用户音量（0–100）乘以淡出增益，静音时为 0。
/// 增益夹在 0–1 之间；不是有限数时当作 1，宁可不淡出也不要突然变响或无声。
fn output_volume(volume: u8, muted: bool, fade_gain: f32) -> f32 {
    if muted {
        return 0.0;
    }
    let gain = if fade_gain.is_finite() {
        fade_gain.clamp(0.0, 1.0)
    } else {
        1.0
    };
    f32::from(volume.min(100)) / 100.0 * gain
}

fn clamp_seek_target(target: Duration, duration: Option<Duration>) -> Duration {
    match duration {
        Some(duration) if duration <= SEEK_END_GUARD => Duration::ZERO,
        Some(duration) => target.min(duration.saturating_sub(SEEK_END_GUARD)),
        None => target,
    }
}

fn reject_unsupported(path: &Path) -> Result<(), String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_lowercase());
    match extension.as_deref() {
        Some("ape") => {
            return Err(format!("不支持 APE 格式: {}", path.display()));
        }
        Some("wma") => {
            return Err(format!("不支持 WMA 格式: {}", path.display()));
        }
        Some("opus") => {
            return Err(format!("暂不支持 Opus: {}", path.display()));
        }
        _ => {}
    }

    if let Ok(probe) = Probe::open(path)
        && let Ok(probe) = probe.guess_file_type()
    {
        match probe.file_type() {
            Some(FileType::Ape) => {
                return Err(format!("不支持 APE 格式: {}", path.display()));
            }
            Some(FileType::Opus) => {
                return Err(format!("暂不支持 Opus: {}", path.display()));
            }
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    #[test]
    fn the_pending_position_moves_on_while_playing_and_waits_while_paused() {
        let start = Instant::now();
        let mut pending = PendingPosition::new(ms(10_000), true, start);
        assert_eq!(pending.expected(start + ms(300)), ms(10_300));
        pending.pause(start + ms(300));
        assert_eq!(pending.expected(start + ms(900)), ms(10_300));
        pending.resume(start + ms(900));
        assert_eq!(pending.expected(start + ms(950)), ms(10_350));

        let paused = PendingPosition::new(ms(5_000), false, start);
        assert_eq!(paused.expected(start + ms(500)), ms(5_000));
    }

    #[test]
    fn rodio_is_trusted_once_it_agrees_even_when_the_first_read_is_late() {
        let start = Instant::now();
        let pending = PendingPosition::new(ms(10_000), true, start);
        // 头一次读位置时已经播了 300ms，Rodio 报 10.3s：和估计一致，改用 Rodio。
        // 以前只和 10s 比，这里会对不上，位置从此停在 10s。
        assert!(pending.settled_by(ms(10_290), start + ms(300)));
        // Rodio 还停在上一首的位置：先用估计值。
        assert!(!pending.settled_by(ms(95_000), start + ms(20)));
        // 等了一秒还对不上，以 Rodio 为准。
        assert!(pending.settled_by(ms(95_000), start + ms(1_000)));
    }

    #[test]
    fn early_eos_is_classified_as_error() {
        assert_eq!(
            classify_source_end(Duration::from_secs(5), Some(Duration::from_secs(10))),
            SourceEndKind::Error
        );
        assert_eq!(
            classify_source_end(Duration::from_secs(9), Some(Duration::from_secs(10))),
            SourceEndKind::EndOfStream
        );
        assert_eq!(
            classify_source_end(Duration::from_secs(3), None),
            SourceEndKind::EndOfStream
        );
    }

    #[test]
    fn fade_gain_scales_the_user_volume_and_never_amplifies() {
        assert_eq!(output_volume(80, false, 1.0), 0.8);
        assert_eq!(output_volume(80, false, 0.5), 0.4);
        assert_eq!(output_volume(80, true, 1.0), 0.0);
        assert_eq!(output_volume(80, false, 2.0), 0.8);
        assert_eq!(output_volume(80, false, -1.0), 0.0);
        assert_eq!(output_volume(80, false, f32::NAN), 0.8);
    }

    #[test]
    fn positions_become_whole_frames_of_samples() {
        // 1.5 秒、44.1 kHz 立体声：66150 帧。
        assert_eq!(duration_to_samples(ms(1_500), 2, 44_100), 132_300);
        // 落在两帧之间时取前一帧的开头，不会停在半帧上。
        let between = Duration::from_nanos(1_000_000_000 / 44_100 * 3 + 7_000);
        assert_eq!(duration_to_samples(between, 2, 44_100), 6);
        assert_eq!(duration_to_samples(ms(1_500), 0, 44_100), 0);
    }

    #[test]
    fn seek_never_lands_on_duration() {
        assert_eq!(
            clamp_seek_target(Duration::from_secs(10), Some(Duration::from_secs(10))),
            Duration::from_secs(10) - SEEK_END_GUARD
        );
        assert_eq!(
            clamp_seek_target(Duration::from_millis(10), Some(Duration::from_millis(40))),
            Duration::ZERO
        );
        assert_eq!(
            clamp_seek_target(Duration::from_secs(2), None),
            Duration::from_secs(2)
        );
    }
}
