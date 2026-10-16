//! 命名播放列表：新建、删除、编辑和从中播放。
//!
//! 在一条上按 Enter 进入这个列表的播放范围（见 `scope.rs`）。条目可以移动、改名，
//! 开头和结尾从这个文件的跳转点里挑；片段从跳转点面板加进来（见 `marks.rs`）。
//! 列表改动后立刻写盘；写不了时改动照样留着，提示原因，之后改动任何列表或退出时再写。

use std::path::PathBuf;
use std::time::Duration;

use crate::marks::Mark;
use crate::playlist::{Entry, Range, Saved};
use crate::track::{Track, fmt_duration};

use super::marks::label;
use super::scope::Cursor;
use super::{App, Overlay};

/// 条目的开头还是结尾。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    #[default]
    Start,
    End,
}

/// 条目开头或结尾的一个可选位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundChoice {
    /// 文件里的位置；None 是文件结尾。
    pub position: Option<Duration>,
    /// 这个位置上的跳转点；文件开头和结尾没有。
    pub mark: Option<Mark>,
}

impl BoundChoice {
    /// 提示里的写法，例如“12:30 掏耳”“0:00”“文件结尾”。
    fn label(&self) -> String {
        match (&self.mark, self.position) {
            (Some(mark), _) => label(mark),
            (None, Some(position)) => fmt_duration(position),
            (None, None) => "文件结尾".to_owned(),
        }
    }
}

/// 正要加进播放列表的片段：从跳转点面板里选中的跳转点到下一个跳转点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewClip {
    pub path: PathBuf,
    pub range: Range,
    pub name: Option<String>,
}

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

    /// 新建播放列表。是在“加到播放列表”面板里新建的，就把正要加的片段直接加进去。
    pub(super) fn create_playlist(&mut self) {
        match self.playlists.create(&self.view.name_input) {
            Ok(index) => {
                self.view.playlist_selected = index;
                if self.view.clip.is_some() {
                    self.add_clip_to_selected_playlist();
                } else {
                    self.view.overlay = Overlay::Playlists;
                    self.view.message = Some("播放列表已创建".to_owned());
                }
            }
            Err(error) => self.view.message = Some(error),
        }
    }

    /// “加到播放列表”面板里的 Enter：把片段加到选中列表的末尾，回到跳转点面板。
    pub(super) fn add_clip_to_selected_playlist(&mut self) {
        let Some(clip) = self.view.clip.clone() else {
            return;
        };
        let index = self.view.playlist_selected;
        let Some(playlist) = self.playlists.all().get(index) else {
            return;
        };
        let (id, name) = (playlist.id, playlist.name.clone());
        let Some(track) = self
            .index_for_path(&clip.path)
            .map(|index| self.catalog.tracks()[index].clone())
        else {
            self.view.message = Some("这首已经不在曲库中".to_owned());
            return;
        };
        match self
            .playlists
            .add_entry(index, &track, clip.range, clip.name.clone())
        {
            Ok(saved) => {
                // 写片段长度而不写起止：正在放片段时，面板上的时间是片段内的，写起止容易混淆。
                let length = clip
                    .range
                    .end
                    .or(track.duration)
                    .map(|end| fmt_duration(end.saturating_sub(clip.range.start)));
                let what = match (&clip.name, length) {
                    (Some(clip_name), Some(length)) => format!("{clip_name}（{length}）"),
                    (Some(clip_name), None) => clip_name.clone(),
                    (None, Some(length)) => format!("{length} 的片段"),
                    (None, None) => "片段".to_owned(),
                };
                self.view.message = Some(saved_message(format!("已加进“{name}”：{what}"), saved));
                self.view.clip_list = Some(id);
                self.view.clip = None;
                self.view.overlay = Overlay::Marks;
                self.after_playlist_edit(id, None);
            }
            Err(error) => self.view.message = Some(error),
        }
    }

    /// 选中的列表条目，以及它在曲库里的曲目；文件不在曲库中时曲目为 None。
    fn selected_entry(&self) -> Option<(&Entry, Option<&Track>)> {
        let entry = self
            .playlists
            .all()
            .get(self.view.playlist_selected)?
            .entries
            .get(self.view.playlist_track_selected)?;
        let track = self
            .index_for_path(&entry.path)
            .map(|index| &self.catalog.tracks()[index]);
        Some((entry, track))
    }

    /// 条目开头或结尾能挑的位置：开头是文件开头和结尾之前的跳转点，
    /// 结尾是开头之后的跳转点和文件结尾。跳转点按扫描时的时长截取。
    pub fn bound_choices(&self) -> Vec<BoundChoice> {
        let Some((entry, Some(track))) = self.selected_entry() else {
            return Vec::new();
        };
        let marks = self.marks_of(track).iter();
        let choice = |mark: &Mark| BoundChoice {
            position: Some(mark.position),
            mark: Some(mark.clone()),
        };
        let range = entry.range;
        match self.view.bound {
            Bound::Start => std::iter::once(BoundChoice {
                position: Some(Duration::ZERO),
                mark: None,
            })
            .chain(
                marks
                    .filter(|mark| {
                        !mark.position.is_zero() && range.end.is_none_or(|end| mark.position < end)
                    })
                    .map(choice),
            )
            .collect(),
            Bound::End => marks
                .filter(|mark| mark.position > range.start)
                .map(choice)
                .chain(std::iter::once(BoundChoice {
                    position: None,
                    mark: None,
                }))
                .collect(),
        }
    }

    /// 条目面板里的 `s` / `S`：挑开头或结尾，光标落在现在的位置上。
    /// 这个文件没有跳转点（短曲目、还没设、或者不在曲库中）时只能整首播放，只提示。
    pub(super) fn open_entry_bound(&mut self, bound: Bound) {
        let Some((entry, track)) = self.selected_entry() else {
            return;
        };
        if track.is_none_or(|track| self.marks_of(track).is_empty()) {
            self.view.message = Some("这首没有跳转点，只能整首播放".to_owned());
            return;
        }
        let current = match bound {
            Bound::Start => Some(entry.range.start),
            Bound::End => entry.range.end,
        };
        self.view.bound = bound;
        self.view.bound_selected = self
            .bound_choices()
            .iter()
            .position(|choice| choice.position == current)
            .unwrap_or(0);
        self.view.overlay = Overlay::EntryBound;
    }

    /// 开头和结尾选择面板里的 Enter：改好后回到条目面板。
    pub(super) fn set_entry_bound(&mut self) {
        let Some(choice) = self.bound_choices().get(self.view.bound_selected).cloned() else {
            return;
        };
        let Some((entry, _)) = self.selected_entry() else {
            return;
        };
        let mut range = entry.range;
        let what = match self.view.bound {
            Bound::Start => {
                range.start = choice.position.unwrap_or_default();
                "开头"
            }
            Bound::End => {
                range.end = choice.position;
                "结尾"
            }
        };
        self.view.overlay = Overlay::PlaylistTracks;
        match self.playlists.set_range(
            self.view.playlist_selected,
            self.view.playlist_track_selected,
            range,
        ) {
            Ok(saved) => {
                let done = format!("{what}改为 {}", choice.label());
                self.view.message = Some(saved_message(done, saved));
            }
            Err(error) => self.view.message = Some(error),
        }
    }

    /// 条目面板里的 `e`：打开改名输入框，预填现有的名字。
    pub(super) fn start_renaming_entry(&mut self) {
        let Some((entry, _)) = self.selected_entry() else {
            return;
        };
        self.view.name_input = entry.name.clone().unwrap_or_default();
        self.view.overlay = Overlay::EntryName;
    }

    /// 改名输入框里的 Enter：留空就清除名字，然后回到条目面板。
    pub(super) fn rename_selected_entry(&mut self) {
        self.view.overlay = Overlay::PlaylistTracks;
        match self.playlists.rename_entry(
            self.view.playlist_selected,
            self.view.playlist_track_selected,
            &self.view.name_input,
        ) {
            Ok(Saved::Yes) => {}
            Ok(saved) => self.view.message = Some(saved_message("已改名".to_owned(), saved)),
            Err(error) => self.view.message = Some(error),
        }
    }

    /// 条目面板里的 `J` / `K`：选中的条目下移或上移，选择跟着它走。到头时不动。
    pub(super) fn move_playlist_entry(&mut self, offset: isize) {
        let Some(id) = self
            .playlists
            .all()
            .get(self.view.playlist_selected)
            .map(|playlist| playlist.id)
        else {
            return;
        };
        let position = self.view.playlist_track_selected;
        let Ok(saved) = self
            .playlists
            .move_entry(self.view.playlist_selected, position, offset)
        else {
            return;
        };
        self.view.playlist_track_selected = position.saturating_add_signed(offset);
        if let Saved::No(_) = saved {
            self.view.message = Some(saved_message("已移动".to_owned(), saved));
        }
        self.after_playlist_edit(id, None);
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

    /// 扫描后把播放列表里改了名的文件跟过去。没能写盘时交回一句说明，接在扫描完成的提示后面。
    pub(super) fn follow_renamed_playlists(&mut self) -> Option<String> {
        match self
            .playlists
            .follow_renames(self.catalog.tracks(), &self.library_dir)
        {
            Saved::Yes => None,
            Saved::No(error) => Some(format!("{error}；之后改动或退出时再试")),
        }
    }

    /// 退出时把没存上的播放列表再写一次。
    pub fn save_playlists(&mut self) -> Result<(), String> {
        self.playlists.flush()
    }
}
