# Rodio / Symphonia / Lofty 迁移可行性报告

状态：探索完成，待决策（**尚未开始迁移**）
日期：2026-08-16
范围：纯 PoC。未改正式播放路径，未删 GStreamer，未动 CI/packaging。
PoC 产物（均未提交）：`examples/backend_poc.rs`、`examples/lofty_poc.rs`；
测试媒体 21 个在 `Temp/test-media/`（`Temp/gen-test-media.sh` 可复现；
APE 由 Fedora 仓库 `mac` 编码）；rodio/lofty/symphonia/cpal 源码快照在
`Temp/vendor-rodio/`。
注：工作区 Cargo.toml 已有用户加入的 `rodio = "0.22.2"`、`lofty = "0.25.1"`
（默认 feature），本报告全部结论基于这两个确切版本。

---

## 1. 当前架构与 GStreamer 耦合审计（master `a8f8093`）

GStreamer 在项目里的全部职责：

| 职责 | 位置 | 用法 |
| --- | --- | --- |
| 解码/播放/暂停/停止/seek/position/duration/音量/静音 | `src/player.rs` | `gstreamer-play` Play 管线 |
| EOS / Error / StateChanged 异步事件 | `src/player.rs` | Play 信号适配器 → mpsc |
| 频谱数据 | `src/player.rs` | `spectrum` 元素挂 `audio-filter`，bus sync handler 抽 magnitude + caps 里的 rate |
| metadata / duration / 音频流存在性检查 | `src/library.rs` | `gstreamer-pbutils` **Discoverer**（每文件 3s 超时），tag 优先级 = 全局 tags → 首个音频流 tags → 文件名兜底；`format` 列其实只用文件扩展名，不经 GStreamer |
| 构建期 | Cargo.toml | `gstreamer` / `gstreamer-pbutils` / `gstreamer-play` 三个 -sys 绑定，需系统 glib/gstreamer 开发包 |
| CI | `.github/workflows/ci.yml` | Linux 装 gstreamer devel；Windows 下载安装 GStreamer MSVC runtime+devel |
| Windows 发行 | `packaging/windows/` | 捆绑 GStreamer runtime DLL、LGPL 合规文件（SOURCE-CODE-OFFER、THIRD-PARTY-NOTICES）、安装器 ~194MB |
| RPM | `packaging/rpmbuild/` | Requires gstreamer1-plugins-* 等 |

**App 没有绕过 Player 直接使用 GStreamer**（全仓库只有 player.rs 与
library.rs 引入 gst 系 crate）。Player 对 App 暴露的事实接口：

```text
new / play / toggle_pause / stop / position / duration / seek_relative
set_volume / volume / set_muted / is_muted / state / current_path
set_spectrum_enabled / drain_events
事件: EndOfStream / Error(String) / StateChanged(PlayState) / SpectrumFrame{magnitudes, sample_rate}
```

**结论：接口可以完全保持不变、只换内部实现。** PoC 逐项验证了每个方法的
Rodio 对应物（见 §3 功能矩阵）。

## 2. 新依赖拓扑

Cargo.lock 实测（非文档推测）：

```text
rodio 0.22.2
├─ symphonia 0.5.5        ← 锁里只有这一个 symphonia 实例
│   ├─ codec: flac / mp3 / aac / pcm / vorbis（默认 feature 集）
│   └─ format: isomp4 / ogg / riff(wav)
├─ cpal 0.17.3
│   └─ Linux: alsa 0.11（构建需 alsa-lib-devel + pkg-config；容器已具备）
└─ dasp_sample 等纯 Rust

lofty 0.25.1（纯 Rust，无 native 依赖）
```

### 问题 A：能否完全不直接调用 Symphonia API？

**能。** 解码走 `rodio::decoder::DecoderBuilder`（内部即 Symphonia）；PCM tap
是对 `rodio::Source` 的普通包装器；seek 走 `Source::try_seek`；格式提示走
`with_hint`。整个 PoC 没有一行直接 `use symphonia`。

### 问题 B：什么功能会迫使直接依赖 Symphonia？

PoC 范围内**没有**。理论上只有这些情形才需要：自定义 demux/codec（不需要）、
绕过 rodio 的帧裁剪控制（gapless 已由 builder 暴露）、或启用 rodio 没有转发的
symphonia feature——但 rodio 0.22.2 把全部相关 feature 都转发了
（`symphonia-aiff` / `symphonia-adpcm` / `symphonia-alac` / `symphonia-mkv` /
`symphonia-all` …），启用它们只是给 rodio 加 feature，不产生第二个依赖实例。

### 建议：方案 A —— 只显式依赖 Rodio + Lofty

```toml
rodio = { version = "0.22.2", features = ["symphonia-aiff"] }  # AIFF 见 §4
lofty = "0.25.1"
```

`cargo tree -d` 视角：symphonia 全仓库单一实例 0.5.5，无重复。
（方案 B「显式加同版本 symphonia」当前无必要；方案 C「独立 0.6」明确违反
原则 7，仅在未来确需 symphonia 0.6 独有特性时再议。）

## 3. 功能矩阵（全部实机/PoC 实测）

| 能力 | 结果 | 实测事实 |
| --- | --- | --- |
| play | ✅ PASS | `DecoderBuilder::build()` + `Player::append` |
| pause / resume | ✅ PASS | `pause()` 后 `get_pos()` 精确冻结（±0ms）；`play()` 恢复 |
| stop → 再播放 | ✅ PASS | `stop()` 仅置标志（~100ns）；随后 `append` 阻塞 ~9-25ms 等旧源冲刷，可接受 |
| seek ±10s | ✅ PASS | `try_seek` 成功；单次 ~130-160ms（FLAC，含 symphonia 重定位+缓冲回填）；连续 50 次 seek 无卡死（~33ms/次） |
| seek → 0 | ✅ PASS | 正常 |
| seek → duration（精确结尾） | ⚠️ PARTIAL | **返回 Err(UnexpectedEof) 且源终止 → `empty()`=true**。应用层应把 seek 目标 clamp 到 `duration - ε`，或将此视同自然 EOS（自动下一首，UX 上其实合理） |
| seek 超出 duration | ⚠️ PARTIAL | duration 已知时饱和到结尾 → 行为同上 |
| 暂停中 seek | ✅ PASS | pos 正确更新，恢复后从新位置播放 |
| 切歌（A 播放中直接 B） | ✅ PASS | 无双播放（队列恒为 1）；**注意 `get_pos` 有 ~数十 ms 的惰性，切换瞬间仍显示旧位置**——App 切歌时应本地重置显示位置 |
| position | ✅ PASS | `get_pos` 每 5ms 由音频线程刷新；切歌/停止后清零 |
| duration | ✅ PASS | `Decoder::total_duration` 与 Lofty 相差 ≤31ms（MP3 含 padding）；建议：播放中用 Rodio（与 seek 饱和语义一致），列表用 `Track.duration`（Lofty 扫描期已存） |
| volume / mute | ✅ PASS | `set_volume(0..=1)`；mute 用「记忆音量 + 置 0」即可（rodio 无独立 mute） |
| EOS | ✅ PASS | 轮询 `empty()` 转变：自然结束**恰好一次**（0.5s 文件实测 1 次转变）；`stop()`/切歌也由 App 发起，天然可区分——不需要 generation id，App 自知是否下达过 stop/switch 命令；`EmptyCallback` 存在但对 50Hz 轮询循环非必需 |
| 打开期错误 | ✅ PASS | 不存在→IO error；空文件→`UnrecognizedFormat`；垃圾→probe IO error；全部在 `play()` 同步返回 |
| 播放中解码错误 | ⚠️ 语义变化 | rodio 的 SymphoniaDecoder：**单 packet 解码错误静默跳过**；硬错误（IO/Reset/Limit）→ 迭代器 None = **静默提前 EOS，无 Error 事件**。截断 MP3 实测：报全时长 10s、播到 5s 处静默结束 |
| 输出设备错误 | ✅ PASS | `DeviceSinkBuilder::with_error_callback` 可接 `PlayerEvent::Error`；`open_sink_or_fallback` 提供设备回退 |
| PCM tap | ✅ PASS | 自定义 `Source` 包装器，2048 样本/批经 latest-wins 槽送分析线程；1s 实测 产 43 批/分析 22/丢 23，**音频路径零阻塞** |
| headless 测试 | ✅ PASS | `mixer(2, 44100)` + `Player::connect_new` + 手动消费 `MixerSource`：无设备完成 play/pause/seek/EOS 全链路，且**可全速快放**（0.5s 文件 20ms 播完）——比 GStreamer fakesink 更快更确定 |

### §3 PoC 备忘（DecoderBuilder 细节）

- `with_byte_len(file.len())`：**应显式提供**——MP3/Vorbis 的 duration 计算与
  可靠 seek 依赖它（builder Settings 文档明示）。
- `with_seekable(true)`：**必须**，默认 false，`try_seek` 才不报 NotSupported。
- `with_hint(扩展名)`：可有可无，内容嗅探已足够（无扩展名 FLAC 照样解码）；
  给了更稳，成本为零。
- 生命周期：`Player::drop` 停掉其所有声音；`MixerDeviceSink::drop` 停音频并打
  一行警告日志（`log_on_drop(false)` 可关）。`Player::detach` 存在但不需要。
- 设备释放即停播，无泄漏线程。

## 4. 格式矩阵（真实样本逐个实测）

| Format | Lofty metadata | Rodio 解码 | Seek | Duration | 备注 |
| --- | --- | --- | --- | --- | --- |
| MP3 (CBR) | ✅ Id3v2 | ✅ | ✅ | ✅ 10.031s | |
| MP3 (VBR) | ✅ | ✅ | ✅ | ✅ | VBR 时长精确 |
| MP3 双标签 | ✅ primary=Id3v2 优先于 Id3v1 | ✅ | ✅ | ✅ | 优先级符合预期 |
| FLAC | ✅ VorbisComments | ✅ | ✅ | ✅ | |
| FLAC 96kHz | ✅ | ✅ | ✅ | ✅ | |
| WAV | ✅（仅 `first_tag`=RiffInfo，`primary_tag`=None） | ✅ | ✅ | ✅ | precedence 必须 primary→first |
| WAV 96kHz | ✅ | ✅ | ✅ | ✅ | |
| OGG Vorbis | ✅ | ✅ | ✅ | ✅ | |
| Opus (ogg) | ✅ VorbisComments | ❌ **UnrecognizedFormat** | — | — | symphonia 0.5.5 无 opus codec（`all-codecs` 也无；0 处提及） |
| M4A/AAC | ✅ Mp4Ilst | ✅ | ✅ | ✅ | |
| AAC ADTS | ✅（ADTS 无标签容器，tags=0 属正常） | ✅ | ✅ | ✅ | |
| AIFF | ✅（AiffText 仅 `first_tag`） | ⚠️ 默认 feature 下 IO error；**开 `symphonia-aiff` 后全通过**（完整解码 10s、seek OK） | ✅* | ✅ | *开 feature 后 |
| APE | ✅（时长/流信息正常） | ❌ **假阳性**：open 成功、报 10s，但解出 0.7s 后静默终止 | — | — | 比明确失败更糟，迁移时必须按扩展名/sniff 显式拦截 |
| WMA | ❌ 对本样本（ffmpeg 混流 ASF）`failed to parse file`（ffprobe 读同文件正常） | ❌ UnrecognizedFormat | — | — | |
| 无扩展名 | ✅ 内容嗅探正确（FLAC） | ✅ | ✅ | ✅ | |
| 空/垃圾文件 | 干净报错 | 干净报错 | — | — | |

### 相对 GStreamer 的格式能力损失

- **Opus**：真实损失（现代音乐库常见）。可选路径：
  a) 自定义 rodio `Source` 包 `ogg`+`opus` crate（libopus，引入一个小型 C 依赖，
     Windows 需带 opus 构建——轻度打破「纯 Rust 无 runtime」）；
  b) 等/推 rodio 上 symphonia 0.6 + `symphonia-adapter-libopus`（该 adapter 需
     symphonia 0.6，rodio 0.22.2 用不了；rodio 是否有 0.6 底座版本**待验证**）；
     > 2026-10-13 更正：adapter 0.2.x 基于 symphonia-core 0.5，不必等 symphonia 0.6；
     > rodio master 已用它加了 `symphonia-libopus`，但尚未发布，见 `asmr-pivot-plan.md` 第 5 节。
  c) 首版砍掉 Opus，UI 里标记「暂不支持」。
- **WMA**：建议归入「可以移除」（老旧格式；Lofty 对 ffmpeg 混流 ASF 都失败）。
- **APE**：建议「可以移除」，但**必须显式拒绝**（防止上述假阳性静默播 0.7s）。
- **AIFF**：无损失（一个 feature 的事）。

## 5. 已知风险

1. **播放中错误无事件**（最大语义差异）：损坏/截断文件表现为「静默提前 EOS」。
   缓解：UI 上 position 未到 duration 就 EOS 时可在消息里提示；或接受（自动跳
   下一首本就是当前错误处理路径的行为）。不需要为此引入新机制。
2. **seek 到精确结尾 = Err + 源终止**：App 的 `seek_relative`/`seek_absolute` 必须
   clamp 到 `duration.saturating_sub(~50ms)`。
3. **切歌后 get_pos 惰性**：App 本地立即把显示位置归零，不等 rodio。
4. **单次 seek ~130ms**：连打方向键时请求会排队（50 次实测无卡死）；
   App 侧做 seek 合并（按住时累计、松手一次执行）即可，非阻塞项。
5. **APE 假阳性**：见 §4，必须显式拦截。
6. **频谱实时性**：PCM tap 已验证零阻塞；自有 FFT/分析在分析线程做，音频线程
   只写 latest-wins 槽。丢帧允许。
7. **headless 已解决**：`mixer()` 路径可在 CI 无设备运行全部播放逻辑测试。
8. **Windows**：`cargo check --target x86_64-pc-windows-msvc` 通过（Temp/win-check，
   rodio+lofty 纯 Rust 解析）。**发行包可彻底去掉 GStreamer runtime**：
   install-gstreamer.ps1、194MB 安装器载荷、LGPL 捆绑合规文件全部退役；
   构建机只需 Rust 工具链（CPAL/WASAPI 直接绑系统 COM）。
   Linux 构建依赖从「gstreamer 全家 devel」缩到 `alsa-lib-devel`。
9. **Opus 路径未定**：见 §4，需要用户决策。
10. 资源占用实测：FLAC/96kHz 播放 CPU <1%、峰值 RSS ≈ 8.9MB，20 次快速切歌
    最坏 9ms，无 underrun 迹象（Fedora 容器 → 宿主 PipeWire）。

## 6. 建议迁移架构（初步，未实现）

```text
src/player.rs（接口完全不变，内部换实现）
   │
   ├─ DeviceSinkBuilder（with_error_callback → PlayerEvent::Error）
   │     └─ MixerDeviceSink → Mixer
   ├─ Player::connect_new(mixer)
   │     └─ append(PcmTap<Decoder>)
   │           ├─ DecoderBuilder（byte_len + seekable + hint）
   │           └─ PcmTap → latest-wins 槽 → 分析线程（自有 spectrum）
   ├─ EOS：tick 轮询 empty() 转变 + App 自知 stop/switch
   └─ mute：记忆音量 + set_volume(0)

src/library.rs（Discoverer → Lofty）
   └─ Probe::open → guess_file_type → read
      tag 优先级：primary_tag → first_tag → 文件名兜底（与现行等价）
      duration：FileProperties.duration
```

- 测试：`Player::new_for_tests` 等价物 = `mixer()` headless 路径
  （现有 `tests/player.rs` 的 fakesink 用例可平移，且更快）。
- spectrum：`SpectrumFrame` 事件由自有分析器产生，payload 可从「dB 序列」改为
  直接给 bar 高度（谱线分析彻底离开播放线程）。

## 7. 建议迁移顺序

```text
A. Rodio 基础播放器（player.rs 内部替换，接口不变；GStreamer 暂保留 feature 开关或并存）
B. EOS / Error 事件适配（empty() 轮询 + 打开期错误 + 设备错误回调 + seek clamp）
C. PCM tap → 自有 spectrum 分析（含 96kHz 验收）
D. Lofty metadata（library.rs Discoverer 替换）
E. 格式兼容调整（AIFF feature、APE 显式拒绝、Opus 决策落地、扩展名表收敛）
F. 删除 GStreamer 依赖与 player.rs 旧实现
G. 清理 CI / Windows packaging / RPM spec（去 GStreamer runtime 与 LGPL 捆绑）
H. 全平台回归（Linux 容器 + Windows 实机目验）
```

A-D 每步独立可验收；F/G 不可逆，放在全部功能验收之后。

## 8. 待用户决策清单

1. **Opus**：自定义 opus Source（加 libopus C 依赖）/ 等 rodio+symphonia 0.6 / 首版砍掉？
2. **WMA、APE**：确认移除（APE 会显式拒绝防假阳性）。
3. **AIFF**：确认开 `rodio/symphonia-aiff`（保留格式）。
4. 播放中 duration 来源：建议 Rodio 运行时值（与 seek 饱和一致），列表用 Lofty
   扫描值——是否同意双来源分工（两者实测差 ≤31ms）。
5. 迁移期间 GStreamer 是 feature 开关并存，还是一次性切换（建议前者）。
```
