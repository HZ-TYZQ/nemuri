//! 播放范围：曲库（含文件夹筛选），或者一个播放列表。
//!
//! `n`、自然播完、循环和随机都在当前范围里走；队列里的插播不改变范围。
//! 列表范围有自己的播放顺序，按条目号排，循环方式用这个列表自己的；随机开关和曲库共用。
//! 当前条目按编号认，列表增删条目之后还能找到它。
//! 列表里的条目只按自己的范围播放，不用录音的起点和终点，也不碰收听进度。

use std::path::PathBuf;

use crate::playlist::{EntryId, Playlist, PlaylistId, Range};
use crate::track::{PlaybackMode, RepeatMode};

use super::order::PlaybackOrder;
use super::playback::{Skip, file_label};
use super::{App, BagUpdate};

/// 正在播放的这一项从哪里来。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub enum Origin {
    /// 曲库，包括从曲库排进队列的插播：按录音的起点、终点和收听进度播放。
    #[default]
    Library,
    /// 播放列表里的一条：只按这一条自己的范围播放。范围和名字在装载时复制下来，
    /// 之后改了这一条，正在放的这一遍不受影响。
    Entry {
        playlist: PlaylistId,
        entry: EntryId,
        range: Range,
        name: Option<String>,
    },
}

/// 列表范围里的当前位置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cursor {
    /// 当前条目：正在播放或刚放完的那一条。
    At(EntryId),
    /// 当前条目被删掉了，下一条是现在排在这个位置上的那一条。
    Before(usize),
}

/// 某一刻的播放范围，记在历史里，“上一首”退回时一起恢复。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeMark {
    Library,
    Playlist {
        playlist: PlaylistId,
        cursor: Cursor,
    },
}

/// 播放过的一项，“上一首”从这里往回取。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Played {
    pub path: PathBuf,
    pub origin: Origin,
    /// 放它时的播放范围。
    pub scope: ScopeMark,
}

pub(crate) enum Scope {
    Library,
    Playlist(ListScope),
}

pub(crate) struct ListScope {
    pub(crate) playlist: PlaylistId,
    pub(crate) cursor: Cursor,
    /// 列表自己的播放顺序，按条目号排；和曲库的随机袋互不影响。
    order: PlaybackOrder,
}

/// 列表里的条目号：`0..条目数`，交给播放顺序当作范围。
fn positions(playlist: &Playlist) -> Vec<usize> {
    (0..playlist.entries.len()).collect()
}

fn position_of(playlist: &Playlist, entry: EntryId) -> Option<usize> {
    playlist.entries.iter().position(|item| item.id == entry)
}

impl App {
    /// 当前在哪个列表的范围里；曲库范围或者列表已经不在了时为 None。
    pub(super) fn in_playlist(&self) -> Option<PlaylistId> {
        match &self.playback.scope {
            Scope::Playlist(scope) => self
                .playlists
                .get(scope.playlist)
                .map(|playlist| playlist.id),
            Scope::Library => None,
        }
    }

    /// 正在播放的这一项是不是曲库播放：曲库里点的，或者从曲库排进队列的插播。
    pub(super) fn plays_from_library(&self) -> bool {
        self.playback.origin == Origin::Library
    }

    /// 列表范围的说明：列表名、当前条目是第几条（从 1 数；当前条目删掉了时为 None）和条目数。
    /// 曲库范围时为 None。
    pub fn scope_summary(&self) -> Option<(&str, Option<usize>, usize)> {
        let Scope::Playlist(scope) = &self.playback.scope else {
            return None;
        };
        let playlist = self.playlists.get(scope.playlist)?;
        let number = match scope.cursor {
            Cursor::At(entry) => position_of(playlist, entry).map(|position| position + 1),
            Cursor::Before(_) => None,
        };
        Some((playlist.name.as_str(), number, playlist.entries.len()))
    }

    /// 当前范围的循环方式和随机开关：列表范围用这个列表的循环方式。
    pub fn playback_mode(&self) -> PlaybackMode {
        let repeat = match self.in_playlist().and_then(|id| self.playlists.get(id)) {
            Some(playlist) => playlist.repeat,
            None => self.config.repeat,
        };
        PlaybackMode {
            repeat,
            shuffle: self.config.shuffle,
        }
    }

    /// 曲库范围的循环方式和随机开关。
    pub(super) fn library_mode(&self) -> PlaybackMode {
        PlaybackMode {
            repeat: self.config.repeat,
            shuffle: self.config.shuffle,
        }
    }

    /// 现在的播放范围在历史里的记法。
    pub(super) fn scope_mark(&self) -> ScopeMark {
        match &self.playback.scope {
            Scope::Library => ScopeMark::Library,
            Scope::Playlist(scope) => ScopeMark::Playlist {
                playlist: scope.playlist,
                cursor: scope.cursor,
            },
        }
    }

    /// 进入一个列表的范围，当前位置落在 `cursor` 上。随机播放时在列表里重新洗牌，
    /// 当前条目算已播。
    pub(super) fn enter_playlist(&mut self, playlist: PlaylistId, cursor: Cursor) {
        let order = self.playback.order.split();
        self.playback.scope = Scope::Playlist(ListScope {
            playlist,
            cursor,
            order,
        });
        self.reanchor_playlist_bag();
    }

    /// 回到曲库范围。曲库的随机袋由装载曲目时的 [`BagUpdate`] 管。
    pub(super) fn enter_library(&mut self) {
        self.playback.scope = Scope::Library;
    }

    /// 恢复历史里记下的播放范围；那个列表已经删掉时回到曲库。
    pub(super) fn restore_scope(&mut self, mark: ScopeMark) {
        match mark {
            ScopeMark::Playlist { playlist, cursor } if self.playlists.get(playlist).is_some() => {
                self.enter_playlist(playlist, cursor);
            }
            ScopeMark::Playlist { .. } | ScopeMark::Library => self.return_to_library(),
        }
    }

    /// 从列表范围回到曲库，随机播放时以正在播放的曲目为首重洗曲库的随机袋。
    fn return_to_library(&mut self) {
        self.enter_library();
        if self.config.shuffle {
            match self.playback.playing_index {
                Some(current) => self.reanchor_shuffle_bag(current),
                None => self.playback.order.clear(),
            }
        }
    }

    /// 当前范围里有多少可以放的：自动跳过无法播放的曲目时最多试这么多次。
    pub(super) fn scope_len(&self) -> usize {
        match self.in_playlist().and_then(|id| self.playlists.get(id)) {
            Some(playlist) => playlist.entries.len(),
            None => self.catalog.scope().len(),
        }
    }

    /// 列表范围里的下一条；放完了时为 None。删掉的当前条目之后，下一条就是原来排在它后面的那一条，
    /// 单曲循环也一样。
    pub(super) fn next_entry(&mut self, natural_end: bool) -> Option<EntryId> {
        let mode = self.playback_mode();
        let Scope::Playlist(scope) = &mut self.playback.scope else {
            return None;
        };
        let playlist = self.playlists.get(scope.playlist)?;
        let (current, natural_end) = match scope.cursor {
            Cursor::At(entry) => (position_of(playlist, entry), natural_end),
            Cursor::Before(next) => (
                next.checked_sub(1),
                natural_end && mode.repeat != RepeatMode::One,
            ),
        };
        let next = scope
            .order
            .next(&positions(playlist), current, natural_end, mode)?;
        Some(playlist.entries[next].id)
    }

    /// 不动随机袋地回答当前范围里还有没有下一首，供系统媒体控制显示。
    pub(super) fn scope_has_next(&self) -> bool {
        let mode = self.playback_mode();
        if let Scope::Playlist(scope) = &self.playback.scope
            && let Some(playlist) = self.playlists.get(scope.playlist)
        {
            let current = match scope.cursor {
                Cursor::At(entry) => position_of(playlist, entry),
                Cursor::Before(next) => next.checked_sub(1),
            };
            return scope.order.has_next(&positions(playlist), current, mode);
        }
        self.playback.order.has_next(
            self.catalog.scope(),
            self.playback.playing_index,
            self.library_mode(),
        )
    }

    /// 列表范围的当前位置移到 `entry` 上。
    pub(super) fn move_cursor_to(&mut self, entry: EntryId) {
        if let Scope::Playlist(scope) = &mut self.playback.scope {
            scope.cursor = Cursor::At(entry);
        }
    }

    /// 停止时按空格要从哪一条放：列表范围的当前条目；当前条目删掉了时是排在它后面的那一条。
    pub(super) fn entry_to_resume(&self) -> Option<(PlaylistId, EntryId)> {
        let Scope::Playlist(scope) = &self.playback.scope else {
            return None;
        };
        let playlist = self.playlists.get(scope.playlist)?;
        let entry = match scope.cursor {
            Cursor::At(entry) => playlist.entries.iter().find(|item| item.id == entry),
            Cursor::Before(next) => playlist
                .entries
                .get(next)
                .or_else(|| playlist.entries.last()),
        }?;
        Some((playlist.id, entry.id))
    }

    /// 放列表里的一条。文件得在曲库里，和别的播放一样。
    pub(super) fn play_entry(
        &mut self,
        playlist: PlaylistId,
        entry: EntryId,
        remember_current: bool,
    ) -> Result<(), Skip> {
        let Some(item) = self
            .playlists
            .get(playlist)
            .and_then(|list| list.entries.iter().find(|item| item.id == entry))
        else {
            return Err(Skip::new("播放列表条目", "已经不在列表里"));
        };
        let path = item.path.clone();
        let origin = Origin::Entry {
            playlist,
            entry,
            range: item.range,
            name: item.name.clone(),
        };
        let Some(index) = self.index_for_path(&path) else {
            return Err(Skip::new(file_label(&path), "不在曲库中"));
        };
        self.play_item(index, origin, remember_current, BagUpdate::Leave)
    }

    /// 这一条还在不在那个列表里。
    pub(super) fn entry_exists(&self, playlist: PlaylistId, entry: EntryId) -> bool {
        self.playlists
            .get(playlist)
            .is_some_and(|list| list.entries.iter().any(|item| item.id == entry))
    }

    /// 列表改过之后：当前条目被删掉时记下它原来的位置，随机播放时重新洗牌。
    /// `removed` 是删掉的那一条和它原来的位置。
    pub(super) fn after_playlist_edit(
        &mut self,
        playlist: PlaylistId,
        removed: Option<(EntryId, usize)>,
    ) {
        let Scope::Playlist(scope) = &mut self.playback.scope else {
            return;
        };
        if scope.playlist != playlist {
            return;
        }
        if let Some((entry, position)) = removed {
            match scope.cursor {
                Cursor::At(current) if current == entry => {
                    scope.cursor = Cursor::Before(position);
                }
                Cursor::Before(next) if position < next => scope.cursor = Cursor::Before(next - 1),
                Cursor::At(_) | Cursor::Before(_) => {}
            }
        }
        self.reanchor_playlist_bag();
    }

    /// 列表删掉了：正在它的范围里时回到曲库，返回 true。
    pub(super) fn after_playlist_deleted(&mut self, playlist: PlaylistId) -> bool {
        match &self.playback.scope {
            Scope::Playlist(scope) if scope.playlist == playlist => {
                self.return_to_library();
                true
            }
            Scope::Playlist(_) | Scope::Library => false,
        }
    }

    /// 随机播放时在列表里重新洗牌，当前条目算已播；当前条目删掉了时剩下的都还没播。
    /// 没开随机时清掉，下次打开时再洗。
    pub(super) fn reanchor_playlist_bag(&mut self) {
        let shuffle = self.config.shuffle;
        let Scope::Playlist(scope) = &mut self.playback.scope else {
            return;
        };
        let Some(playlist) = self.playlists.get(scope.playlist) else {
            return;
        };
        if !shuffle {
            scope.order.clear();
            return;
        }
        let current = match scope.cursor {
            Cursor::At(entry) => position_of(playlist, entry),
            Cursor::Before(_) => None,
        };
        scope.order.reanchor(&positions(playlist), current);
    }
}
