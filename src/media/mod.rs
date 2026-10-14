//! 系统媒体会话：命令进入 App，快照与 Seeked 离开 App。

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::player::PlayState;
use crate::track::RepeatMode;

#[derive(Debug, Clone, PartialEq)]
pub enum MediaCommand {
    Play,
    Pause,
    Toggle,
    Next,
    Previous,
    SeekRelMicros(i64),
    SeekTo {
        position: Duration,
        track_id: Option<String>,
    },
    SetVolume(u8),
    SetRepeat(RepeatMode),
    SetShuffle(bool),
    Quit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaEvent {
    Seeked { position: Duration },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSnapshot {
    pub status: PlayState,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub path: Option<PathBuf>,
    pub duration: Option<Duration>,
    pub position: Duration,
    pub volume: u8,
    pub muted: bool,
    pub repeat: RepeatMode,
    pub shuffle: bool,
    pub can_go_previous: bool,
    pub can_go_next: bool,
    pub track_id: String,
}

impl MediaSnapshot {
    pub fn empty() -> Self {
        Self {
            status: PlayState::Stopped,
            title: String::new(),
            artist: None,
            album: None,
            path: None,
            duration: None,
            position: Duration::ZERO,
            volume: 100,
            muted: false,
            repeat: RepeatMode::None,
            shuffle: false,
            can_go_previous: false,
            can_go_next: false,
            track_id: "/org/mpris/MediaPlayer2/TrackList/NoTrack".to_owned(),
        }
    }

    pub fn identity_changed(&self, other: &Self) -> bool {
        self.track_id != other.track_id
            || self.title != other.title
            || self.artist != other.artist
            || self.album != other.album
            || self.path != other.path
            || self.duration != other.duration
    }
}

pub struct MediaSession {
    commands: Receiver<MediaCommand>,
    snapshot: Arc<Mutex<MediaSnapshot>>,
    seeked_tx: Sender<Duration>,
    shutdown: Sender<()>,
    thread: Option<JoinHandle<()>>,
}

impl MediaSession {
    /// 启动平台媒体会话。注册失败返回 `Err`，播放器应继续运行。
    pub fn start() -> Result<Self, String> {
        let (command_tx, commands) = mpsc::channel();
        let (shutdown, shutdown_rx) = mpsc::channel();
        let (seeked_tx, seeked_rx) = mpsc::channel();
        let snapshot = Arc::new(Mutex::new(MediaSnapshot::empty()));
        let (ready_tx, ready_rx) = mpsc::channel();

        let thread_snapshot = Arc::clone(&snapshot);
        let thread = std::thread::Builder::new()
            .name("media-session".to_owned())
            .spawn(move || {
                platform::run(
                    command_tx,
                    thread_snapshot,
                    seeked_rx,
                    shutdown_rx,
                    ready_tx,
                );
            })
            .map_err(|error| format!("无法启动媒体会话线程: {error}"))?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                snapshot,
                seeked_tx,
                shutdown,
                thread: Some(thread),
            }),
            Ok(Err(error)) => Err(abandon_failed_worker(shutdown, thread, error)),
            Err(_) => Err(abandon_failed_worker(
                shutdown,
                thread,
                "媒体会话线程在注册完成前退出".to_owned(),
            )),
        }
    }

    pub fn try_recv(&self) -> Vec<MediaCommand> {
        let mut drained = Vec::new();
        while let Ok(command) = self.commands.try_recv() {
            drained.push(command);
        }
        drained
    }

    pub fn publish(&self, snapshot: MediaSnapshot) {
        if let Ok(mut slot) = self.snapshot.lock() {
            *slot = snapshot;
        }
    }

    pub fn notify_seeked(&self, position: Duration) {
        let _ = self.seeked_tx.send(position);
    }
}

impl Drop for MediaSession {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// 注册失败后必须先丢掉 shutdown sender 再 `join`。
/// 否则 worker 若在 `ready=Err` 之后仍 `recv` shutdown，双方会永久等待。
fn abandon_failed_worker(shutdown: Sender<()>, thread: JoinHandle<()>, error: String) -> String {
    drop(shutdown);
    let _ = thread.join();
    error
}

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod platform;

#[cfg(target_os = "windows")]
#[path = "windows.rs"]
mod platform;

#[cfg(not(any(target_os = "linux", target_os = "windows")))]
mod platform {
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{Arc, Mutex};

    use super::{MediaCommand, MediaSnapshot};

    pub fn run(
        _commands: Sender<MediaCommand>,
        _snapshot: Arc<Mutex<MediaSnapshot>>,
        _seeked: Receiver<std::time::Duration>,
        shutdown: Receiver<()>,
        ready: Sender<Result<(), String>>,
    ) {
        let _ = ready.send(Err("当前平台不支持系统媒体会话".to_owned()));
        let _ = shutdown.recv();
    }
}

#[cfg(test)]
mod tests {
    use super::abandon_failed_worker;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn failed_session_handshake_does_not_deadlock_when_worker_waits_for_shutdown() {
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
        let (shutdown, shutdown_rx) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let _ = ready_tx.send(Err("注册失败".to_owned()));
            let _ = shutdown_rx.recv();
        });
        let (done_tx, done_rx) = mpsc::channel();
        std::thread::spawn(move || {
            let error = match ready_rx.recv() {
                Ok(Err(error)) => abandon_failed_worker(shutdown, thread, error),
                other => panic!("unexpected handshake: {other:?}"),
            };
            let _ = done_tx.send(error);
        });
        let error = done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("注册失败握手发生死锁");
        assert_eq!(error, "注册失败");
    }
}
