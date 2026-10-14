//! 左右平衡与左右声道互换：包在解码出的音频外面，逐帧调整前两个声道。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate, Source};

/// 平衡的两端：-100 只剩左声道，100 只剩右声道。
pub const MAX_BALANCE: i8 = 100;

/// 左右平衡与声道互换的设置。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ChannelMix {
    /// -100 到 100，0 居中；正数偏右。
    pub balance: i8,
    /// 左右声道互换。平衡说的是耳朵这一侧，在互换之后才生效。
    pub swapped: bool,
}

impl ChannelMix {
    /// 左右两耳的增益。偏向一侧时只压低另一侧，偏到头时另一侧没有声音。
    pub fn gains(self) -> (f32, f32) {
        let balance = f32::from(self.balance.clamp(-MAX_BALANCE, MAX_BALANCE)) / 100.0;
        (1.0 - balance.max(0.0), 1.0 + balance.min(0.0))
    }
}

/// 播放器和正在播放的音频共用的设置：改动在下一帧生效，不用重新装载。
#[derive(Debug)]
pub(super) struct MixControl {
    left: AtomicU32,
    right: AtomicU32,
    swapped: AtomicBool,
}

impl MixControl {
    pub(super) fn new(mix: ChannelMix) -> Self {
        let control = Self {
            left: AtomicU32::new(0),
            right: AtomicU32::new(0),
            swapped: AtomicBool::new(false),
        };
        control.set(mix);
        control
    }

    pub(super) fn set(&self, mix: ChannelMix) {
        let (left, right) = mix.gains();
        self.left.store(left.to_bits(), Ordering::Relaxed);
        self.right.store(right.to_bits(), Ordering::Relaxed);
        self.swapped.store(mix.swapped, Ordering::Relaxed);
    }

    fn load(&self) -> (f32, f32, bool) {
        (
            f32::from_bits(self.left.load(Ordering::Relaxed)),
            f32::from_bits(self.right.load(Ordering::Relaxed)),
            self.swapped.load(Ordering::Relaxed),
        )
    }
}

/// 逐帧应用 [`MixControl`]。单声道先复制成左右两个声道，平衡和互换才有意义；
/// 混音器本来也会这样复制。超过两个声道时只动前两个（前左、前右）。
pub(super) struct Mixed<S> {
    inner: S,
    control: Arc<MixControl>,
    /// 正在输出的这一帧，以及下一个要输出的样本在帧里的位置。
    frame: Vec<f32>,
    next: usize,
}

impl<S: Source> Mixed<S> {
    pub(super) fn new(inner: S, control: Arc<MixControl>) -> Self {
        Self {
            inner,
            control,
            frame: Vec::with_capacity(8),
            next: 0,
        }
    }

    fn is_mono(&self) -> bool {
        self.inner.channels().get() == 1
    }

    /// 读入下一帧并调整；内层已经没有样本时返回 None。
    /// 结尾不满一帧时照样输出，只是不再分左右。
    fn fill(&mut self) -> Option<()> {
        let channels = self.inner.channels().get();
        self.frame.clear();
        self.next = 0;
        for _ in 0..channels {
            match self.inner.next() {
                Some(sample) => self.frame.push(sample),
                None => break,
            }
        }
        let first = *self.frame.first()?;
        if channels == 1 {
            self.frame.push(first);
        }
        if self.frame.len() >= 2 {
            let (left, right, swapped) = self.control.load();
            if swapped {
                self.frame.swap(0, 1);
            }
            self.frame[0] *= left;
            self.frame[1] *= right;
        }
        Some(())
    }
}

impl<S: Source> Iterator for Mixed<S> {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.next == self.frame.len() {
            self.fill()?;
        }
        let sample = self.frame[self.next];
        self.next += 1;
        Some(sample)
    }
}

impl<S: Source> Source for Mixed<S> {
    fn current_span_len(&self) -> Option<usize> {
        let len = self.inner.current_span_len()?;
        Some(if self.is_mono() { len * 2 } else { len })
    }

    fn is_exhausted(&self) -> bool {
        self.next == self.frame.len() && self.inner.is_exhausted()
    }

    fn channels(&self) -> ChannelCount {
        if self.is_mono() {
            ChannelCount::new(2).expect("two channels is non-zero")
        } else {
            self.inner.channels()
        }
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    /// 按 Rodio 的约定，跳转之后的下一个样本仍属于原本轮到的那个声道：
    /// 一帧只输出了一半时，新位置的那一帧也从同一个声道接着输出，左右不会错位。
    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        self.inner.try_seek(position)?;
        let phase = self.next % self.frame.len().max(1);
        self.frame.clear();
        self.next = 0;
        if phase > 0 && self.fill().is_some() {
            self.next = phase.min(self.frame.len());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use rodio::buffer::SamplesBuffer;

    use super::*;

    fn source(channels: u16, samples: &[f32]) -> SamplesBuffer {
        SamplesBuffer::new(
            ChannelCount::new(channels).unwrap(),
            SampleRate::new(8_000).unwrap(),
            samples.to_vec(),
        )
    }

    fn mixed(channels: u16, samples: &[f32], mix: ChannelMix) -> Mixed<SamplesBuffer> {
        Mixed::new(source(channels, samples), Arc::new(MixControl::new(mix)))
    }

    fn mix(balance: i8, swapped: bool) -> ChannelMix {
        ChannelMix { balance, swapped }
    }

    #[test]
    fn gains_only_lower_the_far_side() {
        assert_eq!(ChannelMix::default().gains(), (1.0, 1.0));
        assert_eq!(mix(50, false).gains(), (0.5, 1.0));
        assert_eq!(mix(-30, false).gains(), (1.0, 0.7));
        assert_eq!(mix(100, false).gains(), (0.0, 1.0));
        assert_eq!(mix(i8::MIN, false).gains(), (1.0, 0.0));
    }

    #[test]
    fn centre_passes_stereo_through_unchanged() {
        let samples = [0.1, -0.2, 0.3, -0.4];
        let output: Vec<f32> = mixed(2, &samples, ChannelMix::default()).collect();
        assert_eq!(output, samples);
    }

    #[test]
    fn balance_lowers_one_ear_and_swap_happens_first() {
        let samples = [0.8, 0.4, 0.8, 0.4];
        let output: Vec<f32> = mixed(2, &samples, mix(50, false)).collect();
        assert_eq!(output, [0.4, 0.4, 0.4, 0.4]);
        let output: Vec<f32> = mixed(2, &samples, mix(-100, false)).collect();
        assert_eq!(output, [0.8, 0.0, 0.8, 0.0]);
        let output: Vec<f32> = mixed(2, &samples, mix(0, true)).collect();
        assert_eq!(output, [0.4, 0.8, 0.4, 0.8]);
        // 平衡说的是耳朵：互换后左耳听到的是原来的右声道，再按左耳的增益压低。
        let output: Vec<f32> = mixed(2, &samples, mix(50, true)).collect();
        assert_eq!(output, [0.2, 0.8, 0.2, 0.8]);
    }

    #[test]
    fn mono_becomes_stereo_so_balance_still_works() {
        let source = mixed(1, &[0.5, 0.25], mix(50, false));
        assert_eq!(source.channels().get(), 2);
        assert_eq!(source.current_span_len(), Some(4));
        let output: Vec<f32> = source.collect();
        assert_eq!(output, [0.25, 0.5, 0.125, 0.25]);
    }

    #[test]
    fn channels_beyond_the_first_two_pass_through() {
        let samples = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0];
        let output: Vec<f32> = mixed(6, &samples, mix(50, true)).collect();
        assert_eq!(output, [1.0, 1.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn a_truncated_last_frame_is_still_played() {
        let output: Vec<f32> = mixed(2, &[0.8, 0.4, 0.6], mix(0, true)).collect();
        assert_eq!(output, [0.4, 0.8, 0.6]);
    }

    #[test]
    fn changes_apply_from_the_next_frame() {
        let control = Arc::new(MixControl::new(ChannelMix::default()));
        let mut source = Mixed::new(source(2, &[0.8, 0.4, 0.8, 0.4]), Arc::clone(&control));
        assert_eq!(source.next(), Some(0.8));
        control.set(mix(0, true));
        // 这一帧已经读进来了，右声道不受影响，免得一帧里左右错位。
        assert_eq!(source.next(), Some(0.4));
        assert_eq!(source.next(), Some(0.4));
        assert_eq!(source.next(), Some(0.8));
        assert!(source.is_exhausted());
    }

    #[test]
    fn a_seek_keeps_the_channel_order() {
        let samples: Vec<f32> = (0..16).map(|index| index as f32).collect();
        // 8000 Hz 的 0.5 ms 是第 4 帧，也就是第 8、9 个样本。
        let mut source = mixed(2, &samples, mix(0, true));
        source.try_seek(Duration::from_micros(500)).unwrap();
        let output: Vec<f32> = source.take(2).collect();
        assert_eq!(output, [9.0, 8.0]);

        // 左耳那一个已经输出，跳转后接着输出新一帧的右耳。
        let mut source = mixed(2, &samples, mix(0, true));
        assert_eq!(source.next(), Some(1.0));
        source.try_seek(Duration::from_micros(500)).unwrap();
        let output: Vec<f32> = source.take(3).collect();
        assert_eq!(output, [8.0, 11.0, 10.0]);
    }
}
