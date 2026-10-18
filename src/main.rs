use std::io::{self, Write, stdout};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind, MouseEventKind,
};
#[cfg(not(windows))]
use crossterm::event::{DisableBracketedPaste, EnableBracketedPaste};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::prelude::*;

use nemuri::app::{Action, App, join_notes};
use nemuri::cli::{Cli, validate_directory};
use nemuri::config::{self, AppConfig, AppPaths, LoadedConfig};
use nemuri::media::{MediaEvent, MediaSession};
use nemuri::ui::{self, MouseInput, ViewLayout};

fn main() -> ExitCode {
    match run_application() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            report(&error);
            ExitCode::FAILURE
        }
    }
}

/// 终端挂断后标准错误也写不进去；`eprintln!` 写失败会 panic，排在后面的保存就做不成了。
fn report(error: &str) {
    let _ = writeln!(io::stderr(), "nemuri: {error}");
}

/// 终端挂断（关掉终端窗口、终端会话结束，SIGHUP）或收到 SIGTERM 时，照常保存再退出。
/// 默认的处理是直接结束进程，会话和运行中改过的设置都来不及保存。
/// 不管 SIGINT：原始模式下 Ctrl+C 是按键，不是信号。
#[derive(Default)]
struct Termination {
    hangup: Arc<AtomicBool>,
    terminate: Arc<AtomicBool>,
}

impl Termination {
    #[cfg(unix)]
    fn register() -> io::Result<Self> {
        use signal_hook::consts::{SIGHUP, SIGTERM};
        let termination = Self::default();
        signal_hook::flag::register(SIGHUP, Arc::clone(&termination.hangup))?;
        signal_hook::flag::register(SIGTERM, Arc::clone(&termination.terminate))?;
        Ok(termination)
    }

    #[cfg(not(unix))]
    fn register() -> io::Result<Self> {
        Ok(Self::default())
    }

    fn requested(&self) -> bool {
        self.hung_up() || self.terminate.load(Ordering::Relaxed)
    }

    /// 终端已经没了：不能再往里写，写终端出的错也不算失败。
    fn hung_up(&self) -> bool {
        self.hangup.load(Ordering::Relaxed)
    }
}

fn run_application() -> Result<(), String> {
    let cli = Cli::parse();
    let paths = AppPaths::discover().map_err(|error| format!("无法确定用户数据目录: {error}"))?;
    let adopted = config::adopt_previous_dirs(&paths)?;
    let LoadedConfig {
        mut config,
        message: config_warning,
        writable,
    } = AppConfig::load(&paths)
        .map_err(|error| format!("无法读取配置 {}: {error}", paths.config_file.display()))?;

    let mut library_note = None;
    let library_dir = if let Some(path) = cli.set_library {
        let path = validate_directory(&path).map_err(|error| error.to_string())?;
        config::set_library(&paths.config_file, &path)?;
        config.library_dir = Some(path.clone());
        library_note = Some(format!("主音乐库已设置为 {}", path.display()));
        path
    } else if let Some(path) = cli.directory {
        validate_directory(&path).map_err(|error| error.to_string())?
    } else if let Some(path) = config.library_dir.as_ref() {
        validate_directory(path).map_err(|error| {
            format!("配置的主音乐库不可用: {error}。请使用 --set-library PATH 设置新目录")
        })?
    } else {
        let path = paths.default_music_dir.as_ref().ok_or_else(|| {
            "系统没有提供默认 Music 目录，请使用 --set-library PATH 设置主音乐库".to_owned()
        })?;
        validate_directory(path).map_err(|error| {
            format!("默认 Music 目录不可用: {error}。请使用 --set-library PATH 设置主音乐库")
        })?
    };

    // 配置写错的提示在前，复制旧目录的说明在最后，谁也不把谁挤掉。
    let warning = join_notes([config_warning, library_note, adopted]);

    // 播放器、播放列表目录及工作线程都在切换终端模式前初始化。
    // 这样启动失败时错误仍是普通、可复制的终端文本。
    let mut app = App::new(library_dir, paths, config, warning, writable)?;
    let session = match MediaSession::start() {
        Ok(session) => Some(session),
        Err(error) => {
            app.notify_if_idle(error);
            None
        }
    };
    let termination =
        Termination::register().map_err(|error| format!("无法注册信号处理: {error}"))?;
    let mut terminal = setup_terminal().map_err(|error| format!("无法初始化终端: {error}"))?;
    let run_result = run(&mut terminal, &mut app, session.as_ref(), &termination)
        .map_err(|error| format!("终端运行失败: {error}"));
    let hung_up = termination.hung_up();
    let restore_result = if hung_up {
        // 也不让 `Terminal` 析构：它会去显示光标，写失败时用 `eprintln!` 报告。
        std::mem::forget(terminal);
        Ok(())
    } else {
        restore_terminal(&mut terminal).map_err(|error| format!("无法恢复终端状态: {error}"))
    };
    let save_result = app.save_settings();
    // 播放列表这里只是重试没存上的改动；会话、收听进度和跳转点只是便利功能。
    // 这些保存失败都不应让整个程序以失败退出。
    if let Err(error) = app.save_playlists() {
        report(&error);
    }
    if let Err(error) = app.save_session() {
        report(&error);
    }
    if let Err(error) = app.save_progress() {
        report(&error);
    }
    if let Err(error) = app.save_marks() {
        report(&error);
    }

    // 挂断时正在画的那一帧会写失败，这不是程序出错。
    if !hung_up {
        run_result?;
        restore_result?;
    }
    save_result?;
    Ok(())
}

/// 在单独的线程里读终端事件。终端挂断后 crossterm 0.29 读到 EOF 会在 `event::poll`
/// 里一直空转、不再返回；读事件放在这里，主循环总能按时醒来，看到挂断后照常保存退出。
/// 线程不用等它结束，进程退出时一起结束。
fn spawn_input_reader() -> io::Result<Receiver<io::Result<Event>>> {
    let (events, receiver) = mpsc::channel();
    thread::Builder::new()
        .name("terminal-input".to_owned())
        .spawn(move || {
            loop {
                let event = event::read();
                let failed = event.is_err();
                if events.send(event).is_err() || failed {
                    break;
                }
            }
        })?;
    Ok(receiver)
}

fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
    session: Option<&MediaSession>,
    termination: &Termination,
) -> io::Result<()> {
    let input = spawn_input_reader()?;
    let mut view = ViewLayout::default();
    let mut mouse = MouseInput::default();
    let mut mouse_captured = false;
    while !app.should_quit() && !termination.requested() {
        if app.config().mouse_enabled != mouse_captured {
            set_mouse_capture(app.config().mouse_enabled)?;
            mouse_captured = app.config().mouse_enabled;
        }
        terminal.draw(|frame| ui::draw(frame, app, &mut view))?;
        // 频谱开启时把主循环提到约 50Hz，与 spectrum 源 20ms interval 对齐；
        // 关闭时回到 50ms 降低空闲唤醒。
        let poll_ms = if app.config().visualizer_enabled {
            20
        } else {
            50
        };
        match input.recv_timeout(Duration::from_millis(poll_ms)) {
            Ok(event) => match event? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if let Some(action) = ui::key_action(app, &view, key) {
                        app.dispatch(action);
                    }
                }
                // 鼠标捕获会连移动一起上报，这里直接丢弃，避免空闲时被持续唤醒。
                // 关闭鼠标后即便仍有序列送达（例如被上游注入），也不再处理。
                Event::Mouse(event)
                    if mouse_captured && !matches!(event.kind, MouseEventKind::Moved) =>
                {
                    for action in mouse.actions(event, &view, app) {
                        app.dispatch(action);
                    }
                }
                Event::Paste(text) => app.dispatch(Action::Paste(text)),
                _ => {}
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                return Err(io::Error::other("读取终端输入的线程意外退出"));
            }
        }
        if let Some(session) = session {
            for command in session.try_recv() {
                app.dispatch(command.into());
            }
        }
        app.on_tick(Instant::now());
        if let Some(session) = session {
            session.publish(app.media_snapshot());
            for event in app.drain_media_events() {
                let MediaEvent::Seeked { position } = event;
                session.notify_seeked(position);
            }
        }
    }
    if mouse_captured && !termination.hung_up() {
        set_mouse_capture(false)?;
    }
    Ok(())
}

fn set_mouse_capture(enabled: bool) -> io::Result<()> {
    if enabled {
        execute!(stdout(), EnableMouseCapture)
    } else {
        execute!(stdout(), DisableMouseCapture)
    }
}

fn setup_terminal() -> io::Result<Terminal<CrosstermBackend<io::Stdout>>> {
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        #[cfg(not(windows))]
        let _ = execute!(stdout(), DisableBracketedPaste);
        let _ = execute!(stdout(), DisableMouseCapture, LeaveAlternateScreen);
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut output = stdout();
    execute!(output, EnterAlternateScreen)?;
    // 粘贴作为一整段送来，不会被当成一串按键。Windows 上不打开：crossterm 在那里用控制台 API
    // 读按键，终端包在粘贴两头的转义序列会被拆成按键，其中的 Esc 会被当成“返回”。
    // Windows 上的粘贴仍逐键送来，粘贴框里 Enter 是换行，照样能用。
    #[cfg(not(windows))]
    execute!(output, EnableBracketedPaste)?;
    Terminal::new(CrosstermBackend::new(output))
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> io::Result<()> {
    disable_raw_mode()?;
    #[cfg(not(windows))]
    execute!(terminal.backend_mut(), DisableBracketedPaste)?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        LeaveAlternateScreen
    )?;
    terminal.show_cursor()
}
