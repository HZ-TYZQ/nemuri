//! 命名播放列表：新建、删除、编辑和从中播放。
//!
//! 在一条上按 Enter 进入这个列表的播放范围（见 `scope.rs`）。
//! 列表改动后立刻写盘；写不了时改动照样留着，提示原因，之后改动任何列表或退出时再写。

use crate::playlist::{Range, Saved};

use super::scope::Cursor;
use super::{App, Overlay};

/// 改动后的提示；没能写盘时接上原因。
pub(super) fn saved_message(done: String, saved: Saved) -> String {
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

    /// 删掉正在播放的列表时回到曲库范围，正在放的这一项照样放完。
    pub(super) fn delete_selected_playlist(&mut self) {
        let id = self
            .playlists
            .all()
            .get(self.view.playlist_selected)
            .map(|playlist| playlist.id);
        match self.playlists.delete(self.view.playlist_selected) {
            Ok(()) => {
                self.view.playlist_selected = self.view.playlist_selected.saturating_sub(1);
                let back = id.is_some_and(|id| self.after_playlist_deleted(id));
                self.view.message = Some(
                    if back {
                        "播放列表已删除，音乐文件未受影响；播放回到曲库"
                    } else {
                        "播放列表已删除，音乐文件未受影响"
                    }
                    .to_owned(),
                );
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
        let (id, name) = (playlist.id, playlist.name.clone());
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
                self.after_playlist_edit(id, None);
            }
            Err(error) => self.view.message = Some(error),
        }
    }

    /// 条目面板里的 Enter：进入这个列表的播放范围，从选中的这一条开始。队列不受影响。
    /// 这一条放不成时沿列表往后试，和自动切歌一样。
    pub(super) fn play_playlist_from_selected(&mut self) {
        if self.output_blocks_playback() {
            return;
        }
        let Some(playlist) = self.playlists.all().get(self.view.playlist_selected) else {
            return;
        };
        let Some(entry) = playlist.entries.get(self.view.playlist_track_selected) else {
            self.view.message = Some("播放列表是空的".to_owned());
            return;
        };
        let (playlist, entry) = (playlist.id, entry.id);
        // 先放再进范围：历史里记下的是放它之前的范围。
        let played = self.play_entry(playlist, entry, true);
        self.enter_playlist(playlist, Cursor::At(entry));
        match played {
            Ok(()) => self.view.overlay = Overlay::None,
            Err(skip) => {
                self.advance(false, vec![skip]);
                if self.playback.playing_index.is_some() {
                    self.view.overlay = Overlay::None;
                }
            }
        }
    }

    /// 删掉正在放的那一条时，它照样放完，然后接原来排在它后面的那一条。
    pub(super) fn remove_playlist_track(&mut self) {
        let removed = self
            .playlists
            .all()
            .get(self.view.playlist_selected)
            .and_then(|playlist| {
                let entry = playlist.entries.get(self.view.playlist_track_selected)?;
                Some((playlist.id, entry.id))
            });
        match self.playlists.remove_entry(
            self.view.playlist_selected,
            self.view.playlist_track_selected,
        ) {
            Ok(saved) => {
                if let Some((playlist, entry)) = removed {
                    let position = self.view.playlist_track_selected;
                    self.after_playlist_edit(playlist, Some((entry, position)));
                }
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
