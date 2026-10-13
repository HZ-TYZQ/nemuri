# Music Player

Music Player 是一个用 Rust 编写的终端音乐库播放器，以 ASMR 这类按创作者分文件夹整理的长录音为主要场景，也照常播放普通音乐。它使用 Rodio 播放、Lofty 读取媒体信息，用 SQLite 保存可随时重建的增量索引，并把配置与播放列表保存在平台标准用户目录中。

## 功能

- 后台递归扫描音乐库，界面不会因大型目录而停止响应
- 曲目以磁盘上的文件名原样显示，第二行是所在文件夹、歌手、专辑和格式；另有时长和实时播放进度
- 按文件夹筛选：`f` 列出音乐库第一层的各个文件夹及曲目数，只看其中一个时，搜索和顺序、循环、随机播放都限定在该文件夹内
- 播放、平滑暂停、前后跳转、音量和静音
- 内置实时音频频谱，将 50–5000 Hz 对数映射为最多 32 根柱，并支持记忆开关状态
- 循环（播完停止 / 顺序 / 列表 / 单曲）与随机可独立组合；默认播完一首就停，适合听着入睡；随机按整轮重洗
- 可被 Linux 媒体控制器（MPRIS）和 Windows 系统媒体控件（SMTC）识别与遥控
- 鼠标可用：单击选中、双击播放、滚轮滚动、点进度条跳转；可随时关闭
- 曲库可按路径、标题、歌手、专辑或时长排序，升降序独立切换并随配置保存
- Unicode 友好的实时模糊搜索，覆盖文件名和相对路径，以及标签里的标题、歌手和专辑
- 临时播放队列可查看、跳播、移除、重排和清空，另有持久命名播放列表
- 播放列表中的失效歌曲会被标记并跳过；删除列表绝不删除音乐文件
- 歌词：读取同名 `.lrc` 或内嵌歌词，LRC 同步歌词随播放高亮滚动，纯文本歌词整段显示；宽终端在曲库右侧常驻歌词面板，窄终端在播放区边框显示同步歌词的当前行
- 退出时记住当前歌曲与进度，下次打开同一音乐库时暂停在原处，按 `Space` 续播

默认界面继承终端自身的背景，不绘制应用专属的全局背景。主要文字使用柔和白色，次要信息和边框采用分层灰白，音频频谱使用统一的灰白渐变。浅色终端或想沿用终端配色时，可以在配置中换用其他预设或逐项改色，见[配置文件](#配置文件)。

## 安装与运行要求

项目正式支持 Fedora Linux 与 Windows 11 x86_64：

- Fedora RPM 使用系统的 ALSA/PipeWire 与 SQLite
- Windows Installer 与 Portable ZIP 把 SQLite 编入程序，通过 WASAPI 输出，用户无需另行安装解码运行时

Windows Installer 默认安装到当前用户目录，不要求管理员权限，并创建开始菜单入口。Portable ZIP 解压后运行 `music-player.cmd`。两种 Windows 发行包都可在 Windows Terminal 中使用。

第一版 Windows 包尚未进行 Authenticode 签名，Windows Defender SmartScreen 可能显示未知发布者提示。请只从项目 GitHub Release 下载，并使用同一 Release 中的 `SHA256SUMS.txt` 核对文件。

Fedora 44 的 RPM 会自动声明运行依赖。

```sh
sudo dnf install \
  rust cargo \
  alsa-lib-devel \
  sqlite-devel
```

## 使用

```sh
# 首次运行默认使用系统的 Music 目录
cargo run --locked

# 只在本次运行打开另一个目录
cargo run --locked -- /path/to/music

# 永久设置主音乐库
cargo run --locked -- --set-library /path/to/music
```

查看完整命令行帮助：

```sh
music-player --help
```

通过 RPM 安装后，也可以在桌面应用菜单中搜索“Music Player”或“音乐播放器”。菜单项会使用桌面环境配置的默认终端启动程序，不要求安装某个特定的终端模拟器。

扫描不会跟随音乐库目录内部的符号链接：目录软链接会被跳过以避免循环，指向音频文件的软链接目前也不会被收录。如果音乐存放在其他位置，请把真实文件或硬链接放进音乐库目录。

## 按键

| 按键 | 操作 |
|---|---|
| `↑/↓`、`j/k` | 选择歌曲 |
| `Enter` | 立即播放选中歌曲，保留队列 |
| `Space` | 暂停/继续 |
| `←/→`、`h/l` | 后退/前进 10 秒 |
| `Shift+←/→`、`H/L` | 后退/前进 60 秒 |
| `0`–`9` | 跳到曲目的 0%–90% 位置 |
| `-`、`=` | 音量降低/提高 5% |
| `[`、`]` | 音量降低/提高 1% |
| `m` | 静音 |
| `n/p` | 下一首/上一首历史 |
| `z` | 循环方式：播完停止 / 顺序 / 列表循环 / 单曲循环 |
| `s` | 开/关随机播放 |
| `v` | 显示/隐藏音频频谱 |
| `y` | 显示/隐藏歌词 |
| `o`/`O` | 排序字段/升降序 |
| `M` | 开/关鼠标 |
| `/` | 实时模糊搜索 |
| `f` | 按文件夹筛选曲库 |
| `r` | 后台重新扫描 |
| `a/A` | 加到队尾/设为下一首 |
| `Q` | 播放队列面板 |
| `P` | 播放列表面板 |
| `?` | 内置完整帮助 |
| `q` | 退出 |

鼠标默认开启：在曲库或弹层列表中单击选中、双击播放，滚轮上下移动选择，点击进度条跳转到对应位置。开启鼠标后终端自身的拖拽选区会被接管，多数终端可按住 Shift 恢复原生选择；也可以用 `M` 关掉鼠标，该选择会随配置保存。

曲库显示的名称就是磁盘上的文件名（不含扩展名），原样显示，不使用标签里的标题：下载来的录音常带无意义的标签标题，写在文件名里的备注也只有文件名里有。改了文件名，下次扫描（启动或按 `r`）后即显示新名字；标签里的标题、歌手和专辑仍然可以搜索。每首的第二行依次是所在文件夹（音乐库根目录下的第一层文件夹，名称原样显示；直接放在根目录的显示“(根目录)”）、歌手、专辑和格式，没有的项省略。

排序会同时改变顺序播放的走向：播放顺序始终跟随列表当前顺序。按“标题”排序即按显示的文件名排序。缺失的歌手、专辑或时长永远排在最后，不随升降序翻转。按专辑排序时，专辑内按碟号和音轨号排列。

按 `f` 打开文件夹面板，列出“全部”和每个文件夹的曲目数（没有音频的空文件夹不列出），`Enter` 只看选中的文件夹，选“全部”取消筛选。筛选后标题栏显示文件夹名和“筛选数/总数”，搜索只在该文件夹内进行，顺序播放、列表循环和随机也不会离开它；正在播放的曲目不在该文件夹里时，下一首从该文件夹的第一首开始。搜索只用来定位曲目，不改变播放范围。筛选只在本次运行有效；重扫后该文件夹不存在时自动改回全部。

默认的循环方式“播完停止”在一首自然播完后停下，底栏会说明原因；按 `n` 仍可手动切到下一首，已排进队列的曲目照常播放。系统媒体控制只有“不循环”，“播完停止”和“顺序”都报告为不循环。

播放队列面板中，`Enter` 跳到该条目播放并丢弃它之前的条目，`d` 移除单项，`J/K` 上下移动条目，`c` 清空队列。队列只保存在本次运行中，退出后不保留；已经不在曲库中的条目会被标记，播放时自动跳过。

播放列表面板中，`c` 新建、`a` 加入当前选中歌曲、`Enter` 查看内容、`x` 确认删除。列表内容中可按 `Enter` 从选中项开始播放，按 `d` 只从列表移除该项。

## 用户数据

程序不会在音乐库目录中写入文件：

| 数据 | Linux | Windows |
|---|---|---|
| 配置 | `$XDG_CONFIG_HOME/tui-music-player/config.kdl` | `%APPDATA%\tui-music-player\config.kdl` |
| 播放列表 | `$XDG_DATA_HOME/tui-music-player/playlists/*.json` | `%APPDATA%\tui-music-player\playlists\*.json` |
| 上次播放位置 | `$XDG_DATA_HOME/tui-music-player/session.toml` | `%APPDATA%\tui-music-player\session.toml` |
| 可删除缓存 | `$XDG_CACHE_HOME/tui-music-player/library.sqlite3` | `%LOCALAPPDATA%\tui-music-player\library.sqlite3` |

Linux 没有显式设置 XDG 基础目录时，通常对应 `~/.config`、`~/.local/share` 和 `~/.cache`。Windows Installer 和 Portable ZIP 使用相同的 AppData 目录；升级、删除 Portable 文件或卸载程序都不会删除这些用户数据。

频谱默认开启，`v` 的开关状态与音量、静音等设置一起在退出时写回配置文件。频谱从当前播放的 PCM 分析，不会采集麦克风或其他应用的声音。

支持的音频格式：MP3、FLAC、WAV、OGG/OGA Vorbis、M4A/AAC、AAC ADTS、AIFF。Opus 暂缓支持；APE 与 WMA 不再支持。

歌词来自与音频同名的 `.lrc`（如 `歌曲.flac` 旁的 `歌曲.lrc`）或音频标签中内嵌的歌词（FLAC/OGG 的 `LYRICS`、MP3 的 `USLT`、M4A 的 `©lyr`）。带时间标签的同步歌词优先于纯文本歌词；同一种里同名 `.lrc` 优先，`.lrc` 读不了、无法解码或没有时间标签时回退到内嵌歌词。纯文本歌词在面板中整段居中显示，放不下时随播放进度滚动；窄终端只显示同步歌词。`.lrc` 可为 UTF-8、GBK，或带 BOM 的 UTF-16；Big5、Shift_JIS 等其他编码不受支持，可能显示为乱码。支持一行多个时间标签、`[offset:±毫秒]` 和逐字时间标签；同一时刻的多行（如原文与译文）一起高亮。曲库区域不少于 96 列时，歌词面板固定在右侧，当前曲目没有歌词时显示“暂无歌词”，切歌不会改变曲库宽度；按 `y` 关闭歌词后曲库占满整行。新放入的 `.lrc` 在按 `r` 重扫后生效。`y` 的开关状态随配置保存为 `interface` 里的 `lyrics`。

播放区域的图标表示按下 `Space` 后将执行的操作：播放中显示 `⏸︎`，暂停时显示 `⏵`。暂停符号附带文本样式选择符，实际字形由终端和字体决定。

## 配置文件

所有设置都在 `config.kdl` 里（位置见[用户数据](#用户数据)），使用 [KDL v2](https://kdl.dev) 格式。第一次启动时程序会生成一份带说明的配置；如果有 1.4.0 之前的 `config.toml`，其中的设置会搬进新文件，旧文件原样保留，退回旧版本时还能用。

```kdl
library "~/Music"

playback {
    volume 80            // 0–100
    muted #false
    repeat "stop"        // "stop"、"none"、"all" 或 "one"
    shuffle #false
}

sort "artist" descending=#false   // "path"、"title"、"artist"、"album" 或 "duration"

interface {
    visualizer #true     // 频谱，按 v 开关
    lyrics #true         // 歌词面板，按 y 开关
    mouse #true          // 鼠标，按 M 开关
}

theme {
    preset "default"
}
```

- 布尔值写作 `#true` / `#false`，字符串要加双引号；路径可以用 `~` 开头表示家目录，Windows 路径里的 `\` 要写成 `\\`，或改用 `/`。
- 改完重启播放器生效。没写的项使用默认值。
- 运行中用按键改过的设置（音量、静音、循环、随机、排序和三个开关）会在退出时写回文件：程序重新读取文件，只改动这几项所在的行，其余内容和注释保持原样；文件里没有的项会补在所属的块末尾。播放器运行时也可以放心编辑配置，退出时不会覆盖你改的其他项。
- 某一项写错（值不对、键名拼错）时只忽略这一项，启动时在底栏提示行号。整个文件有语法错误时本次使用默认设置并提示出错的行列，退出时也不会改动这个文件。
- `--set-library PATH` 只改写 `library` 这一行。

### 主题

`theme {}` 块里先选一个预设，再按需改其中几种颜色：

```kdl
theme {
    preset "light"            // "default"、"light" 或 "terminal"
    selection-bg "#C8D3F0"    // 只写想改的颜色，其余沿用预设
}
```

- `default`：深色终端用的灰白层级，即默认界面
- `light`：浅色终端用的深灰层级
- `terminal`：只用终端调色板里的颜色，跟着终端配色走；只支持 16 色的终端也能正常显示

| 键 | 用途 | `default` | `light` | `terminal` |
|---|---|---|---|---|
| `primary` | 标题、曲名、播放图标、进度条 | `#F2F2F2` | `#262626` | `reset` |
| `muted` | 歌手、专辑、时长、帮助与提示 | `#B8B8B8` | `#5C5C5C` | `gray` |
| `border` | 边框与分隔线 | `#8A8A8A` | `#8A8A8A` | `dark-gray` |
| `selection-bg` | 选中行背景 | `#343846` | `#DCE0EA` | `dark-gray` |
| `danger` | 失效歌曲等异常 | `red` | `red` | `red` |
| `spectrum-low` | 频谱低频端 | `#A8A8A8` | `#8A8A8A` | `gray` |
| `spectrum-high` | 频谱高频端 | `#F2F2F2` | `#262626` | `reset` |

颜色可以写成 `#RRGGBB`、颜色名（如 `red`、`dark-gray`、`light-blue`）、`0`–`255` 的调色板序号，或 `reset`（终端默认前景色），都要加双引号。频谱两端都是 `#RRGGBB` 时按频率渐变，否则低频一半和高频一半各用一端的颜色。主题没有背景色这一项，播放器始终继承终端背景。预设名写错时改用 `default`，某项颜色写错时只有这一项沿用预设。

## 测试、RPM 与 Windows 发行包

构建 RPM 还需要 Fedora 的打包工具和桌面入口检查工具：

```sh
sudo dnf install rpm-build desktop-file-utils
```

建议容器中运行：

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets --locked

./packaging/build-rpm.sh
sudo dnf install ./packaging/rpmbuild/RPMS/x86_64/music-player-1.4.0-1.fc44.x86_64.rpm
```

RPM 构建脚本会生成离线 vendor 归档、二进制 RPM 和 SRPM。安装后的 RPM 同时提供命令行程序、man 手册、桌面菜单入口、可缩放 SVG 图标和 48 px 兼容图标。升级使用 `sudo dnf upgrade <RPM 路径>`，卸载使用 `sudo dnf remove music-player`。

Windows MSVC 构建、测试和打包由 GitHub Actions 的 Windows runner 完成，生成：

- `music-player-<版本>-windows-x86_64-setup.exe`
- `music-player-<版本>-windows-x86_64-portable.zip`

Installer 的“添加到当前用户 PATH”选项默认不勾选。启用后可以在新打开的终端中直接运行 `music-player`，卸载时只移除应用自己的 PATH 条目。

## 贡献者

- HZ-TYZQ — 作者与维护者
- Codex — 早期版本的应用图标设计

## 许可证

版权属名：HZ-TYZQ。项目采用 MIT 许可证，详见 `LICENSE`。
