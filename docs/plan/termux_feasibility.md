# Android Termux 打包：可行性报告

日期：2026-10-18
依据：从 `feat/v2.4.0`（eb9cd6a）开的实验分支 `exp/termux`；真机是 OnePlus PLZ110（Android 16，arm64），Termux 0.118.3

## 结论

可行，已在真机上出声。Nemuri 自己的代码改动很小，难点只在音频输出。

- 直接为 Android 编译的 cpal（Rodio 用的音频库）在 Termux 里一启动就崩：它的 Android 后端打开设备前要经 JNI
  向 Java 的 AudioManager 问缓冲区大小，上下文取自 `ndk-context`；Termux 运行的是普通可执行文件，没有 JVM，
  没人初始化它，一取就 panic（“android context was not initialized”）。
- **推荐的做法**：Nemuri 打开设备前给 `ndk-context` 填一个空上下文。cpal 用空指针建 `JavaVM` 时得到错误，
  退回默认设备和未知的缓冲区大小，音频流照样由原生的 AAudio 打开。包只依赖 `libsqlite`，不需要 PulseAudio；
  不用改 cpal，在 Termux 里直接 `cargo build` 也行。代价是要 Android 8 及以上。
- 另一种做法是照 termux-packages 里 termusic 的办法，构建时把 cpal 改成用 ALSA 后端，经 Termux 的 PulseAudio 出声。
  真机上也能出声，但要多装 PulseAudio 等 8 个包，多一个后台进程，自动拉起的 PulseAudio 还会往终端里打一行警告。
  只有要支持 Android 7 时才值得用。
- Nemuri 另外改两处：Termux 里默认的音乐目录用 `~/storage/music`；音乐目录打不开时提示先运行 `termux-setup-storage`。

## 1. 音频输出

崩溃的调用链：`DeviceSinkBuilder::from_default_device()` → cpal 的 `default_output_config()` →
`supported_output_configs()` → `buffer_size_range()` → `AudioManager::get_frames_per_buffer()`（JNI）→
`ndk_context::android_context()` → panic。设备列表（`AudioDeviceInfo::request`）和音频特性查询也走同一条路。

### 1.1 AAudio：给一个空上下文（推荐）

cpal 0.17.3 里所有 JNI 调用都经过 `with_attached`，它先调用 `jni::JavaVM::from_raw(vm)`；
jni 0.21 对空指针返回 `Err(NullPtr)`，不会解引用。所以在 `Player::new` 里先调用一次
`ndk_context::initialize_android_context(null, null)`（`Once` 保证只调用一次），之后：

- 缓冲区大小查询失败 → `SupportedBufferSize::Unknown`，Rodio 照常按约 50 ms 设缓冲；
- 设备列表查询失败 → 只有默认设备；
- 音频流由 `ndk::audio::AudioStreamBuilder`（原生 AAudio）创建，不经过 Java。

依赖 cpal 的内部做法：升级 cpal 时要确认所有 JNI 调用仍先经过 `JavaVM::from_raw`。
`libaaudio` 从 API 26 才有，所以按 API 26 链接，包只能装在 Android 8 及以上。

### 1.2 ALSA 经 PulseAudio

和 termusic、chroma-visualizer 的 `build.sh` 一样：复制一份 cpal 源码，把其中的 `"android"` 换成一个不存在的系统名、
`"linux"` 换成 `"android"`，再用 `[patch.crates-io]` 指过去。运行时依赖 `alsa-lib`、`alsa-plugins`
（它把 ALSA 的默认设备设为 PulseAudio）和 `pulseaudio`；PulseAudio 默认用 OpenSL ES 出声。按 API 24 构建，Android 7 也能用。

## 2. 构建

`packaging/termux/build.sh {deb|tests} [aarch64|x86_64|arm|i686] [aaudio|alsa]`（默认 aarch64、aaudio），本地在 dev-fedora 容器里运行。
`deb` 打包，`tests` 只编出测试程序，放到 Termux 里跑：

- 用 Android NDK r30（Termux 现在用的版本）交叉编译，aaudio 按 API 26、alsa 按 API 24；
- 从 Termux 软件源下载 libsqlite（alsa 还有 alsa-lib 等）的 .deb，解开作为链接用的 sysroot；二进制的 RUNPATH 指向 `$PREFIX/lib`；
- 在一份复制出来的源码里构建，alsa 改 cpal 也只改这份，工作区和 `Cargo.lock` 不受影响；
- 打成 Termux 的 .deb：`$PREFIX/bin/nemuri`、man 页、LICENSE 和 README。

二进制依赖：aaudio 是 `libaaudio.so`、`liblog.so`、`libsqlite3.so` 和 bionic 的 libc、libm、libdl；
alsa 把前两个换成 `libasound.so`。aarch64 的包约 1.7 MB。

## 3. 测试

### 3.1 真机（adb）

用 `adb shell run-as com.termux` 以 Termux 的用户身份运行（这个 Termux 是 GitHub 上的可调试版本），
每次用单独的 HOME，用 `script` 给 Nemuri 一个终端、从管道送按键；同时用 `dumpsys audio` 看系统里的播放器。
音乐是 `~/Music` 里的一个 FLAC。

| 检查 | AAudio | ALSA |
| --- | --- | --- |
| 系统里出现 `com.termux` 的播放器，`state:started`、`USAGE_MEDIA`、44100 Hz 双声道 | 通过（type:AAudio） | 通过（type:OpenSL ES，来自 PulseAudio） |
| 播 8 秒后退出，保存的位置约 8 秒，退出码 0 | 7970 ms | 7975 ms |
| 标准错误输出 | 空 | PulseAudio 的一行警告（`/proc/self/exe does not point to ...`） |
| PulseAudio 没在运行 | 不需要 | 打开 ALSA 设备时自动拉起 |
| 播 3 秒、暂停 4 秒、继续 3 秒、快进 10 秒、2 秒后退出：位置约 18 秒 | 18060 ms | 没测 |

暂停时系统里的播放器仍是 `started`：Rodio 暂停时音频流不停，送的是静音，和桌面上一样。

### 3.2 termux-docker（容器里）

Termux 官方的 termux-docker x86_64 镜像（带 Android 的 bionic 和动态链接器）用 skopeo 取下来解开，
在容器里用用户命名空间 chroot 进去，以非 root 身份用 Termux 的 apt 装依赖和 nemuri 的包。
这里没有 Android 的音频服务，只测了 ALSA 那条路，PulseAudio 输出到 null sink。

| 检查 | 结果 |
| --- | --- |
| `apt install ./nemuri_*.deb` 自动装上依赖 | 通过 |
| 播放 WAV：PulseAudio 里出现 “ALSA plug-in [nemuri]” 的流，进度在走，频谱在动 | 通过 |
| 配置、数据、缓存分别在 `~/.config`、`~/.local/share`、`~/.cache` 下；退出时保存会话和位置 | 通过 |
| 不带目录参数第一次启动：用 `~/storage/music`（指向共享存储里的 Music） | 通过 |
| 还没运行 `termux-setup-storage` 时，报错里带提示 | 通过 |
| 46 列宽的终端（手机竖屏） | 能用；歌词栏不显示，帮助里长的说明被截断 |
| 单元测试和集成测试编成 Android 程序，在 Termux 里运行 | 420 + 21 全部通过（少的 4 个是只在 Linux 上编译的 MPRIS 测试） |
| 不改 cpal、也不填空上下文 | 启动即 panic：“android context was not initialized” |

`cargo clippy --target aarch64-linux-android` 没有警告。

## 4. 代码改动

- `src/player/backend.rs`、`Cargo.toml`：Android 上打开设备前给 `ndk-context` 填空上下文（只在 Android 上依赖 `ndk-context`，它本来就在 cpal 的依赖里）。
- `src/config/mod.rs`：Android 上系统没给 Music 目录时用 `~/storage/music`。
- `src/main.rs`：Android 上音乐目录不可用时，报错末尾提示运行 `termux-setup-storage`。
- 系统媒体控制只有 Linux（MPRIS）和 Windows 的实现，Android 上用现有的空实现，不用改。

## 5. 发布途径

1. **自己发 .deb**（已接入发布流程，见第 8 节）：GitHub Release 附上 aarch64 和 x86_64 的包，
   用户运行 `apt install ./nemuri_X_aarch64.deb`。升级要手动下载新包。
2. **在 Termux 里从源码装**：有了空上下文，不用改 cpal，`pkg install rust` 之后 `cargo install --git https://github.com/HZ-TYZQ/nemuri` 应该就行。
   这次没有在手机上构建；手机上之前那个 2.2.0 测试包用的是同样的空上下文，就是在 Termux 里直接 `cargo build` 出来的。
3. **提交到 termux-packages**：之后用 `pkg install nemuri` 安装，跟着软件源升级。termux-packages 按 API 24 构建，
   用 AAudio 要像 cmus、pulseaudio 那样从 API 26 的 sysroot 拿 `libaaudio.so` 链接；或者照 termusic 改用 ALSA。
   要经过 Termux 维护者审核，收不收由他们决定。

## 6. 没验证的和风险

- 只在 adb 的 run-as 环境里测过声音，没在 Termux 应用自己的会话里测；run-as 下读不了共享存储（`~/storage/music` 报权限不足），所以真机上也没测从共享存储读音乐。
- 插拔耳机、切换蓝牙时 AAudio 流会断开，要靠 Nemuri 现有的输出故障恢复重新打开，没测。
- 熄屏和后台：Android 可能暂停或结束后台的 Termux，长时间播放要 `termux-wake-lock`，并关掉 Termux 的电池优化。
- 没有通知栏和锁屏上的播放控制（Android 上没有 MPRIS）。
- 共享存储走 FUSE，读文件和扫描大目录的速度没测过。Nemuri 只读音乐目录，写的文件都在 Termux 自己的目录里。
- arm（32 位）和 i686 的包没编过，要先 `rustup target add` 对应的目标。

## 7. 真机验收（在 Termux 应用里）

1. 把 `nemuri_2.3.0_aarch64.deb`（aaudio）拷到手机，在 Termux 里运行 `apt install ./nemuri_2.3.0_aarch64.deb`。
2. 运行 `termux-setup-storage`，允许访问存储。
3. 运行 `nemuri`：确认用的是 `~/storage/music`，能出声，进度和频谱正常。
4. 暂停和继续、快进快退、切歌、调音量；插拔耳机，连接和断开蓝牙。
5. 熄屏后继续播放（必要时先 `termux-wake-lock`）。
6. 退出再打开，确认会话和收听进度都保存了。

## 8. GitHub Actions

都在 ubuntu-24.04 上，用 runner 自带的 NDK 30（`$ANDROID_NDK_LATEST_HOME`，和 Termux 用的同一版；默认的 `$ANDROID_NDK_HOME` 是 27）。
测试和安装在 Termux 官方的 `termux/termux-docker:x86_64` 容器里，以 Termux 自己的非 root 用户运行。
容器里没有 `libaaudio.so`，挂进 NDK 里的同名存根，只让程序能装载；测试用假后端和无设备的混音器，不打开音频设备。

| 工作流 | 任务 | 做什么 |
| --- | --- | --- |
| `ci.yml`（每个 PR、推送 master） | `android` | `cargo clippy --target aarch64-linux-android`；`build.sh tests x86_64` 编出测试程序，在容器里装上 `libsqlite` 后逐个运行 |
| `packaging.yml`（打包相关文件改动时） | `termux-deb` | `build.sh deb` 打 aarch64、x86_64 两个包；在容器里 `apt install` x86_64 的包（顺带装上依赖），运行 `nemuri --version`、`--help`；上传为 `termux-debs-ci` |
| `release.yml`（标签） | `termux` | 同 `termux-deb`，上传为 `termux-debs`，`publish` 把它们放进发布页和 `SHA256SUMS.txt`；还没有 `packaging/termux/build.sh` 的旧标签跳过这一步 |

`build.sh` 没有 `CARGO_TARGET_DIR` 时（runner 上）在 `target/termux` 下工作。
怎么出声仍然只能在手机上验。

本地照着跑过一遍：在 termux-docker 的根文件系统里挂上同一个存根，测试程序 422 + 21 个全部通过，x86_64 的包装上后能启动。
其间发现一个和 Android 无关的不稳定测试：`plain_lyrics_fill_the_pane_and_scroll_with_the_progress` 的歌词行是 `L01`…`L40`，
检查画面上没有 “L2”，而曲库标题里的临时目录名是随机的字母和数字，约 770 次里有一次碰巧含有 “L2”。
已改用 `第01行`…`第40行`。
