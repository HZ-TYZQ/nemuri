# 已知问题

最近审查日期：2026-10-16，基于提交 `b01ad0fa15387041d6693ea4d7cbefa46d7f2de0`。
本轮新增 BUG-007 至 BUG-010，均为 P2；BUG-007 已于 2026-10-16 修复，其余三个待修复。
四个问题均已在 `dev-fedora` 容器内的临时源码副本中通过独立测试复现，详见文末。

审查日期：2026-10-14。审查基于提交 `c691eb7feb17c2ea0222aa5d03250fb0dba0fb15`。
以下两个问题均已在 `dev-fedora` 容器内的临时源码副本中通过测试复现，均已于 2026-10-14 修复，见各条的“状态”。
Nemuri 改名阶段的新增复查结果见文末。BUG-003、BUG-005 已修复；
再次复查发现的 BUG-004 显示问题和新增的 BUG-006 也已于 2026-10-14 修复。

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
- 状态：已修复，提交 `2f22c46`（保留全部提示）和 `cf4ae63`（底栏折行显示）。启动提示不再用 `Option::or()` 只取一条，而是由 `join_notes()`
  按顺序接成一行：读不了的播放列表和收听进度在前，配置警告和 `--set-library` 说明其次，
  复制旧目录的说明在最后。回归测试 `copied_data_that_cannot_be_loaded_is_reported_before_the_copy_note`
  覆盖损坏的进度、损坏的播放列表以及二者同时损坏。
  对 `8d276c9` 的再次复查确认：字符串中已保留多条提示，但底栏仍只显示一行，
  后面的错误被裁掉；详见文末“修复后再次复查”。
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

## 修复后再次复查（2026-10-14）

复查基于 `8d276c932f1a46439764a88bcabd267bde782db1`，范围为 `2b652dc` 之后四个提交。
BUG-003 的权限放宽问题已修复；BUG-005 通过拒绝当前工作流中的旧布局、
提示使用标签自带工作流解决。当前布局的检查及版本检查通过；
`v1.4.0` 被当前布局检查明确拒绝，其自带工作流的版本检查通过。
`gh workflow run --help` 确认 `--ref` 可以选择包含对应版本工作流的分支或标签。
本次未触发远端构建或发布。

### BUG-004 补充：拼接的多条启动提示仍被单行底栏裁剪

- 优先级：P2。
- 状态：已修复，提交 `cf4ae63`。几条启动提示改为用换行分隔，底栏按终端宽度折行显示每条提示，
  续行缩进到圆点之后；最多占屏幕高度的三分之一，再放不下时最后一行以省略号结尾，多出的行先从频谱让出。
  下一条提示出现时底栏恢复成一行。回归测试 `several_startup_notes_all_show_on_screen` 用实际绘制
  检查 100×30 和 80×24 下三条提示都完整可见以及恢复成一行，
  `a_footer_that_would_take_more_than_a_third_of_the_screen_ends_in_an_ellipsis` 检查高度上限。
  另在容器中用 80×24 的真实终端（tmux）确认损坏的播放列表、收听进度和迁移说明都完整显示。
- 相关代码：[src/app/mod.rs](../src/app/mod.rs) 的 `join_notes()`，第 413 行；
  [src/ui/mod.rs](../src/ui/mod.rs) 的 `draw_footer()`，第 205 行。

复现：同时存在损坏的播放列表与 `progress.json`，第一次启动完成迁移及扫描，
以 100 列、30 行绘制界面。内存中的提示依次包含播放列表错误、进度错误、迁移说明，
但屏幕底栏只能显示第一条播放列表错误的前半段，连“收听进度”都看不到。
底栏没有滚动或查看完整消息的入口；后续普通操作还会把整条消息换掉。
因此用户仍无法从正常宽度的界面得知进度文件已被隔离、续听记录为空。

原因：`join_notes()` 解决了消息字符串丢失，但所有消息被拼接为一条不断增长的单行文本。
底栏布局固定为一行，`Paragraph` 在可用宽度外直接裁剪；现有回归测试只检查字符串，
没有检查实际绘制结果。

建议：提供可查看完整内容的提示面板、可逐条查看的消息，或其他受终端宽度约束的显示方式；
仅增加 `join_notes()` 中的文本不足以确保多条错误都能被看到。

验证：容器临时源码副本中的测试 `review_joined_startup_warnings_are_visible_to_the_user`
调用现有迁移测试夹具生成两条加载错误，再调用实际 `render()`；
屏幕底栏包含“播放列表”，不包含“收听进度”，断言失败。
修复后的回归应覆盖真实的 80 列或 100 列绘制，以及用户查看后续错误的路径。

### BUG-006：保留只读目录权限后，中断的迁移可能无法再次启动

- 优先级：P2。
- 状态：已修复，提交 `0b030da`。临时目录是迁移自己建的，`remove_staging()` 在 Unix 上先给各级目录
  加上所有者的读写执行权限再递归删除，下次启动清理残留和复制失败后的清理都走这里，旧目录不受影响；
  Windows 上标准库删除时不理会只读属性。复制失败后的清理即使失败，也仍报告复制本身的错误，
  下次启动会再清理。回归测试为 `an_interrupted_copy_of_a_read_only_folder_starts_over` 和
  `a_staging_tree_with_read_only_folders_is_removed`。
- 相关代码：[src/config/previous.rs](../src/config/previous.rs) 的 `copy_tree()`，第 76 行，
  以及 `copy_into_place()` 中第 48 行与第 55 行的临时目录清理。

复现：旧配置目录可读但不可写，例如权限为 `0500`，其中已有可读的 `config.kdl`。
复制完成后，`copy_tree()` 把 `nemuri.migrating` 的权限恢复为 `0500`；
如果此时在最终重命名前进程中断，下一次启动会先调用 `remove_dir_all()` 清理它。
由于进程对该目录没有写权限，无法删除其内部文件，清理返回 `Permission denied`，
迁移及应用启动随之失败。再次启动仍会遇到同一个临时目录，须手动调整权限才能继续。
只读子目录在中途失败或中断后残留时也有同样的问题。

原因：新增权限保留使临时树可能包含不可写目录，但清理仍假定临时树可以直接递归删除；
错误分支中的清理失败还被忽略。

建议：让临时目录清理支持迁移程序自己创建的只读目录，在安全范围内恢复清理所需的所有者权限，
并处理清理失败；旧目录应保持不变。需要同时覆盖下次启动时的残留清理和当前失败路径的清理。

验证：容器临时源码副本的测试 `review_interrupted_read_only_migration_can_restart`
用实际 `copy_tree()` 生成完整、权限为 `0500` 的临时树，模拟复制结束但重命名前中断，
随后调用 `adopt_previous_dirs()`。返回 `Permission denied (os error 13)`，预期重新迁移成功的断言失败。

### 再次复查的检查结果

| 检查 | 结果 |
| --- | --- |
| `dev-fedora`：格式检查、Clippy（警告视为错误） | 通过 |
| `dev-fedora`：`cargo test --all-targets --locked` | 266 个单元测试、17 个集成测试通过，共 283 个 |
| `dev-fedora`：`desktop-file-validate packaging/nemuri.desktop` | 通过 |
| 临时副本：多条错误的实际绘制测试 | 失败，确认 BUG-004 显示部分仍未修复 |
| 临时副本：只读临时树的中断恢复测试 | 失败，确认 BUG-006 |
| 当前 Release 检查步骤、旧标签自带版本检查 | 均符合预期，BUG-005 的替代重建路径通过静态及本地脚本验证 |

上述两个复现测试只在容器中的临时源码副本执行，未加入项目源码。
GitHub 最新 CI 仍是 [40b16a8 的成功运行](https://github.com/HZ-TYZQ/tui-music-player/actions/runs/38506400069)，
没有当前 `8d276c9` 的运行结果。本次没有执行 Windows 安装升级的实机验证。

## 跳转点与数据一致性审查（2026-10-16）

审查基于 `b01ad0fa15387041d6693ea4d7cbefa46d7f2de0`，开始审查时工作区无改动。
本轮检查了跳转点、收听进度、扫描缓存、播放列表及主要播放状态流转。
以下四个问题均已通过容器内临时源码副本中的独立测试确认。BUG-007 已修复，见该条的“状态”；
其余三个只记录问题，尚未修复。

### BUG-007：非 UTF-8 的进度或跳转点文件会在保存时被覆盖，且没有备份

- 优先级：P2。
- 状态：已修复，提交 `ac364ac`。`load_json()` 改为按字节读取，先检查整个文件是不是 UTF-8：
  不是 UTF-8、不是 JSON 或版本不对，都按无法识别处理，先改名保留再从空记录开始。
  只检查 JSON 解析不够：`serde_json` 直接读字节时，跳过的字段里有坏字节也不报错。
  文件读不了（不存在除外），或者无法识别又没能改名保留时，原文件留在原处，存储标为只读：
  本次运行 `save()` 不写盘，`MarkStore::add()` 拒绝添加并在底栏说明，提示里写明本次运行不保存。
  回归测试覆盖收听进度和跳转点：UTF-16 与 GBK 内容先备份，备份字节与原文件一致；
  文件读不了，或者没能改名保留、之后目录又能写了，保存都不覆盖原文件。
  去掉只读保护后，这几项测试会失败。
- 相关代码：[src/file_records.rs](../src/file_records.rs) 的 `load_json()`，第 135 行，
  以及 `save_json()`。

复现步骤：

1. 将一份包含已有跳转点的合法 JSON 以 UTF-16（带 BOM）保存为 `marks.json`。
2. 调用 `MarkStore::load()`：返回空记录和读取失败提示，原文件没有被改名保留。
3. 添加一个新跳转点并保存。

预期：无法解析的原始数据应先备份；无法备份时，应阻止空记录覆盖原文件。

实际：保存成功，新内容覆盖原始 UTF-16 文件，目录中没有 `marks.json.corrupt-*` 备份，
旧跳转点无法再从原文件找回。同一读取和保存逻辑也用于 `progress.json`。

原因：`fs::read_to_string()` 遇到非 UTF-8 字节时直接返回错误，`load_json()` 的错误分支
只返回空记录与提示；只有读取为 UTF-8 成功、随后 JSON 解析或版本检查失败时，
才调用 `preserve_unreadable()`。后续保存没有区分“原文件不存在”和“原文件未能读取”。

建议：将解码失败也纳入原文件保留流程，并在读取或备份失败后保护原文件，
避免后续将空记录或部分新记录直接写回。

验证：临时测试 `review_invalid_utf8_marks_are_backed_up_before_a_save` 写入含旧跳转点的
UTF-16 JSON，加载、添加新点、保存后检查原始字节是否存在于备份中，断言失败。
修复后应覆盖跳转点与收听进度、非 UTF-8 内容，以及无法完成备份时禁止覆盖的行为。

### BUG-008：多个失联记录匹配同一新文件时，改名跟随会任意选择来源

- 优先级：P2。
- 状态：待修复。
- 相关代码：[src/file_records.rs](../src/file_records.rs) 的 `follow_renames()`，第 104 行。

复现步骤：

1. 为两条具有相同 `file_size`、`modified_ns` 的录音分别设置不同的跳转点。
   硬链接或保留修改时间的文件副本可以具有相同的这两项属性。
2. 将其中一条改名，另一条移出音乐库；重扫后，两条旧路径都不存在于扫描结果中，
   只有一个新路径匹配该文件大小和修改时间。
3. 执行改名跟随。

预期：来源无法唯一确定时保留两条旧记录，不自动迁移任何一条。

实际：程序迁移首先遍历到的旧记录；选择取决于 `HashMap` 遍历顺序，可能把另一条录音的
跳转点或收听进度赋给新路径，而不是跟随真正改名的文件。

原因：当前只要求目标候选唯一，没有检查具有同一组文件属性的失联来源是否唯一。
第一条来源占用目标后，其余来源被跳过；这只能防止覆盖，不能消除来源歧义。

建议：按文件大小和修改时间同时统计来源、目标，只有两侧都唯一时才自动迁移。
存在歧义时保持原记录，避免猜测。

验证：临时测试 `review_ambiguous_old_records_are_not_assigned_arbitrarily` 建立两条同属性、
不同跳转点的旧记录，再传入一个匹配的新文件；预期不发生迁移，实际返回发生迁移，断言失败。
修复后应覆盖多个来源对一个目标、一个来源对多个目标，以及正常的一对一改名；
跳转点和收听进度共用的行为都应验证。

### BUG-009：非 UTF-8 文件名命中扫描缓存后变成不存在的路径

- 优先级：P2。
- 状态：待修复。
- 相关代码：[src/library.rs](../src/library.rs) 的 `scan_library()`，第 259 行与第 262 行；
  `upsert_track()` 和 `load_cached()` 中的路径存取。

复现步骤：

1. 在 Linux 上创建一个有效 WAV，文件名含非 UTF-8 字节，例如 `song\xFF.wav`。
2. 首次扫描音乐库，确认返回的曲目路径与实际路径一致。
3. 文件不变，再次扫描或重启后扫描，使其命中 SQLite 缓存。

预期：缓存不改变实际文件路径，该曲目仍然可以打开播放。

实际：缓存中的文件名变成 `song�.wav`。第二次扫描直接复用这条路径，
该路径对应的文件不存在，原本首次扫描可打开的曲目随后无法播放。

原因：SQLite 路径字段通过 `to_string_lossy()` 写入，再从文本构造 `PathBuf`，
非 UTF-8 字节已经丢失。重扫使用同样的有损文本作为查询键，因大小和修改时间未变而命中缓存，
随后 `cached.clone()` 将有损路径带入曲库。

建议：无损保存和恢复文件路径，或对无法无损缓存的路径跳过缓存；
不能把仅用于显示的有损文本作为文件身份和实际打开路径。

验证：临时测试 `review_non_utf8_track_keeps_its_path_after_a_cached_scan` 通过真实
`LibraryWorker` 连续扫描同一个有效 WAV 两次，首次路径正确，第二次路径相等断言失败：
实际含 `�`，预期保留字节 `0xFF`。修复后应覆盖重扫、缓存重载及有损文本相同的不同文件名。

### BUG-010：播放列表写盘失败后保留内存修改，导致重试无法保存

- 优先级：P2。
- 状态：待修复。
- 相关代码：[src/playlist.rs](../src/playlist.rs) 的 `add_track()`，第 109 行，
  以及 `remove_track()`，第 126 行。

复现步骤：

1. 创建一个播放列表，然后使其保存目录暂时不可写或不可用。
2. 向列表加入一首曲目，保存失败，调用返回错误。
3. 恢复目录可写，再次加入同一首曲目。

预期：失败的操作可以正常重试，成功后内存与文件一致。

实际：首次调用虽然返回保存失败，却已把曲目加入内存。重试被判定为“这首歌已经在该播放列表中”，
磁盘文件仍未包含它；退出没有重试保存这项修改，重启后曲目消失。
移除曲目也先修改内存再写盘，失败后同样造成内存与磁盘不一致。

原因：`add_track()` 和 `remove_track()` 在原列表上直接修改，然后才调用 `save_playlist()`；
写盘失败没有回滚，也没有记录待重试的修改。

建议：先在副本上修改，写盘成功后才替换内存中的列表；或提供可靠的回滚与重试机制。

验证：临时测试 `review_failed_playlist_add_does_not_prevent_retry` 在创建列表后，
暂时把保存目录移开并用普通文件占据原路径，确认首次添加返回错误；恢复目录后重试仍返回错误，
预期重试成功的断言失败。修复后应同时覆盖添加和移除的失败、重试及重载后的数据一致性。

### 本轮验证结果

| 检查 | 结果 |
| --- | --- |
| `dev-fedora`：`cargo test --all-targets --locked` | 296 个单元测试、17 个播放集成测试全部通过，共 313 个 |
| 临时副本：`cargo test --locked --offline --test review -- --nocapture` | 四个复现测试均触发预期断言失败，确认 BUG-007 至 BUG-010 |

四个复现测试位于容器内临时源码副本的 `tests/review.rs`，未加入项目测试目录。
现有测试通过不代表这些问题已修复；本轮没有执行 Windows 实机或真实音频输出验证。
