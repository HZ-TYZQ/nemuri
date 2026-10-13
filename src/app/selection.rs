//! 曲库光标、搜索编辑，以及重扫后按路径恢复选中项。

use super::App;

impl App {
    pub(super) fn select_next(&mut self) {
        self.cancel_pending_selection_restore();
        if !self.visible_indices().is_empty() {
            self.view.selected = (self.view.selected + 1).min(self.visible_indices().len() - 1);
        }
    }

    pub(super) fn select_previous(&mut self) {
        self.cancel_pending_selection_restore();
        self.view.selected = self.view.selected.saturating_sub(1);
    }

    pub(super) fn restore_pending_selection(&mut self) {
        if !self.catalog.query().is_empty() && self.catalog.search_running() {
            return;
        }
        let Some(path) = self.view.pending_selected_path.take() else {
            return;
        };
        self.view.selected = self
            .index_for_path(&path)
            .and_then(|index| {
                self.visible_indices()
                    .iter()
                    .position(|visible| *visible == index)
            })
            .unwrap_or(0);
    }

    /// 换了查询词，光标回到第一条结果。
    pub(super) fn edit_search(&mut self, query: String) {
        self.cancel_pending_selection_restore();
        self.catalog.set_query(query);
        self.view.selected = 0;
    }

    /// 关闭搜索框并清空查询词。
    pub(super) fn cancel_search(&mut self) {
        self.cancel_pending_selection_restore();
        self.view.search_active = false;
        self.catalog.set_query(String::new());
        self.view.selected = 0;
    }

    pub(super) fn cancel_pending_selection_restore(&mut self) {
        self.view.pending_selected_path = None;
    }

    pub(super) fn clamp_selections(&mut self) {
        self.view.selected = self
            .view
            .selected
            .min(self.visible_indices().len().saturating_sub(1));
        self.view.playlist_selected = self
            .view
            .playlist_selected
            .min(self.playlists.all().len().saturating_sub(1));
        self.view.queue_selected = self
            .view
            .queue_selected
            .min(self.playback.queue.len().saturating_sub(1));
        self.view.folder_selected = self.view.folder_selected.min(self.catalog.folders().len());
    }
}
