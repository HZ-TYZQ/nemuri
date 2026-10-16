//! 命名播放列表：新建、删除、编辑和从中播放。
//!
//! 列表改动后立刻写盘；写不了时改动照样留着，提示原因，之后改动任何列表或退出时再写。

use crate::playlist::{Range, Saved};

use super::{App, BagUpdate, Overlay};

/// 改动后的提示；没能写盘时接上原因。
fn saved_message(done: String, saved: Saved) -> String {
    match saved {
        Saved::Yes => done,
        Saved::No(error) => format!("{done}，但{error}；之后改动或退出时再试"),
    }
}

impl App {
    /// 选中的播放列表里有几条；没有选中的列表时为 0。
    pub(super) fn selected_playlist_len(&self) -> usize {
        self.playlists
            .all()
            .get(self.view.playlist_selected)
            .map(|playlist| playlist.entries.len())
            .unwrap_or(0)
    }

    pub(super) fn create_playlist(&mut self) {
        match self.playlists.create(&self.view.name_input) {
            Ok(index) => {
                self.view.playlist_selected = index;
                self.view.overlay = Overlay::Playlists;
                self.view.message = Some("播放列表已创建".to_owned());
            }
            Err(error) => self.view.message = Some(error),
        }
    }

    pub(super) fn delete_selected_playlist(&mut self) {
        match self.playlists.delete(self.view.playlist_selected) {
            Ok(()) => {
                self.view.playlist_selected = self.view.playlist_selected.saturating_sub(1);
                self.view.message = Some("播放列表已删除，音乐文件未受影响".to_owned());
            }
            Err(error) => self.view.message = Some(error),
        }
        self.view.overlay = Overlay::Playlists;
    }

    /// 选中的歌曲整首加到选中的列表末尾。列表里已经有这首整首时照样加，提示一句：多半是误操作。
    pub(super) fn add_selected_to_playlist(&mut self) {
        let Some(track) = self.selected_track().cloned() else {
            self.view.message = Some("没有选中的歌曲".to_owned());
            return;
        };
        let index = self.view.playlist_selected;
        let Some(playlist) = self.playlists.all().get(index) else {
            self.view.message = Some("播放列表不存在".to_owned());
            return;
        };
        let name = playlist.name.clone();
        let already = playlist
            .entries
            .iter()
            .filter(|entry| entry.path == track.path && entry.range.is_whole())
            .count();
        match self.playlists.add_entry(index, &track, Range::WHOLE, None) {
            Ok(saved) => {
                let done = match already {
                    0 => format!("已加入“{name}”"),
                    _ => format!(
                        "已加入“{name}”（列表里已有这首，现在有 {} 条）",
                        already + 1
                    ),
                };
                self.view.message = Some(saved_message(done, saved));
            }
            Err(error) => self.view.message = Some(error),
        }
    }

    pub(super) fn play_playlist_from_selected(&mut self) {
        if self.output_blocks_playback() {
            return;
        }
        let Some(playlist) = self.playlists.all().get(self.view.playlist_selected) else {
            return;
        };
        let start = self
            .view
            .playlist_track_selected
            .min(playlist.entries.len());
        let paths: Vec<_> = playlist.entries[start..]
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        let mut paths = paths.into_iter();
        let Some(first) = paths.next() else {
            self.view.message = Some("播放列表是空的".to_owned());
            return;
        };
        self.playback.queue = paths.collect();
        match self.play_path(&first, true, BagUpdate::Reanchor) {
            Ok(()) => self.view.overlay = Overlay::None,
            Err(skip) => {
                self.advance(false, vec![skip]);
                if self.playback.playing_index.is_some() {
                    self.view.overlay = Overlay::None;
                }
            }
        }
    }

    pub(super) fn remove_playlist_track(&mut self) {
        match self.playlists.remove_entry(
            self.view.playlist_selected,
            self.view.playlist_track_selected,
        ) {
            Ok(saved) => {
                self.view.playlist_track_selected =
                    self.view.playlist_track_selected.saturating_sub(1);
                self.view.message = Some(saved_message(
                    "已从播放列表移除，音乐文件未受影响".to_owned(),
                    saved,
                ));
            }
            Err(error) => self.view.message = Some(error),
        }
    }

    /// 退出时把没存上的播放列表再写一次。
    pub fn save_playlists(&mut self) -> Result<(), String> {
        self.playlists.flush()
    }
}
