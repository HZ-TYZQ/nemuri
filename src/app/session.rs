//! 退出时记住当前曲目、进度和文件夹筛选，下次启动时回到原来的文件夹，
//! 暂停在原处，按空格续播。放的是播放列表里的一条时，还记住是哪个列表的第几条，
//! 恢复时回到那个列表的播放范围。

use std::path::Path;
use std::time::Duration;

use crate::marks;
use crate::player::PlayState;
use crate::progress::NEAR_END;
use crate::session::{ListEntry, Session};

use super::App;
use super::scope::{Cursor, Origin};

impl App {
    /// 在第一次扫描完成后调用：曲库就绪才能按路径找到曲目和文件夹。
    /// 文件夹已经不在了就显示全部；用户已经开始播放时不打断；
    /// 找不到曲目或打不开都安静放弃。
    pub(super) fn restore_session(&mut self) {
        let Some(session) = self.pending_session.take() else {
            return;
        };
        if let Some(folder) = session.folder
            && self
                .catalog
                .folders()
                .iter()
                .any(|(known, _)| *known == folder)
        {
            self.set_folder(Some(folder));
        }
        let Some(track) = session.track else {
            return;
        };
        if self.playback.player.state() != PlayState::Stopped
            || self.playback.playing_index.is_some()
        {
            return;
        }
        let Some(index) = self.index_for_path(&track) else {
            return;
        };
        // 列表或那一条对不上（删了、改了）时，当作曲库播放来恢复。
        let origin = session
            .entry
            .as_ref()
            .and_then(|saved| self.saved_entry(saved, &track))
            .unwrap_or(Origin::Library);
        // 上次停在结尾 3 秒以内（有终点时是终点前 3 秒以内或终点之后）：这一首算听完了，
        // 退回开头；有起点时退回起点。列表里的条目不用录音的起点和终点，按条目自己的范围：
        // 位置不在范围里（条目改过）或者离条目结尾 3 秒以内时，从条目开头开始。
        let item = &self.catalog.tracks()[index];
        let (start, end, outside) = match &origin {
            Origin::Library => {
                let bounds = marks::bounds(self.marks_of(item));
                let start = bounds.start.unwrap_or_default();
                (start, bounds.end.or(item.duration), false)
            }
            Origin::Entry { range, .. } => {
                let end = match (range.end, item.duration) {
                    (Some(end), Some(duration)) => Some(end.min(duration)),
                    (end, duration) => end.or(duration),
                };
                (range.start, end, session.position < range.start)
            }
        };
        let mut position = session.position;
        if outside || end.is_some_and(|end| position + NEAR_END >= end) {
            position = start;
        }
        // 位置恢复失败（例如文件被改短）时退回曲首，仍然选中这首歌；
        // 列表的条目退回条目开头，开头之前不是这一条的内容。
        let fallback = match &origin {
            Origin::Library => Duration::ZERO,
            Origin::Entry { range, .. } => range.start,
        };
        let opened = self
            .playback
            .player
            .open_at(&track, position)
            .map(|()| position)
            .or_else(|_| match fallback {
                Duration::ZERO => self.playback.player.open(&track).map(|()| Duration::ZERO),
                start => self.playback.player.open_at(&track, start).map(|()| start),
            });
        let Ok(position) = opened else {
            return;
        };

        self.playback.loads += 1;
        self.playback.spectrum.on_track_change();
        self.playback.playing_index = Some(index);
        self.playback.origin = origin.clone();
        self.apply_stop();
        self.view.pending_selected_path = Some(track);
        self.restore_pending_selection();
        if self.config.shuffle {
            self.reanchor_shuffle_bag(index);
        }
        if let Origin::Entry {
            playlist, entry, ..
        } = origin
        {
            self.enter_playlist(playlist, Cursor::At(entry));
        }
        let scope = match self.scope_summary() {
            Some((name, Some(number), count)) => format!("{name} {number}/{count} "),
            Some((name, None, _)) => format!("{name} "),
            None => String::new(),
        };
        self.view.message = Some(format!(
            "已恢复上次播放：{scope}{} {}，按空格继续",
            self.item_title().unwrap_or_default(),
            crate::track::fmt_duration(self.shown(position))
        ));
    }

    /// 会话里记的那一条：列表还在、第几条还在、路径也对得上时才算数。
    /// 片段的开头已经在文件之外（文件被剪短了）时也不算，当作曲库播放来恢复。
    fn saved_entry(&self, saved: &ListEntry, track: &Path) -> Option<Origin> {
        let playlist = self
            .playlists
            .all()
            .iter()
            .find(|playlist| playlist.name == saved.playlist)?;
        let entry = playlist.entries.get(saved.number.checked_sub(1)?)?;
        let duration = self
            .index_for_path(track)
            .and_then(|index| self.catalog.tracks()[index].duration);
        let playable = duration.is_none_or(|duration| entry.range.start < duration);
        (entry.path == track && playable).then(|| Origin::Entry {
            playlist: playlist.id,
            entry: entry.id,
            range: entry.range,
            name: entry.name.clone(),
        })
    }

    /// 正在放的是播放列表里的一条时，它是哪个列表的第几条；列表或这一条已经删掉时为 None。
    fn current_list_entry(&self) -> Option<ListEntry> {
        let Origin::Entry {
            playlist, entry, ..
        } = &self.playback.origin
        else {
            return None;
        };
        let playlist = self.playlists.get(*playlist)?;
        let position = playlist.entries.iter().position(|item| item.id == *entry)?;
        Some(ListEntry {
            playlist: playlist.name.clone(),
            number: position + 1,
        })
    }

    /// 退出时记录当前曲目、进度和文件夹筛选；两者都没有时清掉旧会话。
    /// 还没来得及恢复就退出时保留原文件，不能把上次的会话抹掉。
    pub fn save_session(&self) -> Result<(), String> {
        if self.pending_session.is_some() {
            return Ok(());
        }
        let path = &self.paths.session_file;
        let track = (self.playback.player.state() != PlayState::Stopped)
            .then(|| self.playback.player.current_path())
            .flatten();
        let folder = self.catalog.folder().cloned();
        let result = if track.is_none() && folder.is_none() {
            Session::clear(path)
        } else {
            Session {
                library_dir: self.library_dir.clone(),
                track: track.map(Path::to_path_buf),
                position: track.map_or(Duration::ZERO, |_| self.playback.player.position()),
                entry: track.and_then(|_| self.current_list_entry()),
                folder,
            }
            .save(path)
        };
        result.map_err(|error| format!("无法保存播放会话: {error}"))
    }
}
