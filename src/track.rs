//! 曲目和播放顺序等核心数据模型。

use std::cmp::Ordering;
use std::path::{Component, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Track {
    pub path: PathBuf,
    pub relative_path: PathBuf,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration: Option<Duration>,
    pub format: Option<String>,
    pub track_number: Option<u32>,
    pub disc_number: Option<u32>,
    pub file_size: u64,
    pub modified_ns: i64,
}

impl Track {
    /// 界面上显示的名称：磁盘上的文件名（不含扩展名），原样显示。
    ///
    /// 不用标签标题：下载来的录音常带无意义的标题，用户写在文件名里的备注
    /// 也不会出现在标签中。文件名由用户自己维护，改名后下次扫描即生效。
    pub fn display_title(&self) -> &str {
        let stem = self
            .relative_path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty());
        match stem {
            Some(stem) => stem,
            None if !self.title.trim().is_empty() => &self.title,
            None => "未知曲目",
        }
    }

    /// 文件名已经包含在相对路径里，因此第一列放标签标题，让它照样能被搜到。
    pub fn searchable_columns(&self) -> [String; 4] {
        [
            self.title.clone(),
            self.artist.clone().unwrap_or_default(),
            self.album.clone().unwrap_or_default(),
            self.relative_path.to_string_lossy().into_owned(),
        ]
    }
}

/// 曲目所在的分组：音乐库根目录下的第一层文件夹，名称原样保留，不解析命名规律。
/// 更深的子文件夹归入它所在的第一层文件夹。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Folder {
    /// 直接放在音乐库根目录下的文件。
    Root,
    Named(String),
}

impl Folder {
    pub fn of(track: &Track) -> Self {
        let mut components = track.relative_path.components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(first)), Some(_)) => {
                Self::Named(first.to_string_lossy().into_owned())
            }
            _ => Self::Root,
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Root => "(根目录)",
            Self::Named(name) => name,
        }
    }
}

/// 曲目时长的显示形式：`m:ss`，超过一小时为 `h:mm:ss`。
pub fn fmt_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();
    let (hours, minutes, seconds) = (seconds / 3600, (seconds % 3600) / 60, seconds % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes}:{seconds:02}")
    }
}

/// 曲库列表的排序字段。播放顺序跟随列表顺序，因此它同时决定顺序播放的走向。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SortKey {
    #[default]
    Path,
    Title,
    Artist,
    Album,
    Duration,
}

impl SortKey {
    pub fn next(self) -> Self {
        match self {
            Self::Path => Self::Title,
            Self::Title => Self::Artist,
            Self::Artist => Self::Album,
            Self::Album => Self::Duration,
            Self::Duration => Self::Path,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Path => "路径",
            Self::Title => "标题",
            Self::Artist => "歌手",
            Self::Album => "专辑",
            Self::Duration => "时长",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortOrder {
    pub key: SortKey,
    pub descending: bool,
}

impl SortOrder {
    pub fn label(self) -> String {
        format!(
            "{} {}",
            self.key.label(),
            if self.descending { "↓" } else { "↑" }
        )
    }

    /// 缺失的字段永远排在最后，不随升降序翻转：把"未知歌手"翻到最前面
    /// 对使用者没有意义，只会把真正想看的内容推下去。
    pub fn compare(self, left: &Track, right: &Track) -> Ordering {
        let presence = match self.key {
            SortKey::Path | SortKey::Title => Ordering::Equal,
            SortKey::Artist => missing_last(left.artist.as_deref(), right.artist.as_deref()),
            SortKey::Album => missing_last(left.album.as_deref(), right.album.as_deref()),
            SortKey::Duration => missing_last(left.duration, right.duration),
        };
        if presence != Ordering::Equal {
            return presence;
        }
        let primary = match self.key {
            SortKey::Path => Ordering::Equal,
            SortKey::Title => fold(left.display_title()).cmp(&fold(right.display_title())),
            SortKey::Artist => {
                fold_opt(left.artist.as_deref()).cmp(&fold_opt(right.artist.as_deref()))
            }
            SortKey::Album => {
                fold_opt(left.album.as_deref()).cmp(&fold_opt(right.album.as_deref()))
            }
            SortKey::Duration => left.duration.cmp(&right.duration),
        };
        // 方向作用在整条比较链上，而不只是主键：按路径排序时主键恒等，
        // 只翻转主键会让降序完全失效。
        let ordering = primary.then_with(|| self.secondary(left, right));
        if self.descending {
            ordering.reverse()
        } else {
            ordering
        }
    }

    /// 主键相同时的固定次序，始终升序，保证同一份曲库每次排出的结果一致。
    fn secondary(self, left: &Track, right: &Track) -> Ordering {
        let grouped = match self.key {
            // 按路径排序时再引入专辑会打乱同一目录内的文件顺序。
            SortKey::Path | SortKey::Title | SortKey::Duration => Ordering::Equal,
            SortKey::Artist | SortKey::Album => {
                missing_last(left.album.as_deref(), right.album.as_deref())
                    .then_with(|| {
                        fold_opt(left.album.as_deref()).cmp(&fold_opt(right.album.as_deref()))
                    })
                    .then_with(|| left.disc_number.cmp(&right.disc_number))
                    .then_with(|| left.track_number.cmp(&right.track_number))
            }
        };
        grouped
            .then_with(|| path_fold(left).cmp(&path_fold(right)))
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    }
}

fn missing_last<T>(left: Option<T>, right: Option<T>) -> Ordering {
    match (left, right) {
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

fn fold(value: &str) -> String {
    value.to_lowercase()
}

fn fold_opt(value: Option<&str>) -> String {
    value.map(fold).unwrap_or_default()
}

fn path_fold(track: &Track) -> String {
    fold(&track.relative_path.to_string_lossy())
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepeatMode {
    /// 自然播完一首就停；手动切到下一首照常可用。
    Stop,
    /// 按列表顺序播到最后一首再停。
    #[default]
    None,
    All,
    One,
}

impl RepeatMode {
    pub fn next(self) -> Self {
        match self {
            Self::Stop => Self::None,
            Self::None => Self::All,
            Self::All => Self::One,
            Self::One => Self::Stop,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Stop => "播完停止",
            Self::None => "顺序",
            Self::All => "列表循环",
            Self::One => "单曲循环",
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackMode {
    pub repeat: RepeatMode,
    pub shuffle: bool,
}

impl PlaybackMode {
    pub fn label(self) -> String {
        if self.shuffle {
            format!("{} · 随机", self.repeat.label())
        } else {
            self.repeat.label().to_owned()
        }
    }
}

/// 仅用于读取 v1.1.0 及更早的 `play_mode` 字段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyPlayMode {
    Sequential,
    RepeatAll,
    RepeatOne,
    Shuffle,
}

impl LegacyPlayMode {
    pub fn into_playback_mode(self) -> PlaybackMode {
        match self {
            Self::Sequential => PlaybackMode {
                repeat: RepeatMode::None,
                shuffle: false,
            },
            Self::RepeatAll => PlaybackMode {
                repeat: RepeatMode::All,
                shuffle: false,
            },
            Self::RepeatOne => PlaybackMode {
                repeat: RepeatMode::One,
                shuffle: false,
            },
            Self::Shuffle => PlaybackMode {
                repeat: RepeatMode::None,
                shuffle: true,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: Option<&str>, album: Option<&str>, path: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            relative_path: PathBuf::from(path),
            title: title.to_owned(),
            artist: artist.map(str::to_owned),
            album: album.map(str::to_owned),
            duration: Some(Duration::from_secs(60)),
            format: Some("FLAC".to_owned()),
            track_number: None,
            disc_number: None,
            file_size: 1,
            modified_ns: 1,
        }
    }

    fn sorted(mut tracks: Vec<Track>, order: SortOrder) -> Vec<String> {
        tracks.sort_by(|left, right| order.compare(left, right));
        tracks
            .into_iter()
            .map(|track| track.relative_path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn display_title_is_the_file_stem_not_the_tag_title() {
        // 标签标题是下载工具留下的无意义名字，文件名才是用户看得懂的名称。
        let tagged = track(
            "studio_video_1761120035627.mp4",
            None,
            None,
            "ASMR(Other)/音频 - 涩涩ASMR.mp3",
        );
        assert_eq!(tagged.display_title(), "音频 - 涩涩ASMR");
        // 只去掉最后一个扩展名，文件名里的点原样保留。
        let dotted = track("x", None, None, "A/音频 - 甘やかし_ 9.10.m4a");
        assert_eq!(dotted.display_title(), "音频 - 甘やかし_ 9.10");
        let no_extension = track("x", None, None, "A/B/录音");
        assert_eq!(no_extension.display_title(), "录音");
    }

    #[test]
    fn the_folder_is_the_first_directory_under_the_library_root() {
        let folder = |path: &str| Folder::of(&track("x", None, None, path));
        assert_eq!(
            folder("ASMR(Other)_author)/音频 - 涩涩ASMR.mp3"),
            Folder::Named("ASMR(Other)_author)".to_owned())
        );
        // 更深的子文件夹归入第一层，名称不做任何解析。
        assert_eq!(
            folder("创作者/2026/录音.flac"),
            Folder::Named("创作者".to_owned())
        );
        assert_eq!(folder("根目录里的歌.flac"), Folder::Root);
        assert_eq!(Folder::Root.label(), "(根目录)");
    }

    #[test]
    fn display_title_falls_back_when_the_file_name_is_blank() {
        let mut blank = track("标签标题", None, None, "A/ .flac");
        assert_eq!(blank.display_title(), "标签标题");
        blank.title = " ".to_owned();
        assert_eq!(blank.display_title(), "未知曲目");
    }

    #[test]
    fn title_sort_is_case_insensitive_and_reverses_cleanly() {
        // 名称放在不同目录下，路径顺序（1、2、3）和名称顺序不同，
        // 排序结果只能来自名称；区分大小写时 "Banana" 会排到 "apple" 前面。
        let tracks = vec![
            track("x", None, None, "1/cherry.flac"),
            track("x", None, None, "2/Banana.flac"),
            track("x", None, None, "3/apple.flac"),
        ];
        let ascending = SortOrder {
            key: SortKey::Title,
            descending: false,
        };
        assert_eq!(
            sorted(tracks.clone(), ascending),
            ["3/apple.flac", "2/Banana.flac", "1/cherry.flac"]
        );
        assert_eq!(
            sorted(
                tracks,
                SortOrder {
                    descending: true,
                    ..ascending
                }
            ),
            ["1/cherry.flac", "2/Banana.flac", "3/apple.flac"]
        );
    }

    #[test]
    fn missing_values_stay_last_in_both_directions() {
        let tracks = vec![
            track("x", None, None, "unknown.flac"),
            track("x", Some("Beta"), None, "beta.flac"),
            track("x", Some("Alpha"), None, "alpha.flac"),
        ];
        let ascending = SortOrder {
            key: SortKey::Artist,
            descending: false,
        };
        assert_eq!(
            sorted(tracks.clone(), ascending),
            ["alpha.flac", "beta.flac", "unknown.flac"]
        );
        // 把“未知歌手”翻到最前面没有意义，降序也让它留在末尾。
        assert_eq!(
            sorted(
                tracks,
                SortOrder {
                    descending: true,
                    ..ascending
                }
            ),
            ["beta.flac", "alpha.flac", "unknown.flac"]
        );
    }

    #[test]
    fn album_sort_orders_by_disc_then_track_number() {
        let mut tracks = Vec::new();
        for (disc, number, name) in [
            (2, 1, "d2t1.flac"),
            (1, 10, "d1t10.flac"),
            (1, 2, "d1t2.flac"),
        ] {
            let mut item = track("x", Some("A"), Some("专辑"), name);
            item.disc_number = Some(disc);
            item.track_number = Some(number);
            tracks.push(item);
        }
        // 音轨号按数值比较：10 必须排在 2 之后，而不是按字符串排到前面。
        assert_eq!(
            sorted(
                tracks,
                SortOrder {
                    key: SortKey::Album,
                    descending: false
                }
            ),
            ["d1t2.flac", "d1t10.flac", "d2t1.flac"]
        );
    }

    #[test]
    fn path_sort_ignores_album_grouping() {
        let tracks = vec![
            track("x", Some("A"), Some("Z 专辑"), "01.flac"),
            track("x", Some("A"), Some("A 专辑"), "02.flac"),
        ];
        // 按路径排序时引入专辑分组会打乱同一目录内的文件顺序。
        assert_eq!(
            sorted(
                tracks,
                SortOrder {
                    key: SortKey::Path,
                    descending: false
                }
            ),
            ["01.flac", "02.flac"]
        );
    }

    #[test]
    fn descending_reverses_keys_whose_primary_value_is_always_equal() {
        let tracks = vec![
            track("x", None, None, "a.flac"),
            track("x", None, None, "b.flac"),
            track("x", None, None, "c.flac"),
        ];
        // 按路径排序的主键恒等，全靠次级顺序决定；降序必须照样翻转。
        assert_eq!(
            sorted(
                tracks,
                SortOrder {
                    key: SortKey::Path,
                    descending: true
                }
            ),
            ["c.flac", "b.flac", "a.flac"]
        );
    }

    #[test]
    fn duration_sort_puts_unknown_lengths_last() {
        let mut unknown = track("x", None, None, "unknown.flac");
        unknown.duration = None;
        let mut long = track("x", None, None, "long.flac");
        long.duration = Some(Duration::from_secs(600));
        let tracks = vec![unknown, long, track("x", None, None, "short.flac")];
        assert_eq!(
            sorted(
                tracks,
                SortOrder {
                    key: SortKey::Duration,
                    descending: false
                }
            ),
            ["short.flac", "long.flac", "unknown.flac"]
        );
    }
}
