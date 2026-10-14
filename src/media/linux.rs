use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mpris_server::zbus::{Result as ZbusResult, fdo};
use mpris_server::{
    LoopStatus, Metadata, PlaybackStatus, PlayerInterface, Property, RootInterface, Server, Signal,
    Time, TrackId, Volume,
};
use percent_encoding::{AsciiSet, CONTROLS, percent_encode};

use crate::player::PlayState;
use crate::track::RepeatMode;

use super::{MediaCommand, MediaSnapshot};

// RFC 3986 path 中不允许的 ASCII 字符；非 ASCII 字节会由
// percent-encoding 自动转义。保留 `/` 才能继续表示路径层级。
const FILE_URI_ENCODE_SET: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

#[derive(Clone)]
struct Bridge {
    commands: Sender<MediaCommand>,
    snapshot: Arc<Mutex<MediaSnapshot>>,
}

impl Bridge {
    fn snapshot(&self) -> MediaSnapshot {
        self.snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn send(&self, command: MediaCommand) {
        let _ = self.commands.send(command);
    }
}

impl RootInterface for Bridge {
    async fn raise(&self) -> fdo::Result<()> {
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        self.send(MediaCommand::Quit);
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn set_fullscreen(&self, _fullscreen: bool) -> ZbusResult<()> {
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok("Music Player".to_owned())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("music-player".to_owned())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(Vec::new())
    }
}

impl PlayerInterface for Bridge {
    async fn next(&self) -> fdo::Result<()> {
        self.send(MediaCommand::Next);
        Ok(())
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.send(MediaCommand::Previous);
        Ok(())
    }

    async fn pause(&self) -> fdo::Result<()> {
        self.send(MediaCommand::Pause);
        Ok(())
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        self.send(MediaCommand::Toggle);
        Ok(())
    }

    async fn stop(&self) -> fdo::Result<()> {
        self.send(MediaCommand::Pause);
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        self.send(MediaCommand::Play);
        Ok(())
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        self.send(MediaCommand::SeekRelMicros(offset.as_micros()));
        Ok(())
    }

    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        if position.as_micros() < 0 {
            return Ok(());
        }
        self.send(MediaCommand::SeekTo {
            position: Duration::from_micros(position.as_micros() as u64),
            track_id: Some(track_id.to_string()),
        });
        Ok(())
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Ok(())
    }

    async fn playback_status(&self) -> fdo::Result<PlaybackStatus> {
        Ok(playback_status(&self.snapshot()))
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(loop_status(self.snapshot().repeat))
    }

    async fn set_loop_status(&self, loop_status: LoopStatus) -> ZbusResult<()> {
        self.send(MediaCommand::SetRepeat(repeat_mode(loop_status)));
        Ok(())
    }

    async fn rate(&self) -> fdo::Result<f64> {
        Ok(1.0)
    }

    async fn set_rate(&self, rate: f64) -> ZbusResult<()> {
        if rate == 0.0 {
            self.send(MediaCommand::Pause);
        }
        Ok(())
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().shuffle)
    }

    async fn set_shuffle(&self, shuffle: bool) -> ZbusResult<()> {
        self.send(MediaCommand::SetShuffle(shuffle));
        Ok(())
    }

    async fn metadata(&self) -> fdo::Result<Metadata> {
        Ok(metadata(&self.snapshot()))
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(reported_volume(&self.snapshot()))
    }

    async fn set_volume(&self, volume: Volume) -> ZbusResult<()> {
        let percent = (volume.clamp(0.0, 1.0) * 100.0).round() as u8;
        self.send(MediaCommand::SetVolume(percent));
        Ok(())
    }

    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_micros(duration_micros(self.snapshot().position)))
    }

    async fn minimum_rate(&self) -> fdo::Result<f64> {
        Ok(1.0)
    }

    async fn maximum_rate(&self) -> fdo::Result<f64> {
        Ok(1.0)
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_go_next)
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().can_go_previous)
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        let snapshot = self.snapshot();
        Ok(snapshot.status != PlayState::Stopped
            || !snapshot.title.is_empty()
            || snapshot.can_go_next)
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().status != PlayState::Stopped)
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(self.snapshot().status != PlayState::Stopped)
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(true)
    }
}

pub fn run(
    commands: Sender<MediaCommand>,
    snapshot: Arc<Mutex<MediaSnapshot>>,
    seeked: Receiver<Duration>,
    shutdown: Receiver<()>,
    ready: Sender<Result<(), String>>,
) {
    pollster::block_on(async move {
        let bridge = Bridge {
            commands,
            snapshot: Arc::clone(&snapshot),
        };
        let instance = format!("music_player.instance{}", std::process::id());
        let server = match Server::new("music_player", bridge.clone()).await {
            Ok(server) => server,
            Err(_) => match Server::new(&instance, bridge).await {
                Ok(server) => server,
                Err(error) => {
                    let _ = ready.send(Err(format!("无法注册 MPRIS: {error}")));
                    return;
                }
            },
        };
        let _ = ready.send(Ok(()));

        let mut last = MediaSnapshot::empty();
        loop {
            if shutdown.try_recv().is_ok() {
                break;
            }
            while let Ok(position) = seeked.try_recv() {
                let _ = server
                    .emit(Signal::Seeked {
                        position: Time::from_micros(duration_micros(position)),
                    })
                    .await;
            }
            let current = snapshot
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone();
            if let Some(changes) = changed_properties(&last, &current) {
                let _ = server.properties_changed(changes).await;
                last = current;
            }
            async_io::Timer::after(Duration::from_millis(50)).await;
        }
        let _ = server.release_bus_name().await;
    });
}

fn playback_status(snapshot: &MediaSnapshot) -> PlaybackStatus {
    match snapshot.status {
        PlayState::Playing => PlaybackStatus::Playing,
        PlayState::Paused => PlaybackStatus::Paused,
        PlayState::Stopped => PlaybackStatus::Stopped,
    }
}

/// MPRIS 没有“播完停止”，它和顺序播放一样报告为不循环。
fn loop_status(repeat: RepeatMode) -> LoopStatus {
    match repeat {
        RepeatMode::Stop | RepeatMode::None => LoopStatus::None,
        RepeatMode::One => LoopStatus::Track,
        RepeatMode::All => LoopStatus::Playlist,
    }
}

fn repeat_mode(status: LoopStatus) -> RepeatMode {
    match status {
        LoopStatus::None => RepeatMode::None,
        LoopStatus::Track => RepeatMode::One,
        LoopStatus::Playlist => RepeatMode::All,
    }
}

fn metadata(snapshot: &MediaSnapshot) -> Metadata {
    let track_id = TrackId::try_from(snapshot.track_id.as_str()).unwrap_or(TrackId::NO_TRACK);
    let mut builder = Metadata::builder()
        .trackid(track_id)
        .title(snapshot.title.clone());
    if let Some(artist) = &snapshot.artist {
        builder = builder.artist([artist.clone()]);
    }
    if let Some(album) = &snapshot.album {
        builder = builder.album(album.clone());
    }
    if let Some(duration) = snapshot.duration {
        builder = builder.length(Time::from_micros(duration_micros(duration)));
    }
    if let Some(path) = &snapshot.path {
        builder = builder.url(file_uri(path));
    }
    builder.build()
}

fn file_uri(path: &std::path::Path) -> String {
    use std::os::unix::ffi::OsStrExt;

    format!(
        "file://{}",
        percent_encode(path.as_os_str().as_bytes(), FILE_URI_ENCODE_SET)
    )
}

fn reported_volume(snapshot: &MediaSnapshot) -> Volume {
    if snapshot.muted {
        0.0
    } else {
        f64::from(snapshot.volume) / 100.0
    }
}

fn duration_micros(duration: Duration) -> i64 {
    i64::try_from(duration.as_micros()).unwrap_or(i64::MAX)
}

fn changed_properties(last: &MediaSnapshot, current: &MediaSnapshot) -> Option<Vec<Property>> {
    let mut changes = Vec::new();
    if last.status != current.status {
        changes.push(Property::PlaybackStatus(playback_status(current)));
    }
    if last.repeat != current.repeat {
        changes.push(Property::LoopStatus(loop_status(current.repeat)));
    }
    if last.shuffle != current.shuffle {
        changes.push(Property::Shuffle(current.shuffle));
    }
    if last.identity_changed(current) {
        changes.push(Property::Metadata(metadata(current)));
    }
    if last.volume != current.volume || last.muted != current.muted {
        changes.push(Property::Volume(reported_volume(current)));
    }
    if last.can_go_next != current.can_go_next {
        changes.push(Property::CanGoNext(current.can_go_next));
    }
    if last.can_go_previous != current.can_go_previous {
        changes.push(Property::CanGoPrevious(current.can_go_previous));
    }
    let last_can_pause = last.status != PlayState::Stopped;
    let can_pause = current.status != PlayState::Stopped;
    if last_can_pause != can_pause {
        changes.push(Property::CanPause(can_pause));
        changes.push(Property::CanSeek(can_pause));
    }
    if changes.is_empty() {
        None
    } else {
        Some(changes)
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn file_uri_percent_encodes_unicode_spaces_and_reserved_characters() {
        assert_eq!(
            file_uri(Path::new("/home/测试 用户/Music/a#b?.flac")),
            "file:///home/%E6%B5%8B%E8%AF%95%20%E7%94%A8%E6%88%B7/Music/a%23b%3F.flac"
        );
    }

    #[test]
    fn metadata_without_a_path_has_no_url() {
        let mut snapshot = MediaSnapshot::empty();
        snapshot.title = "正在播放".to_owned();
        assert_eq!(metadata(&snapshot).url(), None);

        snapshot.path = Some(std::path::PathBuf::from("/music/a.flac"));
        assert_eq!(
            metadata(&snapshot).url().as_deref(),
            Some("file:///music/a.flac")
        );
    }

    #[test]
    fn stop_after_track_is_reported_as_no_loop() {
        assert_eq!(loop_status(RepeatMode::Stop), LoopStatus::None);
        assert_eq!(loop_status(RepeatMode::None), LoopStatus::None);
        assert_eq!(repeat_mode(LoopStatus::Track), RepeatMode::One);
    }

    #[test]
    fn file_uri_preserves_non_utf8_unix_paths() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/music/non-utf8-\xFF.flac"));
        assert_eq!(file_uri(path), "file:///music/non-utf8-%FF.flac");
    }
}
