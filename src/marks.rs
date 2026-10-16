//! 长录音各自的跳转点：听的时候记下的位置，之后可以直接跳回去。
//!
//! 和收听进度一样是用户数据，按文件记、跟着改名走，但单独存一份文件：
//! 进度拖回开头时整条记录会删掉，跳转点不能跟着没了。只给长录音设跳转点由调用方把关。
//! 跳转点按位置排序，精确到毫秒；没有名字时界面显示时间。

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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddError {
    /// 离这个已有的跳转点不到 [`SAME_SPOT`]。
    TooClose(Duration),
    /// 路径不是 UTF-8，存不进 JSON。
    UnsupportedPath,
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
}

#[derive(Debug, Default)]
pub struct MarkStore {
    /// 每个录音的跳转点按位置排序，且不为空：删光时整条记录删掉。
    entries: FileRecords<Vec<Mark>>,
}

impl MarkStore {
    /// 读取跳转点文件；还没有时是空记录。读不了或格式不对时把文件改名保留，
    /// 返回空记录和一句说明。
    pub fn load(path: &Path) -> (Self, Option<String>) {
        let (raw, message) =
            file_records::load_json(path, WHAT, |raw: &RawStore| raw.version == MARKS_VERSION);
        (raw.map(Self::from_raw).unwrap_or_default(), message)
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
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
                let mut marks: Vec<Mark> = recording
                    .marks
                    .into_iter()
                    .map(|mark| Mark {
                        position: Duration::from_millis(mark.position_ms),
                        name: mark.name.filter(|name| !name.trim().is_empty()),
                    })
                    .collect();
                // 文件可能被手动改过，不能假定已经排好序。
                marks.sort_by_key(|mark| mark.position);
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
        Self { entries }
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// 时长以内的跳转点。文件被剪短后，超出时长的跳转点留着，但不显示也不参与跳转。
pub fn within(marks: &[Mark], duration: Duration) -> &[Mark] {
    &marks[..marks.partition_point(|mark| mark.position < duration)]
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
        }
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
                    name: Some("开头".to_owned())
                },
                at(10)
            ]
        );
        assert!(store.marks(Path::new("/m/b.flac")).is_empty());
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
}
