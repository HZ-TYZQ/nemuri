//! 播放后端的无声集成测试。

use std::fs;
use std::io::Write;
use std::path::Path;
use std::thread::sleep;
use std::time::{Duration, Instant};

use nemuri::player::{ChannelMix, PlayState, PlaybackBackend, Player, PlayerEvent};

fn write_test_wav(path: &Path, duration_secs: f32) {
    let sample_rate = 8_000u32;
    let sample_count = (sample_rate as f32 * duration_secs) as usize;
    let mut samples = Vec::with_capacity(sample_count * 2);
    for index in 0..sample_count {
        let time = index as f32 / sample_rate as f32;
        let value = (time * 440.0 * std::f32::consts::TAU).sin() * 0.3;
        samples.extend_from_slice(&((value * i16::MAX as f32) as i16).to_le_bytes());
    }
    let data_len = samples.len() as u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&samples);
    let mut file = fs::File::create(path).unwrap();
    file.write_all(&wav).unwrap();
}

#[test]
fn play_pause_seek_volume_mute_and_stop() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("basic.wav");
    write_test_wav(&wav, 3.0);
    let mut player = Player::new_for_tests().unwrap();
    assert_eq!(player.state(), PlayState::Stopped);

    player.set_volume(55);
    assert_eq!(player.volume(), 55);
    player.set_muted(true);
    assert!(player.is_muted());

    player.play(&wav).expect("播放器应能加载测试 WAV");
    assert_eq!(player.state(), PlayState::Playing);
    sleep(Duration::from_millis(150));
    player.seek_relative(1);
    player.toggle_pause();
    assert_eq!(player.state(), PlayState::Paused);
    player.toggle_pause();
    assert_eq!(player.state(), PlayState::Playing);
    player.stop();
    assert_eq!(player.state(), PlayState::Stopped);
}

#[test]
fn natural_end_of_a_long_track_is_not_an_early_eos_error() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("long-finish.wav");
    write_test_wav(&wav, 2.2);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();

    let deadline = Instant::now() + Duration::from_secs(6);
    let mut ended = None;
    while Instant::now() < deadline {
        for event in player.drain_events() {
            match event {
                PlayerEvent::EndOfStream => {
                    ended = Some(true);
                    break;
                }
                PlayerEvent::Error(message) => {
                    panic!("自然播完不应判成解码错误: {message}");
                }
                _ => {}
            }
        }
        if ended.is_some() {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    assert_eq!(ended, Some(true), "长于 1s 的短音频应产生正常 EOS");
    assert_eq!(player.state(), PlayState::Stopped);
}

#[test]
fn reports_natural_end_of_stream() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("finish.wav");
    write_test_wav(&wav, 0.2);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut finished = false;
    while Instant::now() < deadline {
        if player.drain_events().contains(&PlayerEvent::EndOfStream) {
            finished = true;
            break;
        }
        sleep(Duration::from_millis(20));
    }
    assert!(finished, "短音频应产生播放结束事件");
    assert_eq!(player.state(), PlayState::Stopped);
}

#[test]
fn reports_spectrum_frames_from_playing_audio() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("spectrum.wav");
    write_test_wav(&wav, 1.0);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut frame = None;
    while Instant::now() < deadline {
        frame = player.drain_events().into_iter().find_map(|event| {
            if let PlayerEvent::SpectrumFrame {
                magnitudes,
                sample_rate,
            } = event
            {
                Some((magnitudes, sample_rate))
            } else {
                None
            }
        });
        if frame.is_some() {
            break;
        }
        sleep(Duration::from_millis(20));
    }

    let (frame, sample_rate) = frame.expect("播放音频时应产生频谱事件");
    assert_eq!(frame.len(), 512);
    assert_eq!(sample_rate, 8_000);
    assert!(frame.iter().any(|magnitude| *magnitude > -60.0));
}

#[test]
fn missing_file_is_rejected_before_playback() {
    let mut player = Player::new_for_tests().unwrap();
    let error = player
        .play(Path::new("/nonexistent/definitely_missing.mp3"))
        .unwrap_err();
    assert!(error.contains("不存在"));
    assert_eq!(player.state(), PlayState::Stopped);
}

#[test]
fn plays_audio_from_a_unicode_path_with_spaces() {
    let temp = tempfile::tempdir().unwrap();
    let directory = temp.path().join("测试 音乐");
    fs::create_dir(&directory).unwrap();
    let wav = directory.join("示例 音频.wav");
    write_test_wav(&wav, 0.2);

    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).expect("播放器应能加载 Unicode 路径");
    assert_eq!(player.state(), PlayState::Playing);
    player.stop();
}

#[test]
fn play_resets_position_immediately() {
    let temp = tempfile::tempdir().unwrap();
    let first = temp.path().join("first.wav");
    let second = temp.path().join("second.wav");
    write_test_wav(&first, 3.0);
    write_test_wav(&second, 3.0);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&first).unwrap();
    sleep(Duration::from_millis(200));
    player.seek_relative(1);
    player.play(&second).unwrap();
    assert!(
        player.position() < Duration::from_millis(100),
        "切歌后 position 必须立即接近 0，实际 {:?}",
        player.position()
    );
}

#[test]
fn empty_file_is_rejected_before_playback() {
    let temp = tempfile::tempdir().unwrap();
    let empty = temp.path().join("empty.wav");
    fs::write(&empty, []).unwrap();
    let mut player = Player::new_for_tests().unwrap();
    let error = player.play(&empty).unwrap_err();
    assert!(error.contains("空"), "空文件应在 play() 同步失败: {error}");
    assert_eq!(player.state(), PlayState::Stopped);
}

#[test]
fn stale_eos_from_previous_track_is_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let short = temp.path().join("short.wav");
    let long = temp.path().join("long.wav");
    write_test_wav(&short, 0.2);
    write_test_wav(&long, 3.0);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&short).unwrap();
    player.play(&long).unwrap();

    sleep(Duration::from_millis(400));
    let events = player.drain_events();
    assert!(
        !events.contains(&PlayerEvent::EndOfStream),
        "旧曲目的迟到 EOS 不得结束当前播放: {events:?}"
    );
    assert_eq!(player.state(), PlayState::Playing);
}

#[test]
fn pause_does_not_emit_new_spectrum_progress() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("pause-spectrum.wav");
    write_test_wav(&wav, 2.0);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut saw_frame = false;
    while Instant::now() < deadline {
        if player
            .drain_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::SpectrumFrame { .. }))
        {
            saw_frame = true;
            break;
        }
        sleep(Duration::from_millis(20));
    }
    assert!(saw_frame, "播放时应先产生频谱");

    player.toggle_pause();
    let _ = player.drain_events();
    sleep(Duration::from_millis(80));
    assert!(
        player
            .drain_events()
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SpectrumFrame { .. })),
        "暂停后不应再产生新的频谱帧"
    );
}

#[test]
fn stop_does_not_leak_old_spectrum_frames() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("stop-spectrum.wav");
    write_test_wav(&wav, 1.0);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();
    sleep(Duration::from_millis(80));
    player.stop();
    let _ = player.drain_events();
    sleep(Duration::from_millis(80));
    assert!(
        player
            .drain_events()
            .iter()
            .all(|event| !matches!(event, PlayerEvent::SpectrumFrame { .. })),
        "停止后旧频谱不得泄漏"
    );
}

#[test]
fn high_sample_rate_spectrum_reports_source_rate() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("hires.wav");
    write_test_wav_at_rate(&wav, 1.0, 96_000);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();

    let deadline = Instant::now() + Duration::from_secs(3);
    let mut rate = None;
    while Instant::now() < deadline {
        rate = player.drain_events().into_iter().find_map(|event| {
            if let PlayerEvent::SpectrumFrame { sample_rate, .. } = event {
                Some(sample_rate)
            } else {
                None
            }
        });
        if rate.is_some() {
            break;
        }
        sleep(Duration::from_millis(20));
    }
    assert_eq!(rate, Some(96_000));
}

#[test]
fn open_loads_paused_without_starting_playback() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("open.wav");
    write_test_wav(&wav, 2.0);
    let mut player = Player::new_for_tests().unwrap();
    player.open(&wav).unwrap();
    assert_eq!(player.state(), PlayState::Paused);
    assert!(player.position() < Duration::from_millis(50));
    sleep(Duration::from_millis(120));
    assert_eq!(player.state(), PlayState::Paused);
    assert!(
        player.position() < Duration::from_millis(80),
        "open() 后进度不应推进，实际 {:?}",
        player.position()
    );
    player.resume();
    assert_eq!(player.state(), PlayState::Playing);
}

#[test]
fn open_at_then_resume_plays_on_from_the_saved_position() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("resume.wav");
    write_test_wav(&wav, 3.0);
    let mut player = Player::new_for_tests().unwrap();
    player.open_at(&wav, Duration::from_millis(1500)).unwrap();
    assert_eq!(player.state(), PlayState::Paused);

    player.resume();
    assert_eq!(player.state(), PlayState::Playing);
    // 装载之后头一次读位置故意晚一些，位置也要往前走。
    sleep(Duration::from_millis(200));
    let position = player.position();
    assert!(
        position > Duration::from_millis(1600) && position < Duration::from_millis(2300),
        "续播应从 1.5s 往后走，实际 {position:?}"
    );
}

#[test]
fn balance_and_swap_keep_mono_positions_and_the_end_in_real_time() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("mono.wav");
    write_test_wav(&wav, 0.6);
    let mut player = Player::new_for_tests().unwrap();
    // 单声道被复制成左右两个声道，位置不能因此快一倍。
    player.set_channel_mix(ChannelMix {
        balance: 40,
        swapped: true,
    });
    player.play(&wav).unwrap();
    let mut position = player.position();
    for _ in 0..5 {
        sleep(Duration::from_millis(40));
        position = player.position();
    }
    assert!(
        position > Duration::from_millis(120) && position < Duration::from_millis(450),
        "播放 200ms 后位置是 {position:?}"
    );
    assert!(player.seek_to(Duration::from_millis(300)));

    let deadline = Instant::now() + Duration::from_secs(3);
    let events = loop {
        let events = player.drain_events();
        if events.contains(&PlayerEvent::EndOfStream) || Instant::now() > deadline {
            break events;
        }
        sleep(Duration::from_millis(10));
    };
    assert!(events.contains(&PlayerEvent::EndOfStream), "{events:?}");
}

#[test]
fn a_late_first_position_read_after_a_load_or_seek_still_moves_on() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("late.wav");
    write_test_wav(&wav, 3.0);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();
    // 以前装载后第一次读位置晚于 100ms，位置就一直停在 0:00。
    sleep(Duration::from_millis(300));
    let position = player.position();
    assert!(
        position >= Duration::from_millis(150) && position < Duration::from_millis(700),
        "播放 300ms 后位置是 {position:?}"
    );

    assert!(player.seek_to(Duration::from_millis(1000)));
    sleep(Duration::from_millis(300));
    let position = player.position();
    assert!(
        position >= Duration::from_millis(1150) && position < Duration::from_millis(1700),
        "跳到 1s 再播 300ms 后位置是 {position:?}"
    );
    sleep(Duration::from_millis(200));
    assert!(player.position() > position, "位置停住不动了");
}

#[test]
fn seek_to_moves_to_requested_position() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("seek.wav");
    write_test_wav(&wav, 3.0);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();
    assert!(player.seek_to(Duration::from_millis(1500)));
    let position = player.position();
    assert!(
        position >= Duration::from_millis(1400) && position < Duration::from_millis(1700),
        "seek_to(1.5s) 实际 {:?}",
        position
    );
}

/// 等这一首播完，返回从 `since` 起过了多久；报解码错误或等了 8 秒还没播完时测试失败。
fn wait_for_end(player: &mut Player, since: Instant) -> Duration {
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        for event in player.drain_events() {
            match event {
                PlayerEvent::EndOfStream => return since.elapsed(),
                PlayerEvent::Error(message) => panic!("不应报解码错误: {message}"),
                _ => {}
            }
        }
        sleep(Duration::from_millis(10));
    }
    panic!("8 秒内没有播完");
}

#[test]
fn playback_stops_at_the_stop_position_as_a_normal_end() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("stop.wav");
    write_test_wav(&wav, 4.0);
    let mut player = Player::new_for_tests().unwrap();
    let started = Instant::now();
    player.play(&wav).unwrap();
    player.set_stop_at(Some(Duration::from_millis(500)));
    // 远早于 4 秒的文件结尾就停了，而且不当作解码提前终止。
    let elapsed = wait_for_end(&mut player, started);
    assert!(elapsed < Duration::from_millis(2500), "{elapsed:?}");
    assert_eq!(player.state(), PlayState::Stopped);
}

#[test]
fn a_seek_onto_or_past_the_stop_plays_on_to_the_end_of_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("past.wav");
    write_test_wav(&wav, 2.0);
    let mut player = Player::new_for_tests().unwrap();

    player.play(&wav).unwrap();
    player.set_stop_at(Some(Duration::from_millis(500)));
    assert!(player.seek_to(Duration::from_millis(1000)));
    let seeked = Instant::now();
    let elapsed = wait_for_end(&mut player, seeked);
    assert!(elapsed > Duration::from_millis(700), "{elapsed:?}");

    // 正好跳到停止位置上也不停。
    player.play(&wav).unwrap();
    player.set_stop_at(Some(Duration::from_millis(500)));
    assert!(player.seek_to(Duration::from_millis(500)));
    let seeked = Instant::now();
    let elapsed = wait_for_end(&mut player, seeked);
    assert!(elapsed > Duration::from_millis(1100), "{elapsed:?}");
}

#[test]
fn seeking_back_before_the_stop_stops_there_again() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("back.wav");
    write_test_wav(&wav, 4.0);
    let mut player = Player::new_for_tests().unwrap();
    player.play(&wav).unwrap();
    player.set_stop_at(Some(Duration::from_millis(1000)));
    assert!(player.seek_to(Duration::from_millis(2000)));
    sleep(Duration::from_millis(100));
    assert!(player.seek_to(Duration::from_millis(500)));
    let seeked = Instant::now();
    // 从 0.5 秒放到 1 秒的停止位置，而不是放到 4 秒的结尾。
    let elapsed = wait_for_end(&mut player, seeked);
    assert!(
        elapsed > Duration::from_millis(350) && elapsed < Duration::from_millis(2500),
        "{elapsed:?}"
    );
}

#[test]
fn a_cleared_stop_or_a_new_track_plays_to_the_end_of_the_file() {
    let temp = tempfile::tempdir().unwrap();
    let wav = temp.path().join("clear.wav");
    write_test_wav(&wav, 2.0);
    let mut player = Player::new_for_tests().unwrap();
    let started = Instant::now();
    player.play(&wav).unwrap();
    player.set_stop_at(Some(Duration::from_millis(600)));
    sleep(Duration::from_millis(200));
    player.set_stop_at(None);
    let elapsed = wait_for_end(&mut player, started);
    assert!(elapsed > Duration::from_millis(1500), "{elapsed:?}");

    // 停止位置属于上一首：装载新曲目时清掉。
    let other = temp.path().join("other.wav");
    write_test_wav(&other, 1.5);
    player.play(&wav).unwrap();
    player.set_stop_at(Some(Duration::from_millis(300)));
    let started = Instant::now();
    player.play(&other).unwrap();
    let elapsed = wait_for_end(&mut player, started);
    assert!(elapsed > Duration::from_millis(1100), "{elapsed:?}");
}

fn write_test_wav_at_rate(path: &Path, duration_secs: f32, sample_rate: u32) {
    let sample_count = (sample_rate as f32 * duration_secs) as usize;
    let mut samples = Vec::with_capacity(sample_count * 2);
    for index in 0..sample_count {
        let time = index as f32 / sample_rate as f32;
        let value = (time * 440.0 * std::f32::consts::TAU).sin() * 0.3;
        samples.extend_from_slice(&((value * i16::MAX as f32) as i16).to_le_bytes());
    }
    let data_len = samples.len() as u32;
    let mut wav = Vec::new();
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&samples);
    let mut file = fs::File::create(path).unwrap();
    file.write_all(&wav).unwrap();
}
