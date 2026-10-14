# 已知问题

审查日期：2026-10-14。审查基于提交 `c691eb7feb17c2ea0222aa5d03250fb0dba0fb15`。
以下两个问题均已在 `dev-fedora` 容器内的临时源码副本中通过测试复现，均已于 2026-10-14 修复，见各条的“状态”。
Nemuri 改名阶段的新增复查结果见文末，BUG-003 至 BUG-005 均已于 2026-10-14 修复。

## BUG-001：自动切歌后重排队列可能导致程序崩溃

- 优先级：P1。
- 状态：已修复，提交 `22be42a`。自动切歌取走队首时，队列选择跟着原来选中的条目前移；
  `move_queue_selected()` 交换前检查源、目标两个下标。回归测试为
  `reordering_the_queue_right_after_a_natural_end_stays_in_range`。
- 相关代码：[src/app/queue.rs](../src/app/queue.rs) 的 `move_queue_selected()`；
  [src/app/mod.rs](../src/app/mod.rs) 的 `on_tick()`。

### 复现步骤

1. 开始播放一首曲目，并向队列加入至少两项。
2. 按 `Q` 打开队列，选中最后一项。
3. 当前曲目自然结束，自动切歌取走队首后，在下一次 tick 修正选择之前按 `K` 上移。

预期：选中项保持在有效范围内，重排成功或被安全忽略。

实际：`VecDeque::swap()` 使用越界的源下标，触发 panic，播放器退出。
这一触发窗口位于自动切歌完成后、下一次 tick 修正选择之前。

### 原因与修复方向

`on_tick()` 先调用 `clamp_selections()`，再处理播放器事件。
自然结束事件通过 `advance()` 消费队首，使队列缩短，但没有立即更新 `queue_selected`。
`move_queue_selected()` 只检查目标下标，没有检查源下标；旧选择恰好越界一项时，
`K` 算出的目标仍然有效，于是执行越界交换。

建议在消费队列后立即修正选择，并在重排操作中检查源、目标两个下标。

### 验证与回归要求

临时测试 `review_reorder_queue_after_natural_end_does_not_panic` 按照
“修正选择 → 注入自然结束 → 绘制队列 → 按 K”的顺序执行，稳定触发
`assertion failed: i < self.len()`。

修复后应覆盖：选中队尾时自动消费队首，随后立即按 `K`；操作不应崩溃，选择应保持有效。

## BUG-002：快进越过曲尾后，“已听完”被旧进度覆盖

- 优先级：P2。
- 状态：已修复，提交 `f423333`。`mark_finished()` 之后，这一遍播放不再按位置记录进度，
  直到装上下一遍播放；下一首恰好是同一条录音（只有一条且列表循环）时，仍从头开始。
  复现时发现，越过曲尾后没有下一首、播放停止时同样会被覆盖，一并修复。回归测试为
  `seeking_past_the_end_of_a_recording_marks_it_finished`、
  `seeking_past_the_end_of_the_last_recording_marks_it_finished` 和
  `seeking_past_the_end_of_a_repeated_recording_starts_a_fresh_playthrough`。
- 相关代码：[src/app/media.rs](../src/app/media.rs) 的 `seek_rel_micros()`；
  [src/app/playback.rs](../src/app/playback.rs) 的 `play_index()`；
  [src/app/progress.rs](../src/app/progress.rs) 的 `note_progress()`。

### 复现步骤

1. 播放一段 20 分钟的长录音，并准备好可播放的下一首。
2. 跳转到 `19:50`。
3. 按 `L` 前进 60 秒，越过曲尾并切换到下一首。
4. 再次播放刚才的长录音。

预期：录音标记为“已听完”，再次播放从头开始。

实际：进度仍为 `19:50`，再次播放从该位置续播。

### 原因与修复方向

`seek_rel_micros()` 检测到请求位置越过曲尾后，先调用 `mark_finished()`，
再调用 `play_next(false)`。切歌进入 `play_index()` 时，又调用 `note_progress()`；
此时旧曲目仍处于播放状态、位置仍为 `19:50`，因此刚设置的 `Finished`
被覆盖成 `InProgress(1190s)`。

建议让越过曲尾的切歌流程跳过旧曲目的普通进度记录，确保完成状态不会被覆盖。

### 验证与回归要求

临时测试 `review_seek_beyond_end_keeps_finished` 在执行上述快进后，
预期得到 `Some(Finished)`，实际得到 `Some(InProgress(1190s))`，断言失败。

修复后应覆盖：快进越过长录音曲尾后仍保留完成状态，重新打开该录音从零开始；
正常中途切歌仍应保留续播位置。

## 测试与 CI 检查记录

以下是本次审查时的结果，不代表问题已经修复：

| 检查 | 结果 |
| --- | --- |
| 本地 `dev-fedora`：`cargo test --all-targets --locked` | 246 个单元测试、16 个播放集成测试全部通过 |
| 容器临时副本新增的两个复现测试 | 均失败，确认上述问题；测试尚未加入本仓库 |
| GitHub CI：Linux | 格式检查、Clippy、246 个单元测试及 16 个集成测试通过 |
| GitHub CI：Windows MSVC | Clippy、240 个单元测试及 16 个集成测试通过 |

[本次提交的 GitHub CI](https://github.com/HZ-TYZQ/tui-music-player/actions/runs/38503582234)
于北京时间 2026-10-14 10:57 完成，提交与本地审查版本一致。
现有测试没有覆盖上述两个触发场景，因此 CI 通过不能排除这两个问题。

修复后，本地 `dev-fedora` 中 252 个单元测试、17 个播放集成测试全部通过，
其中包括上述四个回归测试。

[最近一次 Packaging 运行](https://github.com/HZ-TYZQ/tui-music-player/actions/runs/37431924817)
也成功，但对应 2026-10-06 的提交 `2de7fffcb4f36891e9ccc87de56beba6da84ce28`，
不属于本次审查提交。按 [packaging.yml](../.github/workflows/packaging.yml) 的路径过滤规则，
本次推送没有触发打包工作流。

## Nemuri 改名阶段复查（2026-10-14）

复查基于 `2b652dcf83c069c1314edaf439340183deab1448`，范围为上次复查的
`40b16a8` 之后六个提交。开始复查时工作区与暂存区均无改动。
下面两个迁移问题在 `dev-fedora` 容器中的临时源码副本内通过独立测试确认；
旧标签重建问题通过在容器内对 `v1.4.0` 的源码副本执行当前工作流的版本检查确认。
复现测试未加入项目源码。

### BUG-003：迁移会放宽旧数据目录的访问权限

- 优先级：P2。
- 状态：已修复，提交 `cc24335`。各级目录先以 `0700` 建立，内容复制完再改成源目录的权限，
  复制途中和复制之后都不会比原来更开放；文件权限仍由 `fs::copy` 保留。回归测试为
  `copied_folders_keep_their_permissions` 和 `folders_are_private_while_they_are_being_copied`。
- 相关代码：[src/config/previous.rs](../src/config/previous.rs) 的 `copy_tree()`，第 62 行。

复现：在 Linux 上，把旧的 `data/tui-music-player` 目录权限设为 `0700`，
其中 `session.toml` 为 `0644`；以常见的 `umask 022` 调用 `adopt_previous_dirs()`。
迁移成功后，新的 `data/nemuri` 目录变成 `0755`，文件仍为 `0644`。
如果 XDG 根目录及其父目录允许其他用户遍历，原先依靠 `0700` 保护的会话、
收听进度和播放列表现在就可被其他用户读取。

原因：`copy_tree()` 用默认权限创建目标目录，只复制文件内容和文件权限，
未保留目录权限。临时目录 `.migrating` 在复制期间也具有放宽后的权限。

建议：创建临时目录时先使用足够严格的权限，并保留源目录及各级子目录的权限；
避免只在所有内容复制结束后才收紧顶层目录。

验证：临时测试 `review_migration_keeps_private_directory_permissions` 失败，
预期权限为 `0700`（十进制 448），实际为 `0755`（十进制 493）。
修复后应覆盖顶层私有目录、私有子目录及复制中的临时目录。

### BUG-004：迁移成功提示会掩盖进度或播放列表加载失败

- 优先级：P2。
- 状态：已修复，提交 `2f22c46`。启动提示不再用 `Option::or()` 只取一条，而是由 `join_notes()`
  按顺序接成一行：读不了的播放列表和收听进度在前，配置警告和 `--set-library` 说明其次，
  复制旧目录的说明在最后。回归测试 `copied_data_that_cannot_be_loaded_is_reported_before_the_copy_note`
  覆盖损坏的进度、损坏的播放列表以及二者同时损坏。
- 相关代码：[src/app/mod.rs](../src/app/mod.rs) 的 `with_player()`，第 137 行；
  [src/main.rs](../src/main.rs) 的迁移提示传递。

复现：旧数据目录中留有格式损坏的 `progress.json`，新目录尚不存在，第一次启动 Nemuri。
目录复制成功，随后 `ProgressStore::load()` 将损坏文件改名保留并从空记录开始，
同时返回加载失败说明；但底栏只显示“已把 tui-music-player 目录里的设置和播放记录复制到新目录”。
第一次扫描后仍然只有这条成功提示，用户无法得知续听记录实际未加载。
损坏的播放列表同样会被略过，而其加载警告被迁移提示遮住。

原因：迁移说明作为 `initial_warning` 传入，而
`initial_warning.or(playlist_warning).or(progress_warning)` 只保留第一条。
改名前已有正常配置的启动流程通常没有迁移说明；新增迁移流程使升级时必然有这条说明，
从而丢弃后续的用户数据加载错误。

建议：优先显示数据加载错误，并保留迁移说明；多条错误需要能被用户看到，
不能仅用 `Option::or()` 选一条。

验证：临时测试 `review_migration_reports_a_copied_progress_file_that_cannot_be_loaded`
在完整的“复制旧数据 → 初始化 App → 完成首次扫描”流程后检查失败：
底栏没有“收听进度”错误，只保留迁移成功说明。
修复后应同时覆盖迁移与损坏进度、迁移与损坏播放列表，以及二者同时损坏的情况。

### BUG-005：当前 Release 工作流无法手动重建改名前的标签

- 优先级：P2。
- 状态：已修复，提交 `3a60985`。没有采用让一个工作流兼容两种文件布局的做法：每个标签都带着
  构建它的那份工作流，所以当前工作流在检出的标签缺少 `packaging/nemuri.spec` 时直接停下，
  并给出用标签自带的工作流重建的命令
  `gh workflow run release.yml --ref <标签> -f tag=<标签>`。已在容器中用当前工作流的检查步骤
  分别对 `v1.4.0` 和当前源码执行（前者给出上述提示后停止，后者通过版本检查），
  并确认 `v1.4.0` 自带工作流的版本检查对其源码通过。
- 相关代码：[.github/workflows/release.yml](../.github/workflows/release.yml)，第 47 行，
  以及后续硬编码的 `nemuri.exe` 和 `-ExePath`。

复现：从当前版本的 Release 工作流手动运行，输入其提供的 `tag=v1.4.0`。
工作流检出该旧标签后，版本检查固定读取 `packaging/nemuri.spec`，
但该标签只有 `packaging/music-player.spec`，因此版本检查失败，所有构建与发布任务都不会运行。
这影响当前工作流提供的“Existing release tag to rebuild”入口；新标签使用新文件名不受此问题影响。

容器中的复现输出：

```text
sed: can't read packaging/nemuri.spec: No such file or directory
Version mismatch: tag=1.4.0 cargo=1.4.0 spec=
```

建议：依据检出标签的包名或文件布局选择 spec、产物名称及 Windows 打包脚本参数，
或让历史标签使用对应版本的构建流程。只修复 spec 路径仍不够：旧标签产物是
`music-player.exe`，旧打包脚本接收的参数是 `-MusicPlayerExe`。
回归检查应覆盖当前工作流重建一个改名前的标签，以及一个改名后的标签。
本次未触发 GitHub Release 或发布任何产物。

### 本轮验证与 GitHub CI

| 检查 | 结果 |
| --- | --- |
| `dev-fedora`：`cargo fmt --all -- --check` | 通过 |
| `dev-fedora`：`cargo clippy --all-targets --locked -- -D warnings` | 通过 |
| `dev-fedora`：`cargo test --all-targets --locked` | 262 个单元测试、17 个集成测试通过，共 279 个 |
| `dev-fedora`：`desktop-file-validate packaging/nemuri.desktop` | 通过 |
| 临时源码副本的两个迁移复现测试 | 均失败，确认 BUG-003、BUG-004 |
| 旧标签源码副本执行当前 Release 版本检查 | 失败，确认 BUG-005 |

检查时 GitHub `master` 仍为 `40b16a863e0708164ce801844530f4febc10eea5`。
[最新 CI](https://github.com/HZ-TYZQ/tui-music-player/actions/runs/38506400069)
成功，但对应上轮版本；本轮本地提交 `2b652dc` 尚无 GitHub CI 或 Packaging 运行结果，
不能把旧版本的成功结果当作改名阶段已经通过远端检查。
本次没有在 Windows 上实际运行升级安装测试。
