# Android Termux 打包：可行性报告

日期：2026-10-17
依据：从 `feat/v2.4.0`（eb9cd6a）开的实验分支 `exp/termux`

## 结论

可行。Nemuri 自己的代码几乎不用改，难点只在音频输出，照 Termux 里现成的做法可以解决。

- 直接为 Android 编译的 cpal（Rodio 用的音频库）在 Termux 里一启动就崩：它的 Android 后端要向 Java 的
  AudioManager 问缓冲区大小，而 Termux 运行的是普通可执行文件，没有 JVM 环境，`ndk-context` 直接 panic
  （“android context was not initialized”）。已在 Termux 环境里复现。
- termux-packages 里用 cpal 的程序（termusic、chroma-visualizer）都在构建时把 cpal 改成用 ALSA 后端：
  alsa-lib 经 alsa-plugins 交给 Termux 的 PulseAudio，PulseAudio 再用 OpenSL ES 或 AAudio 出声。
  照这个做，在 Termux 的用户空间里实测能正常播放。
- Nemuri 自己改两处：Termux 里默认的音乐目录用 `~/storage/music`；音乐目录打不开时提示先运行 `termux-setup-storage`。
- 还差真机验证：实验环境里没有 Android 的音频服务，PulseAudio 输出到的是空设备。

## 1. 音频输出

崩溃的调用链：`DeviceSinkBuilder::from_default_device()` → cpal 的 `default_output_config()` →
`supported_output_configs()` → `buffer_size_range()` → `AudioManager::get_frames_per_buffer()`（JNI）→
`ndk_context::android_context()` → panic。这一步在打开设备之前，换个音频配置也绕不开，
除非 Nemuri 在 Android 上自己拼配置、直接用 AAudio（见第 4 节第 4 条）。

另外，cpal 的 Android 后端要链接 `libaaudio`，它从 API 26（Android 8）才有，而 Termux 按 API 24 构建。走 ALSA 没有这个问题。

做法和 termusic 的 `build.sh` 一样：复制一份 cpal 源码，把其中的 `"android"` 换成一个不存在的系统名、
`"linux"` 换成 `"android"`，再用 `[patch.crates-io]` 指过去。运行时依赖 `alsa-lib`、`alsa-plugins`
（它把 ALSA 的默认设备设为 PulseAudio）和 `pulseaudio`；SQLite 用 Termux 的 `libsqlite`。

## 2. 实验

### 2.1 构建

`packaging/termux/build-deb.sh [aarch64|x86_64|arm|i686]`，在 dev-fedora 容器里运行：

- 用 Android NDK r30（Termux 现在用的版本）按 API 24 交叉编译；
- 从 Termux 软件源下载 alsa-lib、libsqlite 等的 .deb，解开作为链接用的 sysroot；二进制的 RUNPATH 指向 `$PREFIX/lib`；
- 在一份复制出来的源码里改 cpal，工作区和 `Cargo.lock` 不受影响；
- 打成 Termux 的 .deb：`$PREFIX/bin/nemuri`、man 页、LICENSE 和 README。

二进制只依赖 `libasound.so`、`libsqlite3.so` 和 bionic 的 libc、libm、libdl，没有 libaaudio。aarch64 的包约 1.7 MB。

### 2.2 测试环境

Termux 官方的 termux-docker x86_64 镜像（带 Android 的 bionic 和动态链接器）用 skopeo 取下来解开，
在容器里用用户命名空间 chroot 进去，以非 root 身份用 Termux 的 apt 装依赖和 nemuri 的包。
PulseAudio 加载 null sink，代替 Android 的输出。

### 2.3 结果

| 检查 | 结果 |
| --- | --- |
| `apt install ./nemuri_*.deb` 自动装上依赖 | 通过 |
| 播放 WAV：PulseAudio 里出现 “ALSA plug-in [nemuri]” 的流（float32、双声道、44100 Hz），进度在走，频谱在动 | 通过 |
| 配置、数据、缓存分别在 `~/.config`、`~/.local/share`、`~/.cache` 下；退出时保存会话和位置 | 通过 |
| 不带目录参数第一次启动：用 `~/storage/music`（指向共享存储里的 Music） | 通过 |
| 还没运行 `termux-setup-storage` 时，报错里带提示 | 通过 |
| PulseAudio 没在运行：打开 ALSA 设备时自动拉起 | 通过 |
| 46 列宽的终端（手机竖屏） | 能用；歌词栏不显示，帮助里长的说明被截断 |
| 单元测试和集成测试编成 Android 程序，在 Termux 里运行 | 420 + 21 全部通过（少的 4 个是只在 Linux 上编译的 MPRIS 测试） |
| 不改 cpal 直接编译 | 启动即 panic：“android context was not initialized” |
| `cargo clippy --target x86_64-linux-android` | 没有警告 |

## 3. 代码改动

- `src/config/mod.rs`：Android 上系统没给 Music 目录时用 `~/storage/music`。
- `src/main.rs`：Android 上音乐目录不可用时，报错末尾提示运行 `termux-setup-storage`。
- 系统媒体控制只有 Linux（MPRIS）和 Windows 的实现，Android 上用现有的空实现，不用改。

## 4. 发布途径

1. **自己发 .deb**：GitHub Release 附上 aarch64 的包，用户运行 `apt install ./nemuri_X_aarch64.deb`，依赖从 Termux 源装。升级要手动下载新包。
2. **提交到 termux-packages**：写一个 `build.sh`，cpal 的改法照搬 termusic。之后用 `pkg install nemuri` 安装，跟着软件源升级。要经过 Termux 维护者审核，收不收由他们决定。
3. **`cargo install`**：不行。它会用 cpal 的 Android 后端，启动即崩（见 2.3 节）。
4. **另一条路**：Nemuri 在 Android 上自己拼音频配置，绕开查询缓冲区大小的 JNI 调用，直接用 AAudio。
   好处是不需要 PulseAudio，`cargo install` 也可能能用；代价是只支持 Android 8 及以上，要看 cpal 内部别处会不会再碰 JNI，
   而且这个实验环境里没有 AAudio 服务，没法测。暂不建议。

## 5. 没验证的和风险

- 真机出声：实验里 PulseAudio 输出到 null sink。Termux 的 PulseAudio 默认用 `module-sles-sink`；没有声音时，
  pulseaudio 包安装时提示的办法是改用 `module-aaudio-sink`。
- 熄屏和后台：Android 可能暂停或结束后台的 Termux，长时间播放要 `termux-wake-lock`，并关掉 Termux 的电池优化。
- 没有通知栏和锁屏上的播放控制（Android 上没有 MPRIS）。
- 共享存储走 FUSE，读文件和扫描大目录的速度没测过。Nemuri 只读音乐目录，写的文件都在 Termux 自己的目录里。
- arm（32 位）和 i686 的包没编过，要先 `rustup target add` 对应的目标。
- 包的版本号取自 `Cargo.toml`，现在还是 2.3.0。

## 6. 真机验收

1. 把 `nemuri_2.3.0_aarch64.deb` 拷到手机，在 Termux 里运行 `pkg update`，再 `apt install ./nemuri_2.3.0_aarch64.deb`。
2. 运行 `termux-setup-storage`，允许访问存储。
3. 运行 `nemuri`：确认用的是 `~/storage/music`，能出声，进度和频谱正常。
4. 暂停和继续、快进快退、切歌、调音量；插拔耳机，连接和断开蓝牙。
5. 熄屏后继续播放（必要时先 `termux-wake-lock`）。
6. 退出再打开，确认会话和收听进度都保存了。
