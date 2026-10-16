//! 长录音各自的跳转点：听的时候记下的位置，之后可以直接跳回去。
//!
//! 和收听进度一样是用户数据，按文件记、跟着改名走，但单独存一份文件：
//! 进度拖回开头时整条记录会删掉，跳转点不能跟着没了。只给长录音设跳转点由调用方把关。
//! 跳转点按位置排序，精确到毫秒；没有名字时界面显示时间。
//!
//! 一个跳转点可以是录音的起点或终点：从头播放时从起点开始，播到终点就算播完。
//! 每个录音最多一个起点、一个终点，起点在终点之前；删掉那个跳转点，起点或终点也就没了。

use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::file_records::{self, FileRecords, Stamped};
use crate::track::Track;

const MARKS_VERSION: u32 = 1;
/// 提示里对这份数据的称呼。
const WHAT: &str = "跳转点";
/// 相距不到这么远算同一处：不重复添加；跳转后实际位置可能比跳转点早一点，
/// 判断当前所在段和下一个跳转点时也留这么多余量。
pub const SAME_SPOT: Duration = Duration::from_secs(1);
/// 刚过一个跳转点这么久之内往前跳，跳到再前面一个，而不是回到这一个。
pub const PREVIOUS_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub position: Duration,
    pub name: Option<String>,
    pub role: Role,
}

/// 跳转点是不是录音的起点或终点。一个跳转点只能有一种，不会同时是起点和终点。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    #[default]
    Plain,
    Start,
    End,
}

/// 一个录音的起点和终点；没有设时为 None。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    pub start: Option<Duration>,
    pub end: Option<Duration>,
}

/// [`MarkStore::toggle_role`] 改了什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoleChange {
    /// 这个跳转点现在的角色；取消时是 [`Role::Plain`]。
    pub role: Role,
    /// 它原来的角色：起点直接改成终点、终点直接改成起点时，原来那一个就没了。
    pub was: Role,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleError {
    /// 没有这个跳转点。
    Missing,
    /// 起点要在这个终点之前。
    StartNotBeforeEnd(Duration),
    /// 终点要在这个起点之后；没有起点时要在 0:00 之后，这里是 None。
    EndNotAfterStart(Option<Duration>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddError {
    /// 离这个已有的跳转点不到 [`SAME_SPOT`]。
    TooClose(Duration),
    /// 路径不是 UTF-8，存不进 JSON。
    UnsupportedPath,
    /// 启动时跳转点文件读不了或没能改名保留，这次运行不能改动跳转点。
    ReadOnly,
}

#[derive(Serialize, Deserialize)]
struct RawStore {
    version: u32,
    recordings: Vec<RawRecording>,
}

#[derive(Serialize, Deserialize)]
struct RawRecording {
    path: String,
    file_size: u64,
    modified_ns: i64,
    marks: Vec<RawMark>,
}

#[derive(Serialize, Deserialize)]
struct RawMark {
    position_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    start: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    end: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Default)]
pub struct MarkStore {
    /// 每个录音的跳转点按位置排序，且不为空：删光时整条记录删掉。
    entries: FileRecords<Vec<Mark>>,
    /// 启动时原文件读不了或没能改名保留：这次运行不写盘，免得空记录覆盖它。
    /// 这时记录是空的，也不让添加，免得加了却存不下来。
    read_only: bool,
}

impl MarkStore {
    /// 读取跳转点文件；还没有时是空记录。用不了时返回空记录和一句说明，
    /// 规则见 [`file_records::load_json`]。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let loaded =
            file_records::load_json(path, WHAT, |raw: &RawStore| raw.version == MARKS_VERSION);
        let mut store = loaded.raw.map(Self::from_raw).unwrap_or_default();
        store.read_only = loaded.read_only;
        (store, loaded.message)
    }

    /// 写盘；启动时原文件读不了或没能保留时什么也不做。
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if self.read_only {
            return Ok(());
        }
        let mut recordings: Vec<RawRecording> = self
            .entries
            .iter()
            // 添加时已经拒绝了非 UTF-8 路径，这里只是不让它们弄坏文件。
            .filter_map(|(path, entry)| {
                Some(RawRecording {
                    path: path.to_str()?.to_owned(),
                    file_size: entry.file_size,
                    modified_ns: entry.modified_ns,
                    marks: entry
                        .value
                        .iter()
                        .map(|mark| RawMark {
                            position_ms: millis(mark.position),
                            name: mark.name.clone(),
                            start: mark.role == Role::Start,
                            end: mark.role == Role::End,
                        })
                        .collect(),
                })
            })
            .collect();
        recordings.sort_by(|left, right| left.path.cmp(&right.path));
        let raw = RawStore {
            version: MARKS_VERSION,
            recordings,
        };
        file_records::save_json(path, WHAT, &raw)
    }

    /// 这个文件的全部跳转点，按位置排序；没有时为空。
    pub fn marks(&self, path: &Path) -> &[Mark] {
        self.entries
            .get(path)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// 在 `position`（截到毫秒）给 `track` 加一个没有名字的跳转点，返回它在列表里的位置。
    pub fn add(&mut self, track: &Track, position: Duration) -> Result<usize, AddError> {
        if self.read_only {
            return Err(AddError::ReadOnly);
        }
        if track.path.to_str().is_none() {
            return Err(AddError::UnsupportedPath);
        }
        let position = Duration::from_millis(millis(position));
        let mut marks = self.marks(&track.path).to_vec();
        if let Some(near) = marks
            .iter()
            .find(|mark| mark.position.abs_diff(position) < SAME_SPOT)
        {
            return Err(AddError::TooClose(near.position));
        }
        let index = marks.partition_point(|mark| mark.position < position);
        marks.insert(
            index,
            Mark {
                position,
                name: None,
                role: Role::Plain,
            },
        );
        self.entries.set(track, Some(marks));
        Ok(index)
    }

    /// 删掉 `track` 的第 `index` 个跳转点，返回删掉的那个；删光时整条记录删掉。
    pub fn remove(&mut self, track: &Track, index: usize) -> Option<Mark> {
        let mut marks = self.marks(&track.path).to_vec();
        if index >= marks.len() {
            return None;
        }
        let removed = marks.remove(index);
        self.entries
            .set(track, (!marks.is_empty()).then_some(marks));
        Some(removed)
    }

    /// 给 `track` 的第 `index` 个跳转点改名；去掉首尾空白后为空就清除名字。
    /// 记录有变化时返回 true。
    pub fn rename(&mut self, track: &Track, index: usize, name: &str) -> bool {
        let mut marks = self.marks(&track.path).to_vec();
        let Some(mark) = marks.get_mut(index) else {
            return false;
        };
        let name = name.trim();
        mark.name = (!name.is_empty()).then(|| name.to_owned());
        self.entries.set(track, Some(marks))
    }

    /// 把 `track` 的第 `index` 个跳转点设为起点或终点（`role`），它已经是了就取消。
    /// 设新的时原来的起点或终点变回普通跳转点；起点要在终点之前，终点要在起点和 0:00 之后。
    pub fn toggle_role(
        &mut self,
        track: &Track,
        index: usize,
        role: Role,
    ) -> Result<RoleChange, RoleError> {
        let mut marks = self.marks(&track.path).to_vec();
        let Some(mark) = marks.get(index) else {
            return Err(RoleError::Missing);
        };
        let (position, was) = (mark.position, mark.role);
        let new = if was == role { Role::Plain } else { role };
        // 这个跳转点原来的角色会让出来，不拿它自己比。
        let other = |wanted: Role| {
            marks
                .iter()
                .enumerate()
                .find(|(other, mark)| *other != index && mark.role == wanted)
                .map(|(_, mark)| mark.position)
        };
        match new {
            Role::Start => {
                if let Some(end) = other(Role::End)
                    && end <= position
                {
                    return Err(RoleError::StartNotBeforeEnd(end));
                }
            }
            Role::End => {
                let start = other(Role::Start);
                if position <= start.unwrap_or_default() {
                    return Err(RoleError::EndNotAfterStart(start));
                }
            }
            Role::Plain => {}
        }
        for mark in &mut marks {
            if new != Role::Plain && mark.role == new {
                mark.role = Role::Plain;
            }
        }
        marks[index].role = new;
        self.entries.set(track, Some(marks));
        Ok(RoleChange { role: new, was })
    }

    /// 文件改名后把跳转点跟过去，规则和收听进度相同（见 [`FileRecords::follow_renames`]）。
    /// 有搬动时返回 true。
    pub fn follow_renames(&mut self, tracks: &[Track], library_dir: &Path) -> bool {
        self.entries.follow_renames(tracks, library_dir)
    }

    fn from_raw(raw: RawStore) -> Self {
        let entries = raw
            .recordings
            .into_iter()
            .filter(|recording| !recording.marks.is_empty())
            .map(|recording| {
                let mut marks: Vec<(Mark, bool, bool)> = recording
                    .marks
                    .into_iter()
                    .map(|mark| {
                        let plain = Mark {
                            position: Duration::from_millis(mark.position_ms),
                            name: mark.name.filter(|name| !name.trim().is_empty()),
                            role: Role::Plain,
                        };
                        (plain, mark.start, mark.end)
                    })
                    .collect();
                // 文件可能被手动改过，不能假定已经排好序，也不能假定起点和终点合乎规则。
                marks.sort_by_key(|(mark, ..)| mark.position);
                let marks = settle_roles(marks);
                (
                    PathBuf::from(recording.path),
                    Stamped {
                        value: marks,
                        file_size: recording.file_size,
                        modified_ns: recording.modified_ns,
                    },
                )
            })
            .collect();
        Self {
            entries,
            read_only: false,
        }
    }
}

/// 按文件里的 `start`、`end` 定下各跳转点的角色：多个起点只认最早的，多个终点只认最晚的；
/// 同时标了两个的只算起点；终点不在起点（没有起点时是 0:00）之后就不算。
/// 去掉的只是标志，跳转点本身都留着。`marks` 已按位置排好序。
fn settle_roles(marks: Vec<(Mark, bool, bool)>) -> Vec<Mark> {
    let start = marks.iter().position(|(_, start, _)| *start);
    let start_at = start.map_or(Duration::ZERO, |start| marks[start].0.position);
    let end = marks
        .iter()
        .rposition(|(_, start, end)| *end && !*start)
        .filter(|end| marks[*end].0.position > start_at);
    marks
        .into_iter()
        .enumerate()
        .map(|(index, (mut mark, ..))| {
            mark.role = if Some(index) == start {
                Role::Start
            } else if Some(index) == end {
                Role::End
            } else {
                Role::Plain
            };
            mark
        })
        .collect()
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// 时长以内的跳转点。文件被剪短后，超出时长的跳转点留着，但不显示也不参与跳转。
pub fn within(marks: &[Mark], duration: Duration) -> &[Mark] {
    &marks[..marks.partition_point(|mark| mark.position < duration)]
}

/// 这些跳转点里的起点和终点。调用方先用 [`within`] 去掉超出时长的：
/// 文件被剪短后，落在时长之外的起点和终点和其他跳转点一样不算。
pub fn bounds(marks: &[Mark]) -> Bounds {
    let find = |role: Role| {
        marks
            .iter()
            .find(|mark| mark.role == role)
            .map(|mark| mark.position)
    };
    Bounds {
        start: find(Role::Start),
        end: find(Role::End),
    }
}

/// `position` 所在的那一段：不晚于它（留 [`SAME_SPOT`] 余量）的最后一个跳转点；
/// 还没到第一个跳转点时为 None。
pub fn section_at(marks: &[Mark], position: Duration) -> Option<usize> {
    marks
        .partition_point(|mark| mark.position <= position + SAME_SPOT)
        .checked_sub(1)
}

/// 往前跳的目标：不晚于 `position` 减 [`PREVIOUS_GRACE`] 的最后一个跳转点，
/// 所以刚跳到一个点之后再按会跳到再前面一个；前面没有时为 None，由调用方回到开头。
pub fn previous_target(marks: &[Mark], position: Duration) -> Option<usize> {
    marks
        .partition_point(|mark| mark.position + PREVIOUS_GRACE <= position)
        .checked_sub(1)
}

/// 往后跳的目标：晚于 `position` 加 [`SAME_SPOT`] 的第一个跳转点；后面没有时为 None。
pub fn next_target(marks: &[Mark], position: Duration) -> Option<usize> {
    let index = marks.partition_point(|mark| mark.position <= position + SAME_SPOT);
    (index < marks.len()).then_some(index)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    const SECOND: Duration = Duration::from_secs(1);
    const MINUTE: Duration = Duration::from_secs(60);

    fn recording(path: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            title: "x".to_owned(),
            artist: None,
            album: None,
            duration: Some(30 * MINUTE),
            format: None,
            track_number: None,
            disc_number: None,
            file_size: 7,
            modified_ns: 9,
        }
    }

    fn at(minutes: u32) -> Mark {
        Mark {
            position: MINUTE * minutes,
            name: None,
            role: Role::Plain,
        }
    }

    fn with_role(minutes: u32, role: Role) -> Mark {
        Mark {
            role,
            ..at(minutes)
        }
    }

    fn roles(store: &MarkStore, track: &Track) -> Vec<Role> {
        store
            .marks(&track.path)
            .iter()
            .map(|mark| mark.role)
            .collect()
    }

    fn positions(store: &MarkStore, track: &Track) -> Vec<Duration> {
        store
            .marks(&track.path)
            .iter()
            .map(|mark| mark.position)
            .collect()
    }

    #[test]
    fn marks_stay_sorted_and_a_second_mark_on_the_same_spot_is_refused() {
        let track = recording("/m/a.flac");
        let mut store = MarkStore::default();
        assert_eq!(store.add(&track, 20 * MINUTE), Ok(0));
        assert_eq!(store.add(&track, 5 * MINUTE), Ok(0));
        assert_eq!(store.add(&track, 12 * MINUTE), Ok(1));
        assert_eq!(
            positions(&store, &track),
            [5 * MINUTE, 12 * MINUTE, 20 * MINUTE]
        );

        // 不到 1 秒算同一处，不论在前还是在后；整整 1 秒就是另一处。
        let near = 12 * MINUTE + Duration::from_millis(999);
        assert_eq!(
            store.add(&track, near),
            Err(AddError::TooClose(12 * MINUTE))
        );
        let before = 12 * MINUTE - Duration::from_millis(400);
        assert_eq!(
            store.add(&track, before),
            Err(AddError::TooClose(12 * MINUTE))
        );
        assert_eq!(store.add(&track, 12 * MINUTE + SECOND), Ok(2));
    }

    #[test]
    fn positions_are_kept_to_the_millisecond() {
        let track = recording("/m/a.flac");
        let mut store = MarkStore::default();
        store
            .add(&track, Duration::from_nanos(754_250_999_999))
            .unwrap();
        assert_eq!(positions(&store, &track), [Duration::from_millis(754_250)]);
    }

    #[test]
    fn renaming_trims_and_an_empty_name_clears_it() {
        let track = recording("/m/a.flac");
        let mut store = MarkStore::default();
        store.add(&track, 5 * MINUTE).unwrap();
        assert!(store.rename(&track, 0, "  掏耳 "));
        assert_eq!(store.marks(&track.path)[0].name.as_deref(), Some("掏耳"));
        assert!(!store.rename(&track, 0, "掏耳"));
        assert!(store.rename(&track, 0, "   "));
        assert_eq!(store.marks(&track.path)[0].name, None);
        assert!(!store.rename(&track, 3, "没有这一个"));
    }

    #[test]
    fn removing_the_last_mark_drops_the_recording() {
        let track = recording("/m/a.flac");
        let mut store = MarkStore::default();
        store.add(&track, 5 * MINUTE).unwrap();
        store.add(&track, 9 * MINUTE).unwrap();
        assert_eq!(store.remove(&track, 0), Some(at(5)));
        assert_eq!(store.remove(&track, 1), None);
        assert_eq!(store.remove(&track, 0), Some(at(9)));
        assert!(store.entries.is_empty());
    }

    #[test]
    fn the_store_round_trips_through_the_file() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("data/marks.json");
        let mut store = MarkStore::default();
        let first = recording("/m/音频 - 第一段.flac");
        let second = recording("/m/音频 - 第二段.m4a");
        store.add(&first, Duration::from_millis(754_250)).unwrap();
        store.add(&first, 20 * MINUTE).unwrap();
        store.rename(&first, 1, "哄睡");
        store.add(&second, MINUTE).unwrap();
        store.save(&path).unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains("\"position_ms\": 754250"), "{contents}");
        // 没有名字时不写 name。
        assert_eq!(contents.matches("\"name\"").count(), 1, "{contents}");

        let (loaded, message) = MarkStore::load(&path);
        assert_eq!(message, None);
        assert_eq!(loaded.entries, store.entries);
    }

    #[test]
    fn a_hand_edited_file_is_sorted_and_empty_entries_are_dropped() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marks.json");
        fs::write(
            &path,
            r#"{"version":1,"recordings":[
                {"path":"/m/a.flac","file_size":7,"modified_ns":9,
                 "marks":[{"position_ms":600000,"name":" "},{"position_ms":60000,"name":"开头"}]},
                {"path":"/m/b.flac","file_size":7,"modified_ns":9,"marks":[]}
            ]}"#,
        )
        .unwrap();
        let (store, message) = MarkStore::load(&path);
        assert_eq!(message, None);
        let marks = store.marks(Path::new("/m/a.flac"));
        assert_eq!(
            marks,
            [
                Mark {
                    position: MINUTE,
                    name: Some("开头".to_owned()),
                    role: Role::Plain,
                },
                at(10)
            ]
        );
        assert!(store.marks(Path::new("/m/b.flac")).is_empty());
    }

    /// 在 1、5、10、20 分钟各有一个跳转点的录音。
    fn four_marks() -> (MarkStore, Track) {
        let track = recording("/m/a.flac");
        let mut store = MarkStore::default();
        for minutes in [1, 5, 10, 20] {
            store.add(&track, MINUTE * minutes).unwrap();
        }
        (store, track)
    }

    #[test]
    fn a_new_start_or_end_replaces_the_old_one_and_pressing_again_clears_it() {
        use Role::{End, Plain, Start};

        let (mut store, track) = four_marks();
        let set = |role, was| Ok(RoleChange { role, was });
        assert_eq!(store.toggle_role(&track, 1, Start), set(Start, Plain));
        assert_eq!(store.toggle_role(&track, 3, End), set(End, Plain));
        assert_eq!(roles(&store, &track), [Plain, Start, Plain, End]);
        // 另设一个起点：原来的变回普通跳转点。
        assert_eq!(store.toggle_role(&track, 0, Start), set(Start, Plain));
        assert_eq!(store.toggle_role(&track, 2, End), set(End, Plain));
        assert_eq!(roles(&store, &track), [Start, Plain, End, Plain]);
        // 再按一次取消。
        assert_eq!(store.toggle_role(&track, 0, Start), set(Plain, Start));
        assert_eq!(store.toggle_role(&track, 2, End), set(Plain, End));
        assert_eq!(roles(&store, &track), [Plain; 4]);
        assert_eq!(store.toggle_role(&track, 4, Start), Err(RoleError::Missing));
    }

    #[test]
    fn the_start_stays_before_the_end() {
        use Role::{End, Plain, Start};

        let (mut store, track) = four_marks();
        store.toggle_role(&track, 1, Start).unwrap();
        store.toggle_role(&track, 2, End).unwrap();
        assert_eq!(
            store.toggle_role(&track, 3, Start),
            Err(RoleError::StartNotBeforeEnd(10 * MINUTE))
        );
        assert_eq!(
            store.toggle_role(&track, 0, End),
            Err(RoleError::EndNotAfterStart(Some(5 * MINUTE)))
        );
        assert_eq!(roles(&store, &track), [Plain, Start, End, Plain]);

        // 起点直接改成终点：原来的起点让出来，不拿它自己比。
        assert_eq!(
            store.toggle_role(&track, 1, End),
            Ok(RoleChange {
                role: End,
                was: Start
            })
        );
        assert_eq!(roles(&store, &track), [Plain, End, Plain, Plain]);
        assert_eq!(
            store.toggle_role(&track, 1, Start),
            Ok(RoleChange {
                role: Start,
                was: End
            })
        );
        assert_eq!(roles(&store, &track), [Plain, Start, Plain, Plain]);

        // 没有起点时，终点也要在 0:00 之后。
        let track = recording("/m/b.flac");
        store.add(&track, Duration::ZERO).unwrap();
        assert_eq!(
            store.toggle_role(&track, 0, End),
            Err(RoleError::EndNotAfterStart(None))
        );
        assert_eq!(store.toggle_role(&track, 0, Start).unwrap().role, Start);
    }

    #[test]
    fn the_start_and_end_go_with_their_marks() {
        use Role::{End, Start};

        let (mut store, track) = four_marks();
        store.toggle_role(&track, 1, Start).unwrap();
        store.toggle_role(&track, 3, End).unwrap();
        store.rename(&track, 1, "正片");
        assert_eq!(store.marks(&track.path)[1].role, Start);
        assert_eq!(
            bounds(store.marks(&track.path)),
            Bounds {
                start: Some(5 * MINUTE),
                end: Some(20 * MINUTE)
            }
        );
        // 文件被剪短，终点落到时长之外：不算。
        assert_eq!(
            bounds(within(store.marks(&track.path), 15 * MINUTE)),
            Bounds {
                start: Some(5 * MINUTE),
                end: None
            }
        );

        assert_eq!(store.remove(&track, 3), Some(with_role(20, End)));
        assert_eq!(bounds(store.marks(&track.path)).end, None);

        let mut renamed = track.clone();
        renamed.path = PathBuf::from("/m/新名字.flac");
        assert!(store.follow_renames(std::slice::from_ref(&renamed), Path::new("/m")));
        assert_eq!(bounds(store.marks(&renamed.path)).start, Some(5 * MINUTE));
    }

    #[test]
    fn the_start_and_end_round_trip_and_plain_marks_get_no_flags() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marks.json");
        let (mut store, track) = four_marks();
        store.toggle_role(&track, 0, Role::Start).unwrap();
        store.toggle_role(&track, 3, Role::End).unwrap();
        store.save(&path).unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        assert_eq!(contents.matches("\"start\": true").count(), 1, "{contents}");
        assert_eq!(contents.matches("\"end\": true").count(), 1, "{contents}");
        assert!(!contents.contains("false"), "{contents}");
        let (loaded, message) = MarkStore::load(&path);
        assert_eq!(message, None);
        assert_eq!(loaded.entries, store.entries);
    }

    #[test]
    fn hand_edited_starts_and_ends_are_settled_on_load() {
        use Role::{End, Plain, Start};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marks.json");
        let recording = |path: &str, marks: &str| {
            format!(r#"{{"path":"{path}","file_size":7,"modified_ns":9,"marks":[{marks}]}}"#)
        };
        let recordings = [
            // 多个起点只认最早的，多个终点只认最晚的；文件里没排序也一样。
            recording(
                "/m/a.flac",
                r#"{"position_ms":600000,"start":true},{"position_ms":60000,"start":true},
                   {"position_ms":1200000,"end":true},{"position_ms":900000,"end":true}"#,
            ),
            // 同时标了两个的只算起点。
            recording(
                "/m/b.flac",
                r#"{"position_ms":60000,"start":true,"end":true},{"position_ms":600000}"#,
            ),
            // 终点不在起点之后：去掉终点，跳转点留着。
            recording(
                "/m/c.flac",
                r#"{"position_ms":60000,"end":true},{"position_ms":600000,"start":true}"#,
            ),
            // 没有起点时，0:00 上的终点也不算。
            recording("/m/d.flac", r#"{"position_ms":0,"end":true}"#),
        ];
        fs::write(
            &path,
            format!(r#"{{"version":1,"recordings":[{}]}}"#, recordings.join(",")),
        )
        .unwrap();
        let (store, message) = MarkStore::load(&path);
        assert_eq!(message, None);
        let roles = |path: &str| -> Vec<Role> {
            store
                .marks(Path::new(path))
                .iter()
                .map(|mark| mark.role)
                .collect()
        };
        assert_eq!(roles("/m/a.flac"), [Start, Plain, Plain, End]);
        assert_eq!(roles("/m/b.flac"), [Start, Plain]);
        assert_eq!(roles("/m/c.flac"), [Plain, Start]);
        assert_eq!(roles("/m/d.flac"), [Plain]);
    }

    #[test]
    fn a_missing_file_is_an_empty_store_and_an_unreadable_one_is_kept() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marks.json");
        let (store, message) = MarkStore::load(&path);
        assert!(store.entries.is_empty());
        assert_eq!(message, None);

        for contents in ["not json", r#"{"version":99,"recordings":[]}"#] {
            fs::write(&path, contents).unwrap();
            let (store, message) = MarkStore::load(&path);
            assert!(store.entries.is_empty());
            let message = message.unwrap();
            assert!(message.starts_with("跳转点文件无法识别"), "{message}");
            assert!(message.contains("marks.json.corrupt-"), "{message}");
            assert!(!path.exists(), "原文件应已改名");
        }
    }

    #[test]
    fn marks_follow_a_file_renamed_inside_the_library() {
        let mut store = MarkStore::default();
        let mut old = recording("/lib/A/旧名字.flac");
        old.file_size = 100;
        store.add(&old, 5 * MINUTE).unwrap();

        let mut renamed = old.clone();
        renamed.path = PathBuf::from("/lib/A/新名字（备注）.flac");
        assert!(store.follow_renames(std::slice::from_ref(&renamed), Path::new("/lib")));
        assert!(store.marks(&old.path).is_empty());
        assert_eq!(positions(&store, &renamed), [5 * MINUTE]);
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_cannot_be_saved_is_refused() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let mut odd = recording("/m/x.flac");
        odd.path = PathBuf::from(OsStr::from_bytes(b"/m/non-utf8-\xFF.flac"));
        let mut store = MarkStore::default();
        assert_eq!(store.add(&odd, 5 * MINUTE), Err(AddError::UnsupportedPath));
        assert!(store.entries.is_empty());
    }

    #[test]
    fn marks_past_the_duration_are_left_out() {
        let marks = [at(5), at(10), at(20)];
        assert_eq!(within(&marks, 20 * MINUTE), &marks[..2]);
        assert_eq!(within(&marks, 25 * MINUTE), &marks[..]);
        assert!(within(&marks, MINUTE).is_empty());
    }

    #[test]
    fn the_section_is_the_last_mark_reached_with_a_second_to_spare() {
        let marks = [at(5), at(10)];
        assert_eq!(section_at(&marks, 4 * MINUTE), None);
        // 跳转后位置可能比跳转点早一点。
        assert_eq!(
            section_at(&marks, 5 * MINUTE - Duration::from_millis(30)),
            Some(0)
        );
        assert_eq!(section_at(&marks, 9 * MINUTE), Some(0));
        assert_eq!(section_at(&marks, 60 * MINUTE), Some(1));
        assert_eq!(section_at(&[], 9 * MINUTE), None);
    }

    #[test]
    fn previous_skips_a_mark_just_passed_and_falls_back_to_the_start() {
        let marks = [at(5), at(10)];
        assert_eq!(previous_target(&marks, 12 * MINUTE), Some(1));
        assert_eq!(previous_target(&marks, 10 * MINUTE + 3 * SECOND), Some(1));
        assert_eq!(previous_target(&marks, 10 * MINUTE + 2 * SECOND), Some(0));
        assert_eq!(previous_target(&marks, 5 * MINUTE), None);
        assert_eq!(previous_target(&marks, SECOND), None);
        assert_eq!(previous_target(&[], 12 * MINUTE), None);
    }

    #[test]
    fn next_skips_the_mark_just_jumped_to_and_stops_after_the_last() {
        let marks = [at(5), at(10)];
        assert_eq!(next_target(&marks, Duration::ZERO), Some(0));
        assert_eq!(
            next_target(&marks, 5 * MINUTE - Duration::from_millis(30)),
            Some(1)
        );
        assert_eq!(next_target(&marks, 9 * MINUTE), Some(1));
        assert_eq!(next_target(&marks, 10 * MINUTE), None);
        assert_eq!(next_target(&[], Duration::ZERO), None);
    }

    /// 一条跳转点记录的 JSON：/m/旧.flac 在 1 分钟处有一个“开头”。
    const SAVED: &str = r#"{"version":1,"recordings":[{"path":"/m/旧.flac","file_size":7,"modified_ns":9,"marks":[{"position_ms":60000,"name":"开头"}]}]}"#;

    #[test]
    fn a_utf16_file_is_kept_aside_before_new_marks_are_written() {
        use crate::file_records::tests::{backups, utf16};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marks.json");
        fs::write(&path, utf16(SAVED)).unwrap();

        let (mut store, message) = MarkStore::load(&path);
        assert!(message.unwrap().contains("已改名保留"));
        let track = recording("/m/新.flac");
        store.add(&track, 5 * MINUTE).unwrap();
        store.save(&path).unwrap();

        let kept = backups(&path);
        assert_eq!(kept.len(), 1);
        assert_eq!(fs::read(&kept[0]).unwrap(), utf16(SAVED));
        let (loaded, _) = MarkStore::load(&path);
        assert_eq!(positions(&loaded, &track), [5 * MINUTE]);
    }

    #[cfg(unix)]
    #[test]
    fn marks_that_could_not_be_loaded_are_never_written_over() {
        use std::os::unix::fs::PermissionsExt;

        use crate::file_records::tests::utf16;

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("marks.json");
        let track = recording("/m/新.flac");

        // 读不了的文件：目录照样可写，没有防护就会被整个替换掉。
        fs::write(&path, SAVED).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        if fs::read(&path).is_err() {
            let (mut store, message) = MarkStore::load(&path);
            assert!(message.unwrap().contains("本次运行不保存跳转点"));
            assert_eq!(store.add(&track, 5 * MINUTE), Err(AddError::ReadOnly));
            store.save(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            assert_eq!(fs::read_to_string(&path).unwrap(), SAVED);
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        // 无法识别又没能改名保留：之后目录又能写了，也不覆盖。
        fs::write(&path, utf16(SAVED)).unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o555)).unwrap();
        if fs::write(temp.path().join("probe"), b"").is_err() {
            let (mut store, message) = MarkStore::load(&path);
            fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
            assert!(message.unwrap().contains("也无法改名保留"));
            assert_eq!(store.add(&track, 5 * MINUTE), Err(AddError::ReadOnly));
            store.save(&path).unwrap();
            assert_eq!(fs::read(&path).unwrap(), utf16(SAVED));
        }
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o755)).unwrap();
    }
}
