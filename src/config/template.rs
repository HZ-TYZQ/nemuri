//! 第一次启动时生成的 `config.kdl`：写出全部设置，每项附上说明。

use super::AppConfig;
use super::document::{quoted, repeat_name, sort_key_name};

pub(super) fn render(config: &AppConfig) -> String {
    let library = match &config.library_dir {
        Some(path) => format!("library {}", quoted(&path.to_string_lossy())),
        None => "// library \"~/Music\"".to_owned(),
    };
    let flag = |value: bool| if value { "#true" } else { "#false" };
    format!(
        r##"// Nemuri 配置文件，使用 KDL v2 格式（https://kdl.dev）。
//
// 改完重启播放器生效。运行中用按键改过的设置（音量、静音、循环、随机、
// 排序和各个开关）会在退出时写回这里，只改动那几项，其余内容和注释原样保留。
// 布尔值写作 #true / #false，字符串要加双引号。

// 主音乐库目录，可以用 ~ 开头表示家目录；也可以用 nemuri --set-library PATH 设置。
{library}

playback {{
    // 音量 0–100
    volume {volume}
    muted {muted}
    // 循环："stop" 播完停止、"none" 顺序播放、"all" 列表循环、"one" 单曲循环
    repeat "{repeat}"
    shuffle {shuffle}
    // 左右平衡（, / .）：-100 只有左声道，100 只有右声道，0 居中
    balance {balance}
    // 互换左右声道（w），耳机戴反时用
    swap-channels {swap_channels}
    // 播放列表里截在中间的片段：放到结尾时淡出的秒数，0–30；0 表示不淡。
    // 下一段从录音中间开始时，用它的三分之一淡入
    clip-fade {clip_fade}
}}

// 曲库排序："path"、"title"、"artist"、"album" 或 "duration"
sort "{sort}" descending={descending}

interface {{
    // 频谱（v）、歌词面板（y）、鼠标（M）
    visualizer {visualizer}
    lyrics {lyrics}
    mouse {mouse}
    // 隐私模式（i）：系统媒体控制只显示“正在播放”，不发送标题、歌手、专辑和文件路径
    privacy {privacy}
}}

sleep {{
    // 睡眠定时（t）到点前淡出的秒数，0–600；0 表示不淡出，到点直接暂停
    fade {fade}
}}

theme {{
    // 预设："default" 深色终端、"light" 浅色终端、"terminal" 只用终端调色板
    preset "default"
    // 去掉 // 就能逐项覆盖预设的颜色。可以写 #RRGGBB、颜色名（如 red、dark-gray）、
    // 0–255 的调色板序号，或 reset（终端默认前景色）。
    // primary "#F2F2F2"
    // muted "#B8B8B8"
    // border "#8A8A8A"
    // selection-bg "#343846"
    // danger "red"
    // spectrum-low "#A8A8A8"
    // spectrum-high "#F2F2F2"
}}
"##,
        volume = config.volume,
        muted = flag(config.muted),
        repeat = repeat_name(config.repeat),
        shuffle = flag(config.shuffle),
        balance = config.balance,
        swap_channels = flag(config.swap_channels),
        sort = sort_key_name(config.sort.key),
        descending = flag(config.sort.descending),
        visualizer = flag(config.visualizer_enabled),
        lyrics = flag(config.lyrics_enabled),
        mouse = flag(config.mouse_enabled),
        privacy = flag(config.privacy),
        fade = config.sleep_fade.as_secs(),
        clip_fade = config.clip_fade.as_secs(),
    )
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::config::document::read;
    use crate::theme::Theme;
    use crate::track::{RepeatMode, SortKey, SortOrder};

    #[test]
    fn the_template_reads_back_as_the_same_settings_without_warnings() {
        let configs = [
            AppConfig::default(),
            AppConfig {
                library_dir: Some(PathBuf::from(r#"/srv/"引号"\反斜杠"#)),
                volume: 7,
                muted: true,
                repeat: RepeatMode::All,
                shuffle: true,
                balance: 70,
                swap_channels: true,
                sort: SortOrder {
                    key: SortKey::Duration,
                    descending: true,
                },
                visualizer_enabled: false,
                mouse_enabled: false,
                lyrics_enabled: false,
                privacy: true,
                sleep_fade: std::time::Duration::from_secs(90),
                clip_fade: std::time::Duration::from_secs(10),
                theme: Theme::default(),
            },
        ];
        for config in configs {
            let loaded = read(&render(&config));
            assert_eq!(loaded.message, None);
            assert_eq!(loaded.config, config);
        }
    }

    #[test]
    fn a_new_config_stops_after_each_track() {
        assert!(render(&AppConfig::default()).contains("    repeat \"stop\"\n"));
    }

    #[test]
    fn uncommenting_a_theme_color_overrides_the_preset() {
        let text =
            render(&AppConfig::default()).replace("// border \"#8A8A8A\"", "border \"#123456\"");
        let loaded = read(&text);
        assert_eq!(loaded.message, None);
        let mut expected = Theme::default();
        *expected.color_mut("border").unwrap() = ratatui::style::Color::Rgb(0x12, 0x34, 0x56);
        assert_eq!(loaded.config.theme, expected);
    }
}
