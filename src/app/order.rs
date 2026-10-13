//! 曲库内的播放顺序：顺序、列表循环、单曲循环和随机袋。
//!
//! 这里只和曲库下标打交道，不碰播放器、队列和提示，因此能脱离音频单独测试。
//! 播放范围、当前曲目和播放模式都由调用方传入；随机数源在构造时注入。

use crate::track::{PlaybackMode, RepeatMode};

/// 随机袋：一轮里每首歌恰好出现一次，一轮放完再洗下一轮。
#[derive(Debug)]
pub(crate) struct PlaybackOrder {
    bag: Vec<usize>,
    /// 下一首在袋中的位置。
    cursor: usize,
    rng_state: u64,
}

impl PlaybackOrder {
    pub(crate) fn new(seed: u64) -> Self {
        Self {
            bag: Vec::new(),
            cursor: 0,
            // xorshift 的状态为 0 时会一直停在 0。
            rng_state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// `current` 之后该播哪一首。`scope` 是可以播的曲库下标，按列表顺序升序排列，
    /// 文件夹筛选时只含该文件夹，否则是整个曲库。`current` 不在范围内时
    /// （例如正在播放的曲目不属于筛选的文件夹）从范围开头接着播。
    /// `natural_end` 表示上一首是自然播完的，只有这时单曲循环才重播同一首；
    /// 播放出错时单曲循环也照常往后走。
    pub(crate) fn next(
        &mut self,
        scope: &[usize],
        current: Option<usize>,
        natural_end: bool,
        mode: PlaybackMode,
    ) -> Option<usize> {
        if natural_end && mode.repeat == RepeatMode::One {
            return current;
        }
        if mode.shuffle {
            return self.next_from_bag(scope, current, mode.repeat);
        }
        let position = Self::position(scope, current);
        match (mode.repeat, position) {
            (_, None) => scope.first().copied(),
            (RepeatMode::All | RepeatMode::One, Some(position)) => {
                Some(scope[(position + 1) % scope.len()])
            }
            (RepeatMode::None, Some(position)) => scope.get(position + 1).copied(),
        }
    }

    /// 不动随机袋地回答还有没有下一首，供系统媒体控制显示。
    pub(crate) fn has_next(
        &self,
        scope: &[usize],
        current: Option<usize>,
        mode: PlaybackMode,
    ) -> bool {
        if scope.is_empty() {
            return false;
        }
        if mode.repeat != RepeatMode::None {
            return true;
        }
        if mode.shuffle {
            // 袋子还没建好时，第一次取下一首会以当前曲目为首重新洗一轮。
            return self.bag.is_empty() || self.cursor < self.bag.len();
        }
        Self::position(scope, current).is_none_or(|position| position + 1 < scope.len())
    }

    /// 在 `scope` 内重新洗一轮。`current` 在范围内时作为本轮第一首、视为已播，
    /// 其余曲目随机排在后面；不在范围内时整个范围都还没播。
    pub(crate) fn reanchor(&mut self, scope: &[usize], current: Option<usize>) {
        let anchor = current.filter(|current| Self::position(scope, Some(*current)).is_some());
        let mut rest: Vec<usize> = scope
            .iter()
            .copied()
            .filter(|index| Some(*index) != anchor)
            .collect();
        self.shuffle(&mut rest);
        self.bag = anchor.into_iter().chain(rest).collect();
        self.cursor = usize::from(anchor.is_some());
    }

    pub(crate) fn clear(&mut self) {
        self.bag.clear();
        self.cursor = 0;
    }

    #[cfg(test)]
    pub(crate) fn bag(&self) -> &[usize] {
        &self.bag
    }

    #[cfg(test)]
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// `current` 在范围里的位置；范围按下标升序排列，可以二分查找。
    fn position(scope: &[usize], current: Option<usize>) -> Option<usize> {
        scope.binary_search(&current?).ok()
    }

    fn next_from_bag(
        &mut self,
        scope: &[usize],
        current: Option<usize>,
        repeat: RepeatMode,
    ) -> Option<usize> {
        if scope.is_empty() {
            return None;
        }
        if self.bag.is_empty() {
            self.reanchor(scope, current);
        }
        if self.cursor >= self.bag.len() {
            match repeat {
                RepeatMode::None => return None,
                RepeatMode::All | RepeatMode::One => {
                    let avoid = self.bag.last().copied().or(current);
                    self.reshuffle_round(scope, avoid);
                }
            }
        }
        let next = self.bag.get(self.cursor).copied()?;
        self.cursor += 1;
        Some(next)
    }

    /// 新一轮整体洗牌；第一首避开上一轮的最后一首，接缝处不会连播同一首。
    fn reshuffle_round(&mut self, scope: &[usize], avoid_first: Option<usize>) {
        let mut order = scope.to_vec();
        self.shuffle(&mut order);
        if order.len() > 1 && Some(order[0]) == avoid_first {
            order.swap(0, 1);
        }
        self.bag = order;
        self.cursor = 0;
    }

    fn shuffle(&mut self, items: &mut [usize]) {
        if items.len() < 2 {
            return;
        }
        for index in (1..items.len()).rev() {
            let other = self.random_index(index + 1);
            items.swap(index, other);
        }
    }

    fn random_index(&mut self, upper: usize) -> usize {
        self.rng_state ^= self.rng_state << 13;
        self.rng_state ^= self.rng_state >> 7;
        self.rng_state ^= self.rng_state << 17;
        (self.rng_state as usize) % upper
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(repeat: RepeatMode, shuffle: bool) -> PlaybackMode {
        PlaybackMode { repeat, shuffle }
    }

    #[test]
    fn sequential_modes_walk_the_library_in_order() {
        let mut order = PlaybackOrder::new(1);
        let all_three = [0, 1, 2];
        let none = mode(RepeatMode::None, false);
        assert_eq!(order.next(&all_three, None, false, none), Some(0));
        assert_eq!(order.next(&all_three, Some(1), false, none), Some(2));
        assert_eq!(order.next(&all_three, Some(2), false, none), None);
        assert!(!order.has_next(&all_three, Some(2), none));

        let all = mode(RepeatMode::All, false);
        assert_eq!(order.next(&all_three, Some(2), false, all), Some(0));
        assert_eq!(order.next(&all_three, None, false, all), Some(0));

        // 单曲循环只在自然播完时重播；出错时照常往后走。
        let one = mode(RepeatMode::One, false);
        assert_eq!(order.next(&all_three, Some(1), true, one), Some(1));
        assert_eq!(order.next(&all_three, Some(1), false, one), Some(2));
        assert_eq!(order.next(&[], None, false, all), None);
    }

    #[test]
    fn sequential_order_stays_inside_the_scope() {
        let mut order = PlaybackOrder::new(1);
        let folder = [2, 3, 5];
        let none = mode(RepeatMode::None, false);
        assert_eq!(order.next(&folder, Some(3), false, none), Some(5));
        assert_eq!(order.next(&folder, Some(5), false, none), None);
        assert!(!order.has_next(&folder, Some(5), none));
        // 正在播放的曲目不在范围里时，从范围开头接着播。
        assert_eq!(order.next(&folder, Some(0), false, none), Some(2));
        assert!(order.has_next(&folder, Some(0), none));

        let all = mode(RepeatMode::All, false);
        assert_eq!(order.next(&folder, Some(5), false, all), Some(2));
        assert_eq!(order.next(&folder, Some(4), false, all), Some(2));
        // 单曲循环自然播完时重播当前曲，即使它不在范围里。
        let one = mode(RepeatMode::One, false);
        assert_eq!(order.next(&folder, Some(0), true, one), Some(0));
    }

    #[test]
    fn a_shuffle_round_plays_every_track_once_starting_after_the_anchor() {
        let mut order = PlaybackOrder::new(7);
        let scope = [0, 1, 2, 3, 4, 5];
        let shuffle = mode(RepeatMode::None, true);
        order.reanchor(&scope, Some(3));
        assert_eq!(order.bag()[0], 3);
        assert_eq!(order.cursor(), 1);
        let mut played: Vec<usize> = (0..5)
            .map(|_| order.next(&scope, Some(3), false, shuffle).unwrap())
            .collect();
        assert!(!played.contains(&3));
        played.sort_unstable();
        assert_eq!(played, [0, 1, 2, 4, 5]);
        assert_eq!(order.next(&scope, Some(3), false, shuffle), None);
        assert!(!order.has_next(&scope, Some(3), shuffle));
    }

    #[test]
    fn a_shuffle_round_without_an_anchor_in_scope_plays_the_whole_scope() {
        let shuffle = mode(RepeatMode::None, true);
        for current in [None, Some(0)] {
            let mut order = PlaybackOrder::new(11);
            let folder = [4, 6, 9];
            let mut played: Vec<usize> = (0..3)
                .map(|_| order.next(&folder, current, false, shuffle).unwrap())
                .collect();
            played.sort_unstable();
            assert_eq!(played, folder, "current {current:?}");
            assert_eq!(order.next(&folder, current, false, shuffle), None);
        }
    }

    #[test]
    fn repeating_shuffle_never_plays_the_same_track_across_the_seam() {
        for seed in 1..200 {
            let mut order = PlaybackOrder::new(seed);
            let shuffle = mode(RepeatMode::All, true);
            let scope = [0, 1, 2];
            order.reanchor(&scope, Some(0));
            let mut last = 0;
            for _ in 0..12 {
                let next = order.next(&scope, Some(last), false, shuffle).unwrap();
                assert_ne!(next, last, "seed {seed}");
                last = next;
            }
        }
    }

    #[test]
    fn the_same_seed_gives_the_same_order() {
        let shuffle = mode(RepeatMode::All, true);
        let scope: Vec<usize> = (0..8).collect();
        let run = || {
            let mut order = PlaybackOrder::new(42);
            (0..10)
                .map(|_| order.next(&scope, Some(0), false, shuffle))
                .collect::<Vec<_>>()
        };
        assert_eq!(run(), run());
    }
}
