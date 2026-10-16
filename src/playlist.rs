//! 独立 JSON 文件保存的命名播放列表。
//!
//! 每个列表一个文件，文件名是列表名逐字节的十六进制。条目可以是整首，也可以是文件里的一段（片段）；
//! 同一个文件可以出现多次。v1 只记路径，读到时转成整首条目，原文件不动，第一次改动这个列表时写成 v2。
//!
//! 改动先改内存再写盘。写不了时这个列表记为没存上，之后改动任何列表、以及退出时连同它一起再写；
//! 内存里的改动不撤回，重试也就不会被挡住。读不了的文件不会被覆盖：内容用不了的改名保留，
//! 文件本身读不了或者来自更新版本的留在原处，新建同名列表时不覆盖它。

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::config::atomic_write;
use crate::file_records::preserve_unreadable;
use crate::track::{RepeatMode, Track};

const VERSION: u32 = 2;

/// 列表在这次运行里的编号，不写盘。播放范围按它认列表，列表重新排序也不会认错。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PlaylistId(u64);

/// 条目在这次运行里的编号，不写盘。条目移动、增删之后，还能按它找到正在播放的那一条。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntryId(u64);

/// 条目在文件里占的一段。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    pub start: Duration,
    /// 结尾；None 表示放到文件结尾。
    pub end: Option<Duration>,
}

impl Range {
    /// 整首：从 0:00 放到文件结尾。
    pub const WHOLE: Self = Self {
        start: Duration::ZERO,
        end: None,
    };

    pub fn is_whole(&self) -> bool {
        *self == Self::WHOLE
    }
}

/// 文件大小和修改时间。文件改名后靠它们找回同一个文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub file_size: u64,
    pub modified_ns: i64,
}

impl Stamp {
    pub fn of(track: &Track) -> Self {
        Self {
            file_size: track.file_size,
            modified_ns: track.modified_ns,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: EntryId,
    pub path: PathBuf,
    pub range: Range,
    /// 条目的名字，片段和整首都可以有。
    pub name: Option<String>,
    /// 从 v1 转来、还没扫描到过的条目没有。
    pub stamp: Option<Stamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Playlist {
    pub id: PlaylistId,
    pub name: String,
    /// 在这个列表里播放时的循环方式。
    pub repeat: RepeatMode,
    pub entries: Vec<Entry>,
}

/// 一次改动生效之后写盘的结果。
#[must_use]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Saved {
    Yes,
    /// 没能写盘，这里是原因。改动留在内存里，之后改动任何列表或退出时再写。
    No(String),
}

/// 先只读版本号，再按版本解析。
#[derive(Deserialize)]
struct RawVersion {
    version: u32,
}

#[derive(Deserialize)]
struct RawV1 {
    name: String,
    tracks: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct RawPlaylist {
    version: u32,
    name: String,
    #[serde(default = "default_repeat")]
    repeat: RepeatMode,
    entries: Vec<RawEntry>,
}

/// 新建和从 v1 转来的列表按顺序播完整个列表后停止。
fn default_repeat() -> RepeatMode {
    RepeatMode::None
}

#[derive(Serialize, Deserialize)]
struct RawEntry {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    modified_ns: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

impl RawPlaylist {
    fn from_v1(v1: RawV1) -> Option<Self> {
        let entries = v1
            .tracks
            .into_iter()
            .map(|path| {
                Some(RawEntry {
                    path: path.into_os_string().into_string().ok()?,
                    file_size: None,
                    modified_ns: None,
                    start_ms: None,
                    end_ms: None,
                    name: None,
                })
            })
            .collect::<Option<_>>()?;
        Some(Self {
            version: VERSION,
            name: v1.name,
            repeat: default_repeat(),
            entries,
        })
    }

    /// 路径不是 UTF-8 时存不进 JSON，返回 None。加入条目时已经挡掉了这种路径。
    fn from_playlist(playlist: &Playlist) -> Option<Self> {
        let entries = playlist
            .entries
            .iter()
            .map(|entry| {
                Some(RawEntry {
                    path: entry.path.to_str()?.to_owned(),
                    file_size: entry.stamp.map(|stamp| stamp.file_size),
                    modified_ns: entry.stamp.map(|stamp| stamp.modified_ns),
                    start_ms: (!entry.range.start.is_zero()).then(|| millis(entry.range.start)),
                    end_ms: entry.range.end.map(millis),
                    name: entry.name.clone(),
                })
            })
            .collect::<Option<_>>()?;
        Some(Self {
            version: VERSION,
            name: playlist.name.clone(),
            repeat: playlist.repeat,
            entries,
        })
    }
}

impl RawEntry {
    /// 读进来的范围：没有开头从 0:00 开始，没有结尾放到文件结尾；
    /// 结尾不在开头之后的（多半是手改坏的）去掉结尾。
    fn range(&self) -> Range {
        let start = Duration::from_millis(self.start_ms.unwrap_or_default());
        let end = self
            .end_ms
            .map(Duration::from_millis)
            .filter(|end| *end > start);
        Range { start, end }
    }

    fn stamp(&self) -> Option<Stamp> {
        Some(Stamp {
            file_size: self.file_size?,
            modified_ns: self.modified_ns?,
        })
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

#[derive(Debug)]
pub struct PlaylistStore {
    directory: PathBuf,
    /// 按名字排序。
    playlists: Vec<Playlist>,
    /// 改了但还没存上的列表。
    unsaved: BTreeSet<PlaylistId>,
    next_id: u64,
    warnings: Vec<String>,
}

impl PlaylistStore {
    pub fn load(directory: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&directory)?;
        let mut store = Self {
            directory,
            playlists: Vec::new(),
            unsaved: BTreeSet::new(),
            next_id: 0,
            warnings: Vec::new(),
        };
        for entry in fs::read_dir(&store.directory)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    store
                        .warnings
                        .push(format!("无法读取播放列表目录项: {error}"));
                    continue;
                }
            };
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            match read_file(&path) {
                Ok(raw) => {
                    let playlist = store.adopt(raw);
                    store.playlists.push(playlist);
                }
                Err(warning) => store.warnings.push(warning),
            }
        }
        store.sort();
        Ok(store)
    }

    pub fn all(&self) -> &[Playlist] {
        &self.playlists
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn get(&self, id: PlaylistId) -> Option<&Playlist> {
        self.playlists.iter().find(|playlist| playlist.id == id)
    }

    pub fn create(&mut self, name: &str) -> Result<usize, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("播放列表名称不能为空".to_owned());
        }
        if self.playlists.iter().any(|playlist| playlist.name == name) {
            return Err(format!("播放列表“{name}”已经存在"));
        }
        // 同名的文件已经在了，却不在列表里：它读不了，或者来自更新的版本。不能覆盖它。
        let path = self.path_for(name);
        if !matches!(path.try_exists(), Ok(false)) {
            return Err(format!(
                "已有同名的播放列表文件 {}，但读不了；请先处理它",
                path.display()
            ));
        }
        let playlist = Playlist {
            id: self.next_playlist_id(),
            name: name.to_owned(),
            repeat: default_repeat(),
            entries: Vec::new(),
        };
        self.write(&playlist)?;
        let id = playlist.id;
        self.playlists.push(playlist);
        self.sort();
        Ok(self
            .playlists
            .iter()
            .position(|playlist| playlist.id == id)
            .unwrap_or_default())
    }

    /// 在列表末尾加一条，同一个文件可以加多次。路径存不进 JSON 时不加。
    pub fn add_entry(
        &mut self,
        index: usize,
        track: &Track,
        range: Range,
        name: Option<String>,
    ) -> Result<Saved, String> {
        if track.path.to_str().is_none() {
            return Err("文件路径含有无法保存的字符，不能加进播放列表".to_owned());
        }
        let id = EntryId(self.next_id());
        let playlist = self
            .playlists
            .get_mut(index)
            .ok_or_else(|| "播放列表不存在".to_owned())?;
        playlist.entries.push(Entry {
            id,
            path: track.path.clone(),
            range,
            name,
            stamp: Some(Stamp::of(track)),
        });
        let changed = playlist.id;
        Ok(self.changed(changed))
    }

    pub fn remove_entry(&mut self, index: usize, position: usize) -> Result<Saved, String> {
        let playlist = self
            .playlists
            .get_mut(index)
            .ok_or_else(|| "播放列表不存在".to_owned())?;
        if position >= playlist.entries.len() {
            return Err("播放列表项目不存在".to_owned());
        }
        playlist.entries.remove(position);
        let changed = playlist.id;
        Ok(self.changed(changed))
    }

    /// 改这个列表的循环方式。
    pub fn set_repeat(&mut self, id: PlaylistId, repeat: RepeatMode) -> Result<Saved, String> {
        let playlist = self
            .playlists
            .iter_mut()
            .find(|playlist| playlist.id == id)
            .ok_or_else(|| "播放列表不存在".to_owned())?;
        playlist.repeat = repeat;
        Ok(self.changed(id))
    }

    pub fn delete(&mut self, index: usize) -> Result<(), String> {
        let playlist = self
            .playlists
            .get(index)
            .ok_or_else(|| "播放列表不存在".to_owned())?;
        match fs::remove_file(self.path_for(&playlist.name)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => {
                return Err(format!("无法删除播放列表 {}: {error}", playlist.name));
            }
            _ => {}
        }
        let removed = self.playlists.remove(index);
        self.unsaved.remove(&removed.id);
        Ok(())
    }

    /// 退出时把没存上的列表再写一次。
    pub fn flush(&mut self) -> Result<(), String> {
        match self.save_unsaved() {
            Saved::Yes => Ok(()),
            Saved::No(error) => Err(error),
        }
    }

    /// 记下这个列表改过了，并把所有没存上的列表写一遍。
    fn changed(&mut self, id: PlaylistId) -> Saved {
        self.unsaved.insert(id);
        self.save_unsaved()
    }

    /// 写成的从“没存上”里去掉；写不了的留着下次再写，交出第一个的原因。
    fn save_unsaved(&mut self) -> Saved {
        let results: Vec<(PlaylistId, Result<(), String>)> = self
            .unsaved
            .iter()
            .map(|id| {
                let result = self
                    .playlists
                    .iter()
                    .find(|playlist| playlist.id == *id)
                    .map_or(Ok(()), |playlist| self.write(playlist));
                (*id, result)
            })
            .collect();
        let mut failure = None;
        for (id, result) in results {
            match result {
                Ok(()) => {
                    self.unsaved.remove(&id);
                }
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        failure.map_or(Saved::Yes, Saved::No)
    }

    fn write(&self, playlist: &Playlist) -> Result<(), String> {
        let raw = RawPlaylist::from_playlist(playlist).ok_or_else(|| {
            format!(
                "无法保存播放列表“{}”: 有文件路径含有无法保存的字符",
                playlist.name
            )
        })?;
        let json = serde_json::to_vec_pretty(&raw)
            .map_err(|error| format!("无法序列化播放列表: {error}"))?;
        atomic_write(&self.path_for(&playlist.name), &json)
            .map_err(|error| format!("无法保存播放列表“{}”: {error}", playlist.name))
    }

    /// 给读进来的列表和条目编上号。
    fn adopt(&mut self, raw: RawPlaylist) -> Playlist {
        let id = self.next_playlist_id();
        let entries = raw
            .entries
            .into_iter()
            .map(|entry| Entry {
                id: EntryId(self.next_id()),
                range: entry.range(),
                stamp: entry.stamp(),
                name: entry.name,
                path: PathBuf::from(entry.path),
            })
            .collect();
        Playlist {
            id,
            name: raw.name,
            repeat: raw.repeat,
            entries,
        }
    }

    fn sort(&mut self) {
        self.playlists
            .sort_by(|left, right| left.name.cmp(&right.name));
    }

    fn next_playlist_id(&mut self) -> PlaylistId {
        PlaylistId(self.next_id())
    }

    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    fn path_for(&self, name: &str) -> PathBuf {
        let encoded = name
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        self.directory.join(format!("{encoded}.json"))
    }
}

/// 读一个列表文件，v1 转成 v2 的样子。读不了时交出给用户的说明。
///
/// 内容用不了（不是 UTF-8、不是 JSON、缺字段）的改名保留；文件读不了、
/// 没能改名保留和来自更新版本的留在原处。
fn read_file(path: &Path) -> Result<RawPlaylist, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("无法读取播放列表 {}: {error}；文件留在原处", path.display()))?;
    let text = std::str::from_utf8(&bytes).ok();
    let version = text
        .and_then(|text| serde_json::from_str::<RawVersion>(text).ok())
        .map(|raw| raw.version);
    let parsed = match version {
        Some(1) => text
            .and_then(|text| serde_json::from_str::<RawV1>(text).ok())
            .and_then(RawPlaylist::from_v1),
        Some(VERSION) => text.and_then(|text| serde_json::from_str::<RawPlaylist>(text).ok()),
        Some(newer) if newer > VERSION => {
            return Err(format!(
                "播放列表 {} 的版本 {newer} 不受支持，已跳过，文件留在原处",
                path.display()
            ));
        }
        _ => None,
    };
    parsed.ok_or_else(|| match preserve_unreadable(path) {
        Ok(backup) => format!("播放列表文件无法识别，已改名保留为 {}", backup.display()),
        Err(error) => format!(
            "播放列表文件 {} 无法识别，也无法改名保留: {error}；文件留在原处",
            path.display()
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(path: PathBuf) -> Track {
        Track {
            relative_path: path.file_name().unwrap().into(),
            path,
            title: "Song".to_owned(),
            artist: None,
            album: None,
            duration: Some(Duration::from_secs(3600)),
            format: Some("FLAC".to_owned()),
            track_number: None,
            disc_number: None,
            file_size: 42,
            modified_ns: 7,
        }
    }

    fn clip(start: u64, end: Option<u64>) -> Range {
        Range {
            start: Duration::from_secs(start),
            end: end.map(Duration::from_secs),
        }
    }

    fn json_files(directory: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        files.sort();
        files
    }

    /// 列表的内容，不看这次运行里的编号。
    fn contents(playlist: &Playlist) -> Vec<(PathBuf, Range, Option<String>, Option<Stamp>)> {
        playlist
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.path.clone(),
                    entry.range,
                    entry.name.clone(),
                    entry.stamp,
                )
            })
            .collect()
    }

    #[test]
    fn create_add_reload_and_delete_never_touch_music() {
        let temp = tempfile::tempdir().unwrap();
        let music = temp.path().join("song.flac");
        let missing = temp.path().join("later-missing.flac");
        fs::write(&music, b"not real audio").unwrap();
        fs::write(&missing, b"not real audio").unwrap();
        let directory = temp.path().join("playlists");
        let mut store = PlaylistStore::load(directory.clone()).unwrap();
        let index = store.create("我的/列表").unwrap();
        for path in [&music, &missing, &music] {
            assert_eq!(
                store
                    .add_entry(index, &track(path.clone()), Range::WHOLE, None)
                    .unwrap(),
                Saved::Yes
            );
        }

        fs::remove_file(&missing).unwrap();

        let mut reloaded = PlaylistStore::load(directory).unwrap();
        let paths: Vec<&PathBuf> = reloaded.all()[0]
            .entries
            .iter()
            .map(|entry| &entry.path)
            .collect();
        assert_eq!(paths, [&music, &missing, &music]);
        reloaded.delete(0).unwrap();
        assert!(reloaded.all().is_empty());
        assert!(music.exists());
        assert!(!missing.exists());
    }

    #[test]
    fn name_conflicts_are_not_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        store.create("Favorites").unwrap();
        assert!(store.create("Favorites").is_err());
    }

    #[test]
    fn clips_names_stamps_and_repeat_survive_a_reload() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        let index = store.create("睡前").unwrap();
        let long = track(PathBuf::from("/music/A.m4a"));
        let _ = store
            .add_entry(index, &long, clip(750, Some(1820)), Some("掏耳".to_owned()))
            .unwrap();
        let _ = store
            .add_entry(index, &long, clip(1820, None), None)
            .unwrap();
        let _ = store
            .add_entry(
                index,
                &track(PathBuf::from("/music/C.flac")),
                Range::WHOLE,
                None,
            )
            .unwrap();
        store.playlists[index].repeat = RepeatMode::All;
        store.unsaved.insert(store.playlists[index].id);
        store.flush().unwrap();

        let reloaded = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.all().len(), 1);
        assert_eq!(reloaded.all()[0].name, "睡前");
        assert_eq!(reloaded.all()[0].repeat, RepeatMode::All);
        assert_eq!(contents(&reloaded.all()[0]), contents(&store.all()[0]));

        // 整首不写范围和名字，开头在 0:00 的片段不写开头。
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(&json_files(temp.path())[0]).unwrap()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["repeat"], "all");
        let entries = json["entries"].as_array().unwrap();
        assert_eq!(entries[0]["start_ms"], 750_000);
        assert_eq!(entries[0]["end_ms"], 1_820_000);
        assert_eq!(entries[0]["name"], "掏耳");
        assert_eq!(entries[0]["file_size"], 42);
        assert_eq!(entries[0]["modified_ns"], 7);
        assert!(entries[1].get("end_ms").is_none());
        let whole = entries[2].as_object().unwrap();
        let mut keys: Vec<&str> = whole.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["file_size", "modified_ns", "path"]);
    }

    #[test]
    fn a_v1_playlist_reads_as_whole_entries_and_becomes_v2_on_the_first_change() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("e79da1e5898d.json");
        let v1 = r#"{"version":1,"name":"睡前","tracks":["/music/A.m4a","/music/B.flac"]}"#;
        fs::write(&file, v1).unwrap();

        let mut store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        assert!(store.warnings().is_empty(), "{:?}", store.warnings());
        let playlist = &store.all()[0];
        assert_eq!(playlist.repeat, RepeatMode::None);
        assert_eq!(
            contents(playlist),
            [
                (PathBuf::from("/music/A.m4a"), Range::WHOLE, None, None),
                (PathBuf::from("/music/B.flac"), Range::WHOLE, None, None),
            ]
        );
        // 只是读，文件不动：没改过的列表旧版本照样能读。
        assert_eq!(fs::read_to_string(&file).unwrap(), v1);

        let _ = store
            .add_entry(
                0,
                &track(PathBuf::from("/music/C.flac")),
                Range::WHOLE,
                None,
            )
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        assert_eq!(json["version"], 2);
        assert_eq!(json["repeat"], "none");
        assert_eq!(json["entries"].as_array().unwrap().len(), 3);
        assert_eq!(json_files(temp.path()), [file]);
    }

    #[test]
    fn a_playlist_from_a_newer_version_is_left_alone_and_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("e79da1e5898d.json");
        let newer = r#"{"version":3,"name":"睡前","items":[]}"#;
        fs::write(&file, newer).unwrap();

        let mut store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        assert!(store.all().is_empty());
        assert!(
            store.warnings()[0].contains("版本 3 不受支持"),
            "{:?}",
            store.warnings()
        );
        let error = store.create("睡前").unwrap_err();
        assert!(error.contains("请先处理它"), "{error}");
        assert_eq!(fs::read_to_string(&file).unwrap(), newer);
        assert_eq!(json_files(temp.path()), [file]);
    }

    #[test]
    fn unusable_playlist_files_are_kept_under_a_new_name() {
        for contents in [
            b"{ not json".as_slice(),
            &[0xFF, 0xFE, b'{', 0, b'}', 0],
            br#"{"version":2,"name":"x"}"#,
            br#"{"version":1,"name":"x","tracks":"oops"}"#,
            br#"{"version":0,"name":"x","entries":[]}"#,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let file = temp.path().join("78.json");
            fs::write(&file, contents).unwrap();

            let mut store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
            assert!(store.all().is_empty());
            assert!(
                store.warnings()[0].contains("已改名保留"),
                "{:?}",
                store.warnings()
            );
            let kept = json_files(temp.path());
            assert_eq!(kept.len(), 1);
            assert!(
                kept[0].to_string_lossy().contains("78.json.corrupt-"),
                "{kept:?}"
            );
            assert_eq!(fs::read(&kept[0]).unwrap(), contents);
            // 改名保留之后，同名的列表可以照常新建，下次启动也不会再读到旧文件。
            store.create("x").unwrap();
            let reloaded = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
            assert!(reloaded.warnings().is_empty(), "{:?}", reloaded.warnings());
            assert_eq!(reloaded.all().len(), 1);
        }
    }

    #[test]
    fn hand_edited_ranges_follow_the_reading_rules() {
        let temp = tempfile::tempdir().unwrap();
        let entries = r#"[
            {"path":"/m/a","start_ms":0,"end_ms":5000},
            {"path":"/m/b","start_ms":9000,"end_ms":9000},
            {"path":"/m/c","start_ms":9000,"end_ms":3000},
            {"path":"/m/d","start_ms":0},
            {"path":"/m/e","file_size":1}
        ]"#;
        fs::write(
            temp.path().join("78.json"),
            format!(r#"{{"version":2,"name":"x","entries":{entries}}}"#),
        )
        .unwrap();
        let store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        let playlist = &store.all()[0];
        // 没写循环方式时是顺序。
        assert_eq!(playlist.repeat, RepeatMode::None);
        let ranges: Vec<Range> = playlist.entries.iter().map(|entry| entry.range).collect();
        assert_eq!(
            ranges,
            [
                clip(0, Some(5)),
                clip(9, None),
                clip(9, None),
                Range::WHOLE,
                Range::WHOLE
            ]
        );
        // 大小和修改时间缺一个就当没有。
        assert_eq!(playlist.entries[4].stamp, None);
    }

    #[test]
    fn a_change_that_could_not_be_saved_is_kept_and_saved_with_the_next_one() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        let index = store.create("x").unwrap();
        let file = json_files(temp.path())[0].clone();
        // 文件的位置被一个目录占住，写盘会失败。
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();

        let first = track(PathBuf::from("/music/a.flac"));
        let saved = store.add_entry(index, &first, Range::WHOLE, None).unwrap();
        assert!(matches!(&saved, Saved::No(error) if error.contains("无法保存播放列表“x”")));
        // 改动留在内存里；同一首再加一次不会被挡住（BUG-010）。
        assert_eq!(store.all()[0].entries.len(), 1);

        fs::remove_dir(&file).unwrap();
        let second = track(PathBuf::from("/music/b.flac"));
        assert_eq!(
            store.add_entry(index, &second, Range::WHOLE, None).unwrap(),
            Saved::Yes
        );
        let reloaded = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        assert_eq!(reloaded.all()[0].entries.len(), 2);

        // 退出时也会再写一次。
        fs::remove_file(&file).unwrap();
        fs::create_dir(&file).unwrap();
        assert!(matches!(
            store.remove_entry(index, 0).unwrap(),
            Saved::No(_)
        ));
        assert!(store.flush().is_err());
        fs::remove_dir(&file).unwrap();
        store.flush().unwrap();
        let reloaded = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        assert_eq!(
            contents(&reloaded.all()[0]),
            [(
                second.path.clone(),
                Range::WHOLE,
                None,
                Some(Stamp::of(&second))
            )]
        );
    }

    #[test]
    fn deleting_a_playlist_whose_file_is_already_gone_still_removes_it() {
        let temp = tempfile::tempdir().unwrap();
        let mut store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        store.create("x").unwrap();
        fs::remove_file(&json_files(temp.path())[0]).unwrap();
        store.delete(0).unwrap();
        assert!(store.all().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_refused() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().unwrap();
        let mut store = PlaylistStore::load(temp.path().to_path_buf()).unwrap();
        let index = store.create("x").unwrap();
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(b"/music/song\xFF.wav"));
        let error = store
            .add_entry(index, &track(path), Range::WHOLE, None)
            .unwrap_err();
        assert!(error.contains("无法保存的字符"), "{error}");
        assert!(store.all()[0].entries.is_empty());
    }
}
