//! 后台音乐库扫描和 SQLite 增量索引。

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lofty::file::{AudioFile, FileType, TaggedFileExt};
use lofty::probe::Probe;
use lofty::tag::Accessor;
use rusqlite::{Connection, ErrorCode, params};

use crate::track::Track;

pub const AUDIO_EXTENSIONS: &[&str] = &["mp3", "flac", "wav", "ogg", "oga", "m4a", "aac", "aiff"];

#[derive(Debug)]
pub enum LibraryEvent {
    ScanStarted,
    Progress {
        scanned: usize,
        found: usize,
    },
    ScanFinished {
        tracks: Vec<Track>,
        warnings: Vec<String>,
    },
    Warning(String),
    Error(String),
}

enum LibraryCommand {
    Scan,
    Shutdown,
}

pub struct LibraryWorker {
    commands: Sender<LibraryCommand>,
    events: Receiver<LibraryEvent>,
    thread: Option<JoinHandle<()>>,
    cancel: Arc<AtomicBool>,
}

impl LibraryWorker {
    pub fn start(root: PathBuf, database: PathBuf) -> Self {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let thread_cancel = Arc::clone(&cancel);
        let thread =
            thread::spawn(move || worker_loop(root, database, command_rx, event_tx, thread_cancel));
        let worker = Self {
            commands: command_tx,
            events: event_rx,
            thread: Some(thread),
            cancel,
        };
        worker.rescan();
        worker
    }

    pub fn rescan(&self) {
        let _ = self.commands.send(LibraryCommand::Scan);
    }

    pub fn drain_events(&self) -> Vec<LibraryEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }
}

impl Drop for LibraryWorker {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        let _ = self.commands.send(LibraryCommand::Shutdown);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn worker_loop(
    root: PathBuf,
    database: PathBuf,
    commands: Receiver<LibraryCommand>,
    events: Sender<LibraryEvent>,
    cancel: Arc<AtomicBool>,
) {
    let (mut connection, database_warning) = match open_database(&database) {
        Ok(connection) => connection,
        Err(error) => {
            let _ = events.send(LibraryEvent::Error(error));
            return;
        }
    };
    if let Some(warning) = database_warning {
        let _ = events.send(LibraryEvent::Warning(warning));
    }

    while let Ok(command) = commands.recv() {
        match command {
            LibraryCommand::Scan => {
                let _ = events.send(LibraryEvent::ScanStarted);
                match scan_library(&root, &mut connection, &events, &cancel) {
                    Ok((tracks, warnings)) => {
                        let _ = events.send(LibraryEvent::ScanFinished { tracks, warnings });
                    }
                    Err(error) => {
                        let _ = events.send(LibraryEvent::Error(error));
                    }
                }
            }
            LibraryCommand::Shutdown => break,
        }
    }
}

fn open_database(path: &Path) -> Result<(Connection, Option<String>), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "音乐索引路径没有父目录".to_owned())?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("无法创建缓存目录 {}: {error}", parent.display()))?;
    match initialize_database(path) {
        Ok(connection) => Ok((connection, None)),
        Err(error) if path.exists() && error.requires_rebuild() => {
            let backup = path.with_extension(format!("sqlite3.corrupt-{}", unix_nanos()));
            fs::rename(path, &backup).map_err(|rename_error| {
                format!(
                    "音乐索引已损坏（{error}），但无法把它移到 {}: {rename_error}",
                    backup.display()
                )
            })?;
            let connection = initialize_database(path)
                .map_err(|retry_error| format!("音乐索引损坏后无法重建: {retry_error}"))?;
            Ok((
                connection,
                Some(format!(
                    "检测到损坏的音乐索引，旧文件已保留为 {}，正在重新扫描",
                    backup.display()
                )),
            ))
        }
        Err(error) => Err(format!("无法打开音乐索引 {}: {error}", path.display())),
    }
}

#[derive(Debug)]
enum DatabaseInitError {
    Sqlite(rusqlite::Error),
    UnsupportedVersion(i64),
}

impl DatabaseInitError {
    fn requires_rebuild(&self) -> bool {
        match self {
            Self::UnsupportedVersion(_) => true,
            Self::Sqlite(rusqlite::Error::SqliteFailure(sqlite_error, _)) => matches!(
                sqlite_error.code,
                ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase
            ),
            Self::Sqlite(_) => false,
        }
    }
}

impl fmt::Display for DatabaseInitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sqlite(error) => error.fmt(formatter),
            Self::UnsupportedVersion(version) => {
                write!(formatter, "索引格式版本 {version} 不受支持")
            }
        }
    }
}

impl From<rusqlite::Error> for DatabaseInitError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Sqlite(error)
    }
}

const CACHE_VERSION: i64 = 2;

fn initialize_database(path: &Path) -> Result<Connection, DatabaseInitError> {
    let connection = Connection::open(path)?;
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if version > CACHE_VERSION {
        return Err(DatabaseInitError::UnsupportedVersion(version));
    }
    if version == 1 {
        // v1 没有音轨号和碟号，且增量扫描只在文件变动时重读标签，
        // 保留旧行会让专辑排序永远缺号；索引本就是可重建缓存，直接丢弃重扫。
        connection.execute_batch("DROP TABLE IF EXISTS tracks;")?;
    }
    connection.execute_batch(
        "PRAGMA journal_mode = WAL;
             PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS tracks (
                 path TEXT PRIMARY KEY NOT NULL,
                 root TEXT NOT NULL,
                 relative_path TEXT NOT NULL,
                 title TEXT NOT NULL,
                 artist TEXT,
                 album TEXT,
                 duration_ns INTEGER,
                 format TEXT,
                 track_number INTEGER,
                 disc_number INTEGER,
                 file_size INTEGER NOT NULL,
                 modified_ns INTEGER NOT NULL,
                 last_seen INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS tracks_root ON tracks(root);
             PRAGMA user_version = 2;",
    )?;
    Ok(connection)
}

fn scan_library(
    root: &Path,
    connection: &mut Connection,
    events: &Sender<LibraryEvent>,
    cancel: &AtomicBool,
) -> Result<(Vec<Track>, Vec<String>), String> {
    let root_text = root.to_string_lossy().into_owned();
    let cached = load_cached(connection, &root_text)?;
    let scan_id = unix_nanos();
    let mut warnings = Vec::new();
    let files = collect_audio_files(root, &mut warnings);
    let file_count = files.len();
    let mut tracks = Vec::with_capacity(files.len());

    let transaction = connection
        .transaction()
        .map_err(|error| format!("无法开始更新音乐索引: {error}"))?;
    for (index, path) in files.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("音乐库扫描已取消".to_owned());
        }
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                warnings.push(format!("无法读取 {}: {error}", path.display()));
                continue;
            }
        };
        let size = metadata.len();
        let modified_ns = modified_nanos(&metadata);
        let path_text = path.to_string_lossy().into_owned();
        let relative = path.strip_prefix(root).unwrap_or(&path).to_path_buf();

        let track = if let Some(cached) = cached.get(&path_text)
            && cached.file_size == size
            && cached.modified_ns == modified_ns
        {
            cached.clone()
        } else {
            match discover_track(path.clone(), relative, size, modified_ns) {
                Ok(track) => track,
                Err(error) => {
                    warnings.push(error);
                    continue;
                }
            }
        };

        upsert_track(&transaction, &root_text, &track, scan_id)?;
        tracks.push(track);
        if index % 25 == 0 {
            let _ = events.send(LibraryEvent::Progress {
                scanned: index + 1,
                found: tracks.len(),
            });
        }
    }

    transaction
        .execute(
            "DELETE FROM tracks WHERE root = ?1 AND last_seen <> ?2",
            params![root_text, scan_id],
        )
        .map_err(|error| format!("无法移除过期音乐索引: {error}"))?;
    transaction
        .commit()
        .map_err(|error| format!("无法保存音乐索引: {error}"))?;

    tracks.sort_by(|left, right| {
        let left_folded = left.relative_path.to_string_lossy().to_lowercase();
        let right_folded = right.relative_path.to_string_lossy().to_lowercase();
        left_folded
            .cmp(&right_folded)
            .then_with(|| left.relative_path.cmp(&right.relative_path))
    });
    let _ = events.send(LibraryEvent::Progress {
        scanned: file_count,
        found: tracks.len(),
    });
    Ok((tracks, warnings))
}

fn load_cached(connection: &Connection, root: &str) -> Result<HashMap<String, Track>, String> {
    let mut statement = connection
        .prepare(
            "SELECT path, relative_path, title, artist, album, duration_ns, format,
                    track_number, disc_number, file_size, modified_ns
             FROM tracks WHERE root = ?1",
        )
        .map_err(|error| format!("无法读取音乐索引: {error}"))?;
    let rows = statement
        .query_map([root], |row| {
            let path: String = row.get(0)?;
            let duration_ns: Option<i64> = row.get(5)?;
            let file_size: i64 = row.get(9)?;
            let track = Track {
                path: PathBuf::from(&path),
                relative_path: PathBuf::from(row.get::<_, String>(1)?),
                title: row.get(2)?,
                artist: row.get(3)?,
                album: row.get(4)?,
                duration: duration_ns.map(|value| Duration::from_nanos(value.max(0) as u64)),
                format: row.get(6)?,
                track_number: row
                    .get::<_, Option<i64>>(7)?
                    .map(|value| value.max(0) as u32),
                disc_number: row
                    .get::<_, Option<i64>>(8)?
                    .map(|value| value.max(0) as u32),
                file_size: file_size.max(0) as u64,
                modified_ns: row.get(10)?,
            };
            Ok((path, track))
        })
        .map_err(|error| format!("无法查询音乐索引: {error}"))?;

    rows.collect::<Result<HashMap<_, _>, _>>()
        .map_err(|error| format!("音乐索引记录损坏: {error}"))
}

fn upsert_track(
    connection: &Connection,
    root: &str,
    track: &Track,
    scan_id: i64,
) -> Result<(), String> {
    let duration_ns = track
        .duration
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64);
    connection
        .execute(
            "INSERT INTO tracks (
                 path, root, relative_path, title, artist, album, duration_ns, format,
                 track_number, disc_number, file_size, modified_ns, last_seen
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
             ON CONFLICT(path) DO UPDATE SET
                 root = excluded.root,
                 relative_path = excluded.relative_path,
                 title = excluded.title,
                 artist = excluded.artist,
                 album = excluded.album,
                 duration_ns = excluded.duration_ns,
                 format = excluded.format,
                 track_number = excluded.track_number,
                 disc_number = excluded.disc_number,
                 file_size = excluded.file_size,
                 modified_ns = excluded.modified_ns,
                 last_seen = excluded.last_seen",
            params![
                track.path.to_string_lossy(),
                root,
                track.relative_path.to_string_lossy(),
                track.title,
                track.artist,
                track.album,
                duration_ns,
                track.format,
                track.track_number,
                track.disc_number,
                track.file_size.min(i64::MAX as u64) as i64,
                track.modified_ns,
                scan_id,
            ],
        )
        .map_err(|error| format!("无法写入音乐索引: {error}"))?;
    Ok(())
}

fn discover_track(
    path: PathBuf,
    relative_path: PathBuf,
    file_size: u64,
    modified_ns: i64,
) -> Result<Track, String> {
    let probe = Probe::open(&path)
        .map_err(|error| format!("无法打开 {}: {error}", path.display()))?
        .guess_file_type()
        .map_err(|error| format!("无法识别 {}: {error}", path.display()))?;
    if let Some(file_type) = probe.file_type() {
        reject_library_file_type(file_type, &path)?;
    }

    let tagged = probe
        .read()
        .map_err(|error| format!("无法读取 {} 的媒体信息: {error}", path.display()))?;
    reject_library_file_type(tagged.file_type(), &path)?;

    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    let title = tag.and_then(non_empty_title).unwrap_or_else(|| {
        relative_path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("未知曲目")
            .to_owned()
    });
    let artist = tag.and_then(non_empty_artist);
    let album = tag.and_then(non_empty_album);
    let track_number = tag.and_then(Accessor::track);
    let disc_number = tag.and_then(Accessor::disk);
    let format = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_ascii_uppercase());

    Ok(Track {
        path,
        relative_path,
        title,
        artist,
        album,
        duration: Some(tagged.properties().duration()),
        format,
        track_number,
        disc_number,
        file_size,
        modified_ns,
    })
}

fn reject_library_file_type(file_type: FileType, path: &Path) -> Result<(), String> {
    match file_type {
        FileType::Opus => Err(format!("跳过暂不支持的 Opus 文件: {}", path.display())),
        FileType::Ape => Err(format!("跳过不支持的 APE 文件: {}", path.display())),
        _ => Ok(()),
    }
}

fn non_empty_title(tag: &lofty::tag::Tag) -> Option<String> {
    tag.title()
        .map(|value| value.into_owned())
        .filter(|value| !value.trim().is_empty())
}

fn non_empty_artist(tag: &lofty::tag::Tag) -> Option<String> {
    tag.artist()
        .map(|value| value.into_owned())
        .filter(|value| !value.trim().is_empty())
}

fn non_empty_album(tag: &lofty::tag::Tag) -> Option<String> {
    tag.album()
        .map(|value| value.into_owned())
        .filter(|value| !value.trim().is_empty())
}

fn collect_audio_files(root: &Path, warnings: &mut Vec<String>) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                warnings.push(format!("无法扫描目录 {}: {error}", directory.display()));
                continue;
            }
        };
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(format!("无法读取目录项: {error}"));
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    warnings.push(format!(
                        "无法读取 {} 的类型: {error}",
                        entry.path().display()
                    ));
                    continue;
                }
            };
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() && is_audio_file(&entry.path()) {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    files
}

pub fn is_audio_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| AUDIO_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn modified_nanos(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or_default()
}

fn unix_nanos() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos()
        .min(i64::MAX as u128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_extension_check_is_case_insensitive() {
        assert!(is_audio_file(Path::new("hello.FLAC")));
        assert!(is_audio_file(Path::new("hello.aiff")));
        assert!(!is_audio_file(Path::new("hello.opus")));
        assert!(!is_audio_file(Path::new("hello.ape")));
        assert!(!is_audio_file(Path::new("hello.wma")));
        assert!(!is_audio_file(Path::new("cover.jpg")));
    }

    #[cfg(unix)]
    #[test]
    fn collection_does_not_follow_directory_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("outside.mp3"), []).unwrap();
        symlink(outside.path(), temp.path().join("linked")).unwrap();
        fs::write(temp.path().join("inside.MP3"), []).unwrap();

        let mut warnings = Vec::new();
        let files = collect_audio_files(temp.path(), &mut warnings);
        assert_eq!(files, vec![temp.path().join("inside.MP3")]);
    }

    #[test]
    fn lofty_probe_uses_filename_when_tags_are_missing() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("无标签.wav");
        write_silent_wav(&path, 1);
        let track = discover_track(path.clone(), PathBuf::from("无标签.wav"), 1, 2).unwrap();
        assert_eq!(track.title, "无标签");
        assert!(track.artist.is_none());
        assert!(track.album.is_none());
        assert_eq!(track.format.as_deref(), Some("WAV"));
        assert!(track.duration.is_some());
    }

    #[test]
    fn a_renamed_file_shows_its_new_name_after_the_next_scan() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("music");
        fs::create_dir_all(root.join("ASMR(某人)")).unwrap();
        let old = root.join("ASMR(某人)/音频 - 旧名字.wav");
        write_silent_wav(&old, 1);
        let (mut connection, _) = open_database(&temp.path().join("library.sqlite3")).unwrap();
        let (events, _receiver) = mpsc::channel();
        let cancel = AtomicBool::new(false);
        let names = |tracks: &[Track]| {
            tracks
                .iter()
                .map(|track| track.display_title().to_owned())
                .collect::<Vec<_>>()
        };

        let (tracks, _) = scan_library(&root, &mut connection, &events, &cancel).unwrap();
        assert_eq!(names(&tracks), ["音频 - 旧名字"]);

        // 改名不改变文件大小和修改时间，旧路径的索引行必须被清掉，而不是沿用旧名字。
        fs::rename(&old, root.join("ASMR(某人)/音频 - 新名字（已裁剪）.wav")).unwrap();
        let (tracks, _) = scan_library(&root, &mut connection, &events, &cancel).unwrap();
        assert_eq!(names(&tracks), ["音频 - 新名字（已裁剪）"]);
        let cached = load_cached(&connection, &root.to_string_lossy()).unwrap();
        assert_eq!(cached.len(), 1);
        assert!(!cached.contains_key(&*old.to_string_lossy()));
    }

    fn write_silent_wav(path: &Path, sample_rate: u32) {
        let data = [0u8; 2];
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&(36u32 + data.len() as u32).to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16u32.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&1u16.to_le_bytes());
        wav.extend_from_slice(&sample_rate.to_le_bytes());
        wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
        wav.extend_from_slice(&2u16.to_le_bytes());
        wav.extend_from_slice(&16u16.to_le_bytes());
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&(data.len() as u32).to_le_bytes());
        wav.extend_from_slice(&data);
        fs::write(path, wav).unwrap();
    }

    #[test]
    fn sqlite_cache_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("cache/library.sqlite3");
        let (connection, warning) = open_database(&database).unwrap();
        assert!(warning.is_none());
        let track = Track {
            path: PathBuf::from("/music/song.flac"),
            relative_path: PathBuf::from("song.flac"),
            title: "Song".to_owned(),
            artist: Some("Artist".to_owned()),
            album: None,
            duration: Some(Duration::from_secs(42)),
            format: Some("FLAC".to_owned()),
            track_number: Some(3),
            disc_number: Some(1),
            file_size: 123,
            modified_ns: 456,
        };
        upsert_track(&connection, "/music", &track, 1).unwrap();
        let cached = load_cached(&connection, "/music").unwrap();
        assert_eq!(cached.get("/music/song.flac"), Some(&track));
    }

    #[test]
    fn corrupt_cache_is_preserved_and_rebuilt() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("library.sqlite3");
        fs::write(&database, b"not a sqlite database").unwrap();
        let (_connection, warning) = open_database(&database).unwrap();
        assert!(warning.unwrap().contains("旧文件已保留"));
        assert!(database.exists());
        let backups = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("corrupt"))
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn unsupported_cache_version_is_preserved_and_rebuilt() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("library.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection.pragma_update(None, "user_version", 99).unwrap();
        drop(connection);

        let (connection, warning) = open_database(&database).unwrap();
        assert!(warning.unwrap().contains("旧文件已保留"));
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CACHE_VERSION);
    }

    #[test]
    fn v1_cache_is_upgraded_in_place_and_reindexed() {
        let temp = tempfile::tempdir().unwrap();
        let database = temp.path().join("library.sqlite3");
        let connection = Connection::open(&database).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE tracks (path TEXT PRIMARY KEY NOT NULL, root TEXT NOT NULL);
                 PRAGMA user_version = 1;",
            )
            .unwrap();
        connection
            .execute("INSERT INTO tracks (path, root) VALUES ('/a.mp3', '/')", [])
            .unwrap();
        drop(connection);

        // 升级是静默的：不报损坏、不留备份文件，只是丢弃缓存行等待重扫。
        let (connection, warning) = open_database(&database).unwrap();
        assert!(warning.is_none());
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, CACHE_VERSION);
        let rows: i64 = connection
            .query_row("SELECT count(*) FROM tracks", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        let backups = fs::read_dir(temp.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains("corrupt"))
            .count();
        assert_eq!(backups, 0);
    }
}
