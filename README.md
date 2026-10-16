# Nemuri

Nemuri（眠り，睡眠）是一个用 Rust 编写的终端播放器，主要为按创作者分文件夹整理的 ASMR 长录音设计，也照常播放普通音乐。1.4.0 及以前叫 Music Player，见[从 Music Player 升级](#从-music-player-升级)。

播放使用 Rodio，媒体信息由 Lofty 读取，曲库索引存在可随时重建的 SQLite 缓存里。支持 MP3、FLAC、WAV、OGG/OGA Vorbis、M4A/AAC、AAC ADTS 和 AIFF。

## 功能

- **为入睡设计**：默认播完一首就停；`t` 睡眠定时（15–120 分钟或“本条播完时”），到点前淡出，再暂停并保留位置
- **长录音续听**：10 分钟及以上的录音各自记住进度，下次接着放（按 `0` 从头）；曲库里显示进度条和“✓ 听完”，文件改名后重扫也能找回进度
- **跳转点**：长录音里按 `b` 记下当前位置，`{` / `}` 在跳转点之间前后跳，`B` 打开面板跳转、改名或删除；进度条上用 `|` 标出，当前段有名字时显示在“正在播放”的标题上。文件改名后重扫也能找回
- **按文件夹听**：`f` 只看音乐库第一层的某个文件夹，搜索和顺序、循环、随机播放都限定在其中，下次打开时恢复
- **文件名即曲名**：曲库原样显示文件名，第二行是所在文件夹、歌手、专辑和格式；标签里的标题、歌手和专辑仍可搜索
- **声道调整**：`,` / `.` 调左右平衡，`w` 互换左右声道，双耳录音一侧太响或耳机戴反时不用摘耳机
- **隐私模式**：`i` 开启后，系统媒体控制和锁屏小组件只看到“正在播放”，看不到标题和文件路径
- 后台扫描、Unicode 实时模糊搜索；可按路径、标题、歌手、专辑或时长排序，顺序播放跟随列表顺序
- 循环（播完停止 / 顺序 / 列表 / 单曲）与随机可独立组合，随机按整轮重洗
- 临时播放队列和持久命名播放列表；失效歌曲会被标记并跳过，删除列表不会删除音乐文件
- 歌词：读取同名 `.lrc`（UTF-8、GBK 或带 BOM 的 UTF-16）或内嵌歌词，同步歌词随播放高亮滚动；宽终端在曲库右侧常驻歌词面板
- 实时音频频谱（只分析正在播放的声音）、鼠标操作，以及 Linux MPRIS / Windows SMTC 媒体控制
- 退出时记住当前歌曲、进度和文件夹筛选，下次打开时暂停在原处，按 `Space` 续播

界面继承终端背景，默认使用灰白配色；浅色终端可换用其他预设，见[主题](#主题)。

## 安装

正式支持 Fedora 44 与 Windows 11 x86_64。请从项目 GitHub Release 下载，并用同一 Release 中的 `SHA256SUMS.txt` 核对文件。

- **Fedora**：`sudo dnf install ./nemuri-<版本>-1.fc44.x86_64.rpm`，运行依赖会自动安装。装好后可在桌面应用菜单搜索“Nemuri”“ASMR”或“睡眠”启动，`man nemuri` 查看手册。
- **Windows**：Installer 安装到当前用户目录，不需要管理员权限，可选把 `nemuri` 加入 PATH（默认不勾选）；Portable ZIP 解压后运行 `nemuri.cmd`。两者都不需要另装运行时。安装包尚未签名，SmartScreen 可能提示未知发布者。

## 从 Music Player 升级

- 命令从 `music-player` 改为 `nemuri`。RPM 会替换旧的 `music-player` 包；Windows 安装程序会原地升级，沿用原来的安装目录和 PATH 设置。
- 第一次启动时，如果 `nemuri` 的配置或数据目录还不存在，就把旧的 `tui-music-player` 目录整个复制过来，包括配置、播放列表、播放位置和收听进度。旧目录原样保留，之后两边互不同步；确认无误后可以删掉旧目录和旧缓存（`~/.cache/tui-music-player`，Windows 为 `%LOCALAPPDATA%\tui-music-player`）。复制失败时程序不启动，并在终端说明原因和两个目录的位置。
- MPRIS 总线名改为 `org.mpris.MediaPlayer2.nemuri`，脚本里的 `playerctl -p music_player` 要改成 `playerctl -p nemuri`。

## 使用

```sh
nemuri                                # 打开设置好的音乐库，首次运行用系统的 Music 目录
nemuri /path/to/music                 # 只在本次运行打开另一个目录
nemuri --set-library /path/to/music   # 永久设置音乐库
```

在程序里按 `?` 查看完整按键帮助，`nemuri --help` 查看命令行参数。扫描不跟随符号链接，放在别处的音乐请把真实文件或硬链接放进音乐库。

## 按键

| 按键 | 操作 |
|---|---|
| `↑/↓`、`j/k` | 选择歌曲 |
| `Enter` | 立即播放选中歌曲，保留队列 |
| `Space` | 暂停/继续 |
| `←/→`、`h/l` | 后退/前进 10 秒 |
| `Shift+←/→`、`H/L` | 后退/前进 60 秒 |
| `0`–`9` | 跳到曲目的 0%–90% 位置 |
| `b` / `B` | 在当前位置加跳转点 / 跳转点面板（10 分钟及以上的录音） |
| `{` / `}` | 跳到上一个/下一个跳转点 |
| `-`、`=` | 音量降低/提高 5% |
| `[`、`]` | 音量降低/提高 1% |
| `m` | 静音 |
| `,`、`.` | 左右平衡往左/往右挪 10 |
| `w` | 互换左右声道 |
| `n/p` | 下一首/上一首历史 |
| `z` | 循环方式：播完停止 / 顺序 / 列表循环 / 单曲循环 |
| `s` | 开/关随机播放 |
| `t` | 睡眠定时：关 / 15 / 30 / 60 / 90 / 120 分钟 / 本条播完时 |
| `v` | 显示/隐藏音频频谱 |
| `y` | 显示/隐藏歌词 |
| `o`/`O` | 排序字段/升降序 |
| `M` | 开/关鼠标 |
| `i` | 开/关隐私模式（系统媒体控制不显示标题） |
| `/` | 实时模糊搜索 |
| `f` | 按文件夹筛选曲库 |
| `r` | 后台重新扫描 |
| `a/A` | 加到队尾/设为下一首 |
| `Q` | 播放队列面板 |
| `P` | 播放列表面板 |
| `?` | 内置完整帮助；终端不够高时用 `↑/↓`、`j/k` 或滚轮滚动 |
| `q` | 退出 |

队列、播放列表等面板里的按键会显示在底栏。鼠标默认开启：单击选中、双击播放、滚轮滚动、点进度条跳转；开启时多数终端可按住 Shift 使用原生选区。

## 用户数据

程序不会在音乐库目录中写入文件：

| 数据 | Linux | Windows |
|---|---|---|
| 配置 | `$XDG_CONFIG_HOME/nemuri/config.kdl` | `%APPDATA%\nemuri\config.kdl` |
| 播放列表 | `$XDG_DATA_HOME/nemuri/playlists/*.json` | `%APPDATA%\nemuri\playlists\*.json` |
| 上次播放位置和文件夹筛选 | `$XDG_DATA_HOME/nemuri/session.toml` | `%APPDATA%\nemuri\session.toml` |
| 长录音的收听进度 | `$XDG_DATA_HOME/nemuri/progress.json` | `%APPDATA%\nemuri\progress.json` |
| 长录音的跳转点 | `$XDG_DATA_HOME/nemuri/marks.json` | `%APPDATA%\nemuri\marks.json` |
| 可删除缓存 | `$XDG_CACHE_HOME/nemuri/library.sqlite3` | `%LOCALAPPDATA%\nemuri\library.sqlite3` |

Linux 没有设置 XDG 变量时，通常对应 `~/.config`、`~/.local/share` 和 `~/.cache`。升级、卸载或删除 Portable 文件都不会删除这些数据。

## 配置文件

所有设置都在 [KDL v2](https://kdl.dev) 格式的 `config.kdl` 里，第一次启动时自动生成一份带说明的配置（1.4.0 之前的 `config.toml` 会被搬进来，旧文件保留）。

```kdl
library "~/Music"

playback {
    volume 80            // 0–100
    muted #false
    repeat "stop"        // "stop"、"none"、"all" 或 "one"
    shuffle #false
    balance 0            // 左右平衡 -100–100，0 居中
    swap-channels #false
}

sort "artist" descending=#false   // "path"、"title"、"artist"、"album" 或 "duration"

interface {
    visualizer #true
    lyrics #true
    mouse #true
    privacy #false
}

sleep {
    fade 30              // 睡眠定时淡出秒数，0–600；0 表示不淡出
}

theme {
    preset "default"
}
```

- 布尔值写作 `#true` / `#false`，字符串加双引号；Windows 路径里的 `\` 写成 `\\` 或改用 `/`。
- 改完重启生效，没写的项用默认值。写错的项只忽略这一项；整个文件有语法错误时本次使用默认设置，也不会改动文件。两种情况都会在底栏提示出错位置。
- 运行中用按键改过的设置会在退出时写回，只改动对应的行，注释和其他内容保持原样，所以播放器运行时也可以放心编辑。`--set-library` 只改写 `library` 一行。

### 主题

`theme {}` 块里先选预设，再按需覆盖个别颜色：

```kdl
theme {
    preset "light"            // "default"、"light" 或 "terminal"
    selection-bg "#C8D3F0"    // 只写想改的颜色
}
```

`default` 适合深色终端，`light` 适合浅色终端，`terminal` 只用终端调色板，跟着终端配色走，16 色终端也能正常显示。

| 键 | 用途 | `default` | `light` | `terminal` |
|---|---|---|---|---|
| `primary` | 标题、曲名、播放图标、进度条 | `#F2F2F2` | `#262626` | `reset` |
| `muted` | 歌手、专辑、时长、帮助与提示 | `#B8B8B8` | `#5C5C5C` | `gray` |
| `border` | 边框与分隔线 | `#8A8A8A` | `#8A8A8A` | `dark-gray` |
| `selection-bg` | 选中行背景 | `#343846` | `#DCE0EA` | `dark-gray` |
| `danger` | 失效歌曲等异常 | `red` | `red` | `red` |
| `spectrum-low` | 频谱低频端 | `#A8A8A8` | `#8A8A8A` | `gray` |
| `spectrum-high` | 频谱高频端 | `#F2F2F2` | `#262626` | `reset` |

颜色可写成 `#RRGGBB`、颜色名（如 `red`、`dark-gray`）、`0`–`255` 的调色板序号或 `reset`（终端默认前景色）。预设名写错时改用 `default`，某项颜色写错时只有这一项沿用预设。

## 构建与打包

在 Fedora 容器中构建，需要 `rust cargo alsa-lib-devel sqlite-devel`，打 RPM 另需 `rpm-build desktop-file-utils`：

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --locked

./packaging/build-rpm.sh   # 生成离线 vendor 归档、RPM 和 SRPM
sudo dnf install ./packaging/rpmbuild/RPMS/x86_64/nemuri-2.0.0-1.fc44.x86_64.rpm
```

Windows 的构建、测试和打包（`setup.exe` 与 Portable ZIP）由 GitHub Actions 的 Windows runner 完成。

## 贡献者

- HZ-TYZQ — 作者与维护者
- Codex — 早期版本的应用图标设计

## 许可证

版权属名：HZ-TYZQ。项目采用 MIT 许可证，详见 `LICENSE`。
