use std::mem;
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Foundation::{TimeSpan, TypedEventHandler};
use windows::Media::{
    AutoRepeatModeChangeRequestedEventArgs, MediaPlaybackAutoRepeatMode, MediaPlaybackStatus,
    MediaPlaybackType, PlaybackPositionChangeRequestedEventArgs,
    ShuffleEnabledChangeRequestedEventArgs, SystemMediaTransportControls,
    SystemMediaTransportControlsButton, SystemMediaTransportControlsButtonPressedEventArgs,
    SystemMediaTransportControlsTimelineProperties,
};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::WinRT::{
    ISystemMediaTransportControlsInterop, RO_INIT_SINGLETHREADED, RoInitialize, RoUninitialize,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CS_HREDRAW, CS_VREDRAW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, MSG,
    PM_REMOVE, PeekMessageW, PostMessageW, RegisterClassExW, TranslateMessage, WM_DESTROY, WM_QUIT,
    WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_POPUP,
};
use windows::core::{HSTRING, w};

use crate::player::PlayState;
use crate::track::RepeatMode;

use super::{MediaCommand, MediaSnapshot};

pub fn run(
    commands: Sender<MediaCommand>,
    snapshot: Arc<Mutex<MediaSnapshot>>,
    seeked: Receiver<Duration>,
    shutdown: Receiver<()>,
    ready: Sender<Result<(), String>>,
) {
    let mut runtime_ready = false;
    let mut hwnd = HWND(std::ptr::null_mut());
    let result = (|| -> Result<SystemMediaTransportControls, String> {
        unsafe { RoInitialize(RO_INIT_SINGLETHREADED) }
            .map_err(|error| format!("无法初始化 Windows Runtime: {error}"))?;
        runtime_ready = true;
        hwnd = unsafe { create_hidden_window() }?;
        let interop = windows::core::factory::<
            SystemMediaTransportControls,
            ISystemMediaTransportControlsInterop,
        >()
        .map_err(|error| format!("无法创建 SMTC interop: {error}"))?;
        let controls: SystemMediaTransportControls = unsafe { interop.GetForWindow(hwnd) }
            .map_err(|error| format!("无法绑定 SMTC: {error}"))?;
        setup_controls(&controls, commands.clone())
            .map_err(|error| format!("无法配置 SMTC: {error}"))?;
        Ok(controls)
    })();

    let controls = match result {
        Ok(controls) => {
            let _ = ready.send(Ok(()));
            controls
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            teardown_runtime(hwnd, runtime_ready);
            return;
        }
    };

    let mut last = MediaSnapshot::empty();
    let mut last_timeline = Instant::now()
        .checked_sub(Duration::from_secs(1))
        .unwrap_or_else(Instant::now);
    loop {
        if shutdown.try_recv().is_ok() {
            break;
        }
        while seeked.try_recv().is_ok() {
            let current = read_snapshot(&snapshot);
            let _ = publish_timeline(&controls, &current);
            last_timeline = Instant::now();
        }
        unsafe {
            pump_messages();
        }
        let current = read_snapshot(&snapshot);
        let identity_changed = last.identity_changed(&current);
        let status_changed = last.status != current.status
            || last.repeat != current.repeat
            || last.shuffle != current.shuffle
            || last.can_go_next != current.can_go_next
            || last.can_go_previous != current.can_go_previous;
        if identity_changed {
            let _ = publish_metadata(&controls, &current);
        }
        if identity_changed || status_changed {
            let _ = publish_status(&controls, &current);
            let _ = publish_timeline(&controls, &current);
            last = current;
            last_timeline = Instant::now();
        } else if last_timeline.elapsed() >= Duration::from_millis(750) {
            let _ = publish_timeline(&controls, &current);
            last.position = current.position;
            last_timeline = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(16));
    }

    let _ = controls.SetIsEnabled(false);
    drop(controls);
    teardown_runtime(hwnd, true);
}

fn teardown_runtime(hwnd: HWND, runtime_ready: bool) {
    unsafe {
        if !hwnd.0.is_null() {
            let _ = DestroyWindow(hwnd);
            pump_messages();
        }
        if runtime_ready {
            RoUninitialize();
        }
    }
}

fn setup_controls(
    controls: &SystemMediaTransportControls,
    commands: Sender<MediaCommand>,
) -> windows::core::Result<()> {
    controls.SetIsEnabled(true)?;
    controls.SetIsPlayEnabled(true)?;
    controls.SetIsPauseEnabled(true)?;
    controls.SetIsStopEnabled(false)?;
    controls.SetIsNextEnabled(true)?;
    controls.SetIsPreviousEnabled(true)?;
    controls.SetIsFastForwardEnabled(true)?;
    controls.SetIsRewindEnabled(true)?;
    controls
        .DisplayUpdater()?
        .SetType(MediaPlaybackType::Music)?;

    let button_commands = commands.clone();
    controls.ButtonPressed(&TypedEventHandler::new(
        move |_, args: windows::core::Ref<SystemMediaTransportControlsButtonPressedEventArgs>| {
            let args = args.ok()?;
            let button = args.Button()?;
            let command = if button == SystemMediaTransportControlsButton::Play {
                Some(MediaCommand::Play)
            } else if button == SystemMediaTransportControlsButton::Pause {
                Some(MediaCommand::Pause)
            } else if button == SystemMediaTransportControlsButton::Next {
                Some(MediaCommand::Next)
            } else if button == SystemMediaTransportControlsButton::Previous {
                Some(MediaCommand::Previous)
            } else if button == SystemMediaTransportControlsButton::FastForward {
                Some(MediaCommand::SeekRelMicros(10_000_000))
            } else if button == SystemMediaTransportControlsButton::Rewind {
                Some(MediaCommand::SeekRelMicros(-10_000_000))
            } else {
                None
            };
            if let Some(command) = command {
                let _ = button_commands.send(command);
            }
            Ok(())
        },
    ))?;

    let position_commands = commands.clone();
    controls.PlaybackPositionChangeRequested(&TypedEventHandler::new(
        move |_, args: windows::core::Ref<PlaybackPositionChangeRequestedEventArgs>| {
            let args = args.ok()?;
            let span = args.RequestedPlaybackPosition()?;
            let _ = position_commands.send(MediaCommand::SeekTo {
                position: timespan_to_duration(span),
                track_id: None,
            });
            Ok(())
        },
    ))?;

    let repeat_commands = commands.clone();
    controls.AutoRepeatModeChangeRequested(&TypedEventHandler::new(
        move |_, args: windows::core::Ref<AutoRepeatModeChangeRequestedEventArgs>| {
            let args = args.ok()?;
            let mode = args.RequestedAutoRepeatMode()?;
            let _ = repeat_commands.send(MediaCommand::SetRepeat(repeat_from_smtc(mode)));
            Ok(())
        },
    ))?;

    controls.ShuffleEnabledChangeRequested(&TypedEventHandler::new(
        move |_, args: windows::core::Ref<ShuffleEnabledChangeRequestedEventArgs>| {
            let args = args.ok()?;
            let enabled = args.RequestedShuffleEnabled()?;
            let _ = commands.send(MediaCommand::SetShuffle(enabled));
            Ok(())
        },
    ))?;
    Ok(())
}

fn publish_metadata(
    controls: &SystemMediaTransportControls,
    snapshot: &MediaSnapshot,
) -> windows::core::Result<()> {
    let updater = controls.DisplayUpdater()?;
    let properties = updater.MusicProperties()?;
    let (title, artist, album) = metadata_text(snapshot);
    properties.SetTitle(&HSTRING::from(title))?;
    // SMTC 会保留 DisplayUpdater 上一次的值，因此缺失字段也要
    // 显式写入空字符串，避免无标签曲目显示上一首的歌手或专辑。
    properties.SetArtist(&HSTRING::from(artist))?;
    properties.SetAlbumTitle(&HSTRING::from(album))?;
    updater.Update()?;
    Ok(())
}

fn metadata_text(snapshot: &MediaSnapshot) -> (&str, &str, &str) {
    (
        snapshot.title.as_str(),
        snapshot.artist.as_deref().unwrap_or_default(),
        snapshot.album.as_deref().unwrap_or_default(),
    )
}

fn publish_status(
    controls: &SystemMediaTransportControls,
    snapshot: &MediaSnapshot,
) -> windows::core::Result<()> {
    let status = match snapshot.status {
        PlayState::Playing => MediaPlaybackStatus::Playing,
        PlayState::Paused => MediaPlaybackStatus::Paused,
        PlayState::Stopped => MediaPlaybackStatus::Stopped,
    };
    controls.SetPlaybackStatus(status)?;
    controls.SetIsNextEnabled(snapshot.can_go_next)?;
    controls.SetIsPreviousEnabled(snapshot.can_go_previous)?;
    controls.SetAutoRepeatMode(smtc_repeat(snapshot.repeat))?;
    controls.SetShuffleEnabled(snapshot.shuffle)?;
    Ok(())
}

fn publish_timeline(
    controls: &SystemMediaTransportControls,
    snapshot: &MediaSnapshot,
) -> windows::core::Result<()> {
    let timeline = SystemMediaTransportControlsTimelineProperties::new()?;
    let duration = snapshot.duration.unwrap_or(snapshot.position);
    timeline.SetStartTime(TimeSpan::default())?;
    timeline.SetMinSeekTime(TimeSpan::default())?;
    timeline.SetPosition(duration_to_timespan(snapshot.position))?;
    timeline.SetEndTime(duration_to_timespan(duration))?;
    timeline.SetMaxSeekTime(duration_to_timespan(duration))?;
    controls.UpdateTimelineProperties(&timeline)?;
    Ok(())
}

fn read_snapshot(snapshot: &Arc<Mutex<MediaSnapshot>>) -> MediaSnapshot {
    snapshot
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// SMTC 没有“播完停止”，它和顺序播放一样报告为不循环。
fn smtc_repeat(repeat: RepeatMode) -> MediaPlaybackAutoRepeatMode {
    match repeat {
        RepeatMode::Stop | RepeatMode::None => MediaPlaybackAutoRepeatMode::None,
        RepeatMode::One => MediaPlaybackAutoRepeatMode::Track,
        RepeatMode::All => MediaPlaybackAutoRepeatMode::List,
    }
}

fn repeat_from_smtc(mode: MediaPlaybackAutoRepeatMode) -> RepeatMode {
    if mode == MediaPlaybackAutoRepeatMode::Track {
        RepeatMode::One
    } else if mode == MediaPlaybackAutoRepeatMode::List {
        RepeatMode::All
    } else {
        RepeatMode::None
    }
}

fn duration_to_timespan(duration: Duration) -> TimeSpan {
    TimeSpan {
        Duration: i64::try_from(duration.as_nanos() / 100).unwrap_or(i64::MAX),
    }
}

fn timespan_to_duration(span: TimeSpan) -> Duration {
    Duration::from_nanos(
        u64::try_from(span.Duration.max(0))
            .unwrap_or(0)
            .saturating_mul(100),
    )
}

unsafe fn create_hidden_window() -> Result<HWND, String> {
    let class_name = w!("nemuri-smtc");
    let instance = unsafe { GetModuleHandleW(None) }
        .map_err(|error| format!("GetModuleHandleW 失败: {error}"))?;
    let class = WNDCLASSEXW {
        cbSize: mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wnd_proc),
        hInstance: instance.into(),
        hbrBackground: HBRUSH::default(),
        lpszClassName: class_name,
        ..Default::default()
    };
    if unsafe { RegisterClassExW(&class) } == 0 {
        return Err("RegisterClassExW 失败".to_owned());
    }
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
            class_name,
            w!(""),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance.into()),
            None,
        )
    }
    .map_err(|error| format!("CreateWindowExW 失败: {error}"))?;
    if hwnd.0.is_null() {
        return Err("隐藏顶层窗口句柄为空".to_owned());
    }
    Ok(hwnd)
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_DESTROY {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

unsafe fn pump_messages() {
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
        if message.message == WM_QUIT {
            break;
        }
        let _ = unsafe { TranslateMessage(&message) };
        unsafe {
            DispatchMessageW(&message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_metadata_fields_are_published_as_empty_text() {
        let snapshot = MediaSnapshot {
            title: "Track".to_owned(),
            artist: None,
            album: None,
            ..MediaSnapshot::empty()
        };

        assert_eq!(metadata_text(&snapshot), ("Track", "", ""));
    }
}
