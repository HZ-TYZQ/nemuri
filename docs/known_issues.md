# 已知问题

审查日期：2026-10-14。审查基于提交 `c691eb7feb17c2ea0222aa5d03250fb0dba0fb15`。
以下两个问题均已在 `dev-fedora` 容器内的临时源码副本中通过测试复现，均已于 2026-10-14 修复，见各条的“状态”。

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
