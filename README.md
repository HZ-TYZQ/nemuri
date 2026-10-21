# Nemuri

Nemuri（眠り，睡眠）是为 ASMR 长录音设计的终端播放器，也照常播放普通音乐。

- 适合按创作者分文件夹整理的音乐库
- 支持 MP3、FLAC、WAV、OGG/OGA Vorbis、M4A/AAC、AAC ADTS 和 AIFF
- 支持 Fedora 44、Arch Linux 及其衍生版和 Windows 11（x86_64）

1.4.0 及以前叫 Music Player，升级说明见[下文](#从-music-player-升级)。

## 功能

- **为入睡设计**：默认每首播完就停；`t` 睡眠定时（15–120 分钟或本条播完），到点前慢慢淡出；录音从起点或上次听到的地方开始时淡入，放到终点前淡出，不会突然出声或被一刀切断
- **续听**：10 分钟以上的录音记住听到哪里，下次接着放，曲库里显示进度
- **跳转点**：`b` 记下位置，`{` `}` 前后跳；B 站的时间轴可以整段粘贴进来一次加好；还能设成录音的起点和终点，跳过开场和片尾，播放时按 `(` `)` 就在当前位置设好
- **片段播放列表**：把几个录音里喜欢的段落排成列表，一段放完接下一段，切换时淡出淡入
- **按文件夹听**：`f` 只看一个创作者的文件夹，搜索和播放都在里面进行
- **声道调整**：`,` `.` 调左右平衡，`w` 互换左右声道
- **隐私模式**：`i` 打开后，系统媒体控制和锁屏只显示“正在播放”
- **其他**：模糊搜索、排序、队列、歌词、频谱、鼠标操作、系统媒体控制（MPRIS / SMTC），退出后记住播放位置

## 安装

从 [GitHub Releases](https://github.com/HZ-TYZQ/nemuri/releases) 下载，用同一页的 `SHA256SUMS.txt` 核对。

| 系统 | 安装方法 |
|---|---|
| Fedora 44 | `sudo dnf install ./nemuri-<版本>-1.fc44.x86_64.rpm` |
| Arch Linux 及衍生版 | `sudo pacman -U ./nemuri-<版本>-1-x86_64.pkg.tar.zst` |
| Windows 11 | 运行 `nemuri-<版本>-windows-x86_64-setup.exe`，装到当前用户，不需要管理员权限；<br>或者解压 `…-portable.zip`，运行 `nemuri.cmd` |

Arch 包按发版时的 Arch 构建，安装前先用 `sudo pacman -Syu` 把系统更新到最新。

Windows 安装包尚未签名，SmartScreen 可能提示“未知发布者”。

## 使用

```sh
nemuri                         # 打开音乐库（第一次运行时用系统的“音乐”目录）
nemuri ~/ASMR                  # 这一次打开别的目录
nemuri --set-library ~/ASMR    # 以后都用这个目录
```

按 `?` 查看全部按键，`man nemuri` 查看完整手册。扫描不跟随符号链接。

### 常用按键

| 按键 | 操作 | 按键 | 操作 |
|---|---|---|---|
| `Enter` | 播放选中的曲目 | `Space` | 暂停 / 继续 |
| `n` / `p` | 下一首 / 上一首 | `←` `→` | 后退 / 前进 10 秒（`H` `L` 为 60 秒） |
| `0`–`9` | 跳到 0%–90%（有起点时 `0` 先回到起点） | `-` `=` | 音量 −5% / +5% |
| `z` | 切换循环方式 | `s` | 随机播放 |
| `t` | 睡眠定时 | `/` | 搜索 |
| `f` | 按文件夹筛选 | `a` / `A` | 加到队尾 / 设为下一首 |
| `b` / `B` | 加跳转点 / 跳转点面板 | `{` `}` | 上一个 / 下一个跳转点 |
| `(` / `)` | 在当前位置设起点 / 终点 | `Q` / `P` | 播放队列 / 播放列表 |
| `?` | 帮助 | `q` | 退出 |

面板里能用的按键显示在面板底部。鼠标：单击选中，双击播放，点进度条跳转。

## 片段播放列表

1. 播放一个长录音，在喜欢的段落开头按 `b` 加跳转点。也可以按 `B` 再按 `v`，把视频简介里的时间轴（每行“03:21 掏耳”）粘贴进去，按 `Ctrl+S` 一次加好。
2. 按 `B` 打开跳转点面板，选中一个跳转点按 `a`：从它到下一个跳转点的一段加进播放列表（按 `c` 新建列表）。
3. 按 `P`，选中列表按 `Enter`，再在一条上按 `Enter`，从这里开始播放。

- 列表里也能放整首：在曲库选中一首，按 `P`，选中列表按 `a`。
- 每个列表各有自己的循环方式，新建的列表放完就停；在列表里播放时，`z` 切换的是这个列表的。
- 片段只放自己那一段，时间从 0:00 算起，不影响录音的续听进度、起点和终点。
- 片段截在录音中间时，结尾 3 秒淡出，下一段从中间开始时 1 秒淡入；时长在设置里改（`clip-fade`）。
- 在条目面板里，`J` `K` 移动，`s` `S` 改开头 / 结尾，`e` 改名，`d` 移除。

## 设置

设置在 `config.kdl` 里（[KDL v2](https://kdl.dev) 格式）。第一次启动时自动生成，每一项都附有说明，直接编辑，重启后生效。例如：

```kdl
playback {
    volume 80
    repeat "stop"            // "stop"、"none"、"all" 或 "one"
    clip-fade 3              // 片段切换、录音放到终点时淡出的秒数，0 表示不淡
}
sleep {
    fade 30                  // 睡眠定时淡出的秒数，0 表示不淡出
}
theme {
    preset "light"           // "default" 深色终端、"light" 浅色终端、"terminal" 跟随终端配色
    selection-bg "#C8D3F0"   // 单独改某种颜色，能改的颜色都列在生成的文件里
}
```

- 运行中用按键改的设置（音量、循环、排序等）会在退出时写回，只改对应的行，注释保持原样。
- 写错的项只忽略那一项，启动时在底栏提示行号。

## 数据存放位置

Nemuri 不会往音乐库里写任何文件。

| 内容 | Linux | Windows |
|---|---|---|
| 设置 | `~/.config/nemuri/config.kdl` | `%APPDATA%\nemuri\config.kdl` |
| 播放列表 | `~/.local/share/nemuri/playlists/` | `%APPDATA%\nemuri\playlists\` |
| 续听进度 | `~/.local/share/nemuri/progress.json` | `%APPDATA%\nemuri\progress.json` |
| 跳转点、起点和终点 | `~/.local/share/nemuri/marks.json` | `%APPDATA%\nemuri\marks.json` |
| 上次的播放位置 | `~/.local/share/nemuri/session.toml` | `%APPDATA%\nemuri\session.toml` |
| 曲库缓存（可删） | `~/.cache/nemuri/library.sqlite3` | `%LOCALAPPDATA%\nemuri\library.sqlite3` |

- Linux 上设置了 `XDG_CONFIG_HOME`、`XDG_DATA_HOME` 或 `XDG_CACHE_HOME` 时，用这些目录。
- 升级和卸载都不会删除这些文件。
- 2.2.0 起播放列表改用新格式。旧列表照常能读，但改动过的列表，2.0.0 及更早的版本就读不了了。

## 从 Music Player 升级

- 命令改为 `nemuri`。RPM 和 Windows 安装程序会直接替换旧版。
- 第一次启动时，会把旧的 `tui-music-player` 目录（设置、播放列表、进度）复制过来，旧目录保留不动。
- 脚本里的 `playerctl -p music_player` 要改成 `playerctl -p nemuri`。

## 构建

Nemuri 用 Rust 编写，播放用 Rodio，标签用 Lofty 读取，曲库索引存在可以随时重建的 SQLite 缓存里。

在 Fedora 容器中构建，需要 `rust cargo alsa-lib-devel sqlite-devel`；打 RPM 另需 `rpm-build desktop-file-utils`。
Arch 包在 Arch 容器中打，需要 `base-devel rust alsa-lib sqlite hicolor-icon-theme desktop-file-utils`。

```sh
cargo test --all-targets --locked
./packaging/build-rpm.sh    # 生成 RPM 和 SRPM
./packaging/build-arch.sh   # 生成 Arch 包（.pkg.tar.zst）
```

Windows 版由 GitHub Actions 构建和打包。

## 许可证

MIT，版权属于 HZ-TYZQ，见 `LICENSE`。

作者和维护者是 HZ-TYZQ；早期版本的应用图标由 Codex 设计。
