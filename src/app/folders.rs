//! 按文件夹筛选曲库：文件夹面板与筛选切换。

use crate::track::{Folder, Track};

use super::{App, Overlay};

impl App {
    /// 打开文件夹面板，光标落在当前的筛选上（第 0 行是“全部”）。
    pub(super) fn open_folders(&mut self) {
        self.view.folder_selected = self
            .catalog
            .folder()
            .and_then(|active| {
                self.catalog
                    .folders()
                    .iter()
                    .position(|(folder, _)| folder == active)
            })
            .map_or(0, |position| position + 1);
        self.view.overlay = Overlay::Folders;
    }

    /// 只看面板里选中的文件夹，选中第 0 行时显示全部。
    pub(super) fn apply_selected_folder(&mut self) {
        let chosen = self
            .view
            .folder_selected
            .checked_sub(1)
            .and_then(|row| self.catalog.folders().get(row))
            .cloned();
        self.view.overlay = Overlay::None;
        let message = match &chosen {
            Some((folder, count)) => format!("文件夹：{}（{count} 首）", folder.label()),
            None => format!("显示全部 {} 首", self.catalog.len()),
        };
        self.set_folder(chosen.map(|(folder, _)| folder));
        self.view.message = Some(message);
    }

    /// 曲目下标不受筛选影响，只有可见列表和光标需要跟着变。
    /// 光标优先留在原来选中的曲目上；它不在新文件夹里时落到正在播放的曲目，
    /// 两者都不在时回到第一行。
    fn set_folder(&mut self, folder: Option<Folder>) {
        let kept = |track: &Track| {
            folder
                .as_ref()
                .is_none_or(|folder| Folder::of(track) == *folder)
        };
        let cursor = self
            .selected_track()
            .filter(|track| kept(track))
            .or_else(|| self.current_track().filter(|track| kept(track)))
            .map(|track| track.path.clone());
        self.catalog.set_folder(folder);
        // 播放范围变了，随机袋要在新范围里重洗。
        if self.config.shuffle {
            match self.playback.playing_index {
                Some(current) => self.reanchor_shuffle_bag(current),
                None => self.playback.order.clear(),
            }
        }
        self.view.selected = 0;
        self.view.pending_selected_path = cursor;
        self.restore_pending_selection();
    }
}
