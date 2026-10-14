//! 临时播放队列：跳播、移除、重排和清空。

use super::{App, BagUpdate, Overlay};

impl App {
    /// 跳到队列中的某一项：它之前的条目视为已跳过，直接从队列丢弃。
    pub(super) fn play_queue_selected(&mut self) {
        if self.output_blocks_playback() {
            return;
        }
        if self.view.queue_selected >= self.playback.queue.len() {
            self.view.message = Some("队列是空的".to_owned());
            return;
        }
        self.playback.queue.drain(..self.view.queue_selected);
        self.view.queue_selected = 0;
        let Some(path) = self.playback.queue.pop_front() else {
            return;
        };
        let Err(skip) = self.play_path(&path, true, BagUpdate::Reanchor) else {
            self.view.overlay = Overlay::None;
            return;
        };
        // 该条目无法播放时沿队列继续向后尝试，与自动切歌的跳过行为一致；
        // 它的失败原因并入这一轮的汇总提示。
        self.advance(false, vec![skip]);
        if self.playback.playing_index.is_some() {
            self.view.overlay = Overlay::None;
        }
    }

    pub(super) fn remove_queue_selected(&mut self) {
        if self
            .playback
            .queue
            .remove(self.view.queue_selected)
            .is_none()
        {
            self.view.message = Some("队列是空的".to_owned());
            return;
        }
        self.view.queue_selected = self
            .view
            .queue_selected
            .min(self.playback.queue.len().saturating_sub(1));
        self.view.message = Some(format!(
            "已从队列移除，队列中还有 {} 首",
            self.playback.queue.len()
        ));
    }

    pub(super) fn clear_queue(&mut self) {
        if self.playback.queue.is_empty() {
            self.view.message = Some("队列是空的".to_owned());
            return;
        }
        let removed = self.playback.queue.len();
        self.playback.queue.clear();
        self.view.queue_selected = 0;
        self.view.message = Some(format!("已清空队列（{removed} 首），音乐文件未受影响"));
    }

    /// 上移或下移选中条目，选择跟随该条目移动。
    /// 两端都要在队列范围内：选择可能还没跟上刚刚缩短的队列。
    pub(super) fn move_queue_selected(&mut self, offset: isize) {
        let len = self.playback.queue.len();
        let source = self.view.queue_selected;
        let Some(target) = source.checked_add_signed(offset) else {
            return;
        };
        if source >= len || target >= len {
            return;
        }
        self.playback.queue.swap(source, target);
        self.view.queue_selected = target;
    }

    /// 自动切歌取走队首。后面的条目都往前挪一位，选择跟着原来选中的那一条走，
    /// 不必等下一个 tick 修正，也不会指到队列外面。
    pub(super) fn take_queue_head(&mut self) -> Option<std::path::PathBuf> {
        let path = self.playback.queue.pop_front()?;
        self.view.queue_selected = self.view.queue_selected.saturating_sub(1);
        Some(path)
    }
}
