---
name: power-test-compare
description: |
  Compare two `power_test` runs side-by-side (HTML diff page or text
  report). Use when the user has two run_ids and wants a delta across
  latency / TTFT / ITL / TPS / cache / throughput, OR when they want a
  regression report after a code / config / endpoint change.
  Cross-target compare is supported (same model, different endpoint)
  and produces a "different target" warning by design — that is the
  upstream-vs-our pattern. For the 3-report SLA workflow (vendor +
  ours + diff) use `power-test-onboard` instead. For a single-endpoint
  stress test use `power-test-run`.
---

# power-test-compare（两 run 对比 · 引导式）

> 一次只问用户一件事。等用户回答再进下一步。
> 用户如果直接给两个 run_id，跳到 Step 3。

---

## Step 1 · Run A

**问用户**：

> 第一次压测的 run_id 是什么？形如 `20260808-11-28-15-1db22a`。
>
> 不知道？选一个：
>   1. **我告诉你**（贴 run_id 或后 6 位）
>   2. **帮我找最近的**（我跑 `power_test list` 给你看最近的 10 次）

**如果用户选 2**：

```bash
power_test list
```

把输出按"模型 / tag / 时间"展示，让用户挑。

---

## Step 2 · Run B

**问用户**：

> 第二次压测的 run_id 是什么？

同样的"帮我找"选项。

---

## Step 3 · 输出格式

**问用户**：

> 要哪种？
>   1. **HTML**（默认，浏览器打开看图）
>   2. **纯文本**（适合贴 PR / 飞书 / 钉钉）

**默认**：1。

---

## Step 4 · Review & Run

**回显命令**：

```bash
power_test compare <RUN_A> <RUN_B> --html
# 或不加 --html 走纯文本
```

**问用户**：

> 这样对吗？
>   1. **跑**
>   2. **改 / 取消**

跑命令。

---

## 跑完后的交付

给用户：

### 1. 报告路径

```
~/.power_test/history/compare-<RUN_A>-vs-<RUN_B>-<ts>.html
```

（如果纯文本就贴终端输出）

### 2. Top 3 变化

按 `|%delta|` 排序，挑出最大的 3 个指标，**说方向**：

```
最大的变化：
  + latency p99:    1500ms → 2200ms  (+47%, 退步)  🔴
  + ttft p50:        200ms →  180ms  (-10%, 改进)  🟢
  +  achieved_rps:  2.05   →  1.97   (-4%,  退步)  🟡
```

### 3. 上下游对比特别提示

如果对比 header 写了 `different target`：

> 这是**跨端点**对比（model 一样，URL 不一样）。常见用途是：
> - **上游 vs 我方**（厂商 → 我们的网关）→ 看 latency overhead
> - **同一模型不同时刻**（上线前 vs 上线后）→ 看回归
>
> 上游 vs 我方的 SLA 预算（粗略）：
> - latency p50（我方）≤ 1.5 × 上游 p50
> - latency p99（我方）≤ 2.0 × 上游 p99
> - ttft p50（我方）≤ 1.3 × 上游 p50
> - tps（我方）在上游 ±5% 内
>
> 任何一项超出就值得开个工单排查我方栈。

如果 header 写了 `different model alias`：

> 这是**同模型别名**对比（M6g 模式，比如 `DeepSeek-V4-Flash-20260115` vs `DeepSeek-V4-Flash-20260301` 归到同一个 `DeepSeek-V4-Flash` 别名下）。看的是**版本间回归**，不是跨端点。

### 4. 下一步

> 要不要：
> - 把这次对比加到 PR 描述？ → 帮你拼一段 markdown
> - 接新模型 SLA 验证？ → 交给 `power-test-onboard`
> - 收工

---

## 故障处理

| 现象 | 处理 |
|---|---|
| `run A not found` / `run B not found` | 让用户跑 `power_test list` 看实际 run_id |
| 报告说 `index.json is corrupt` | 索引坏了但能跑（目录扫描 fallback）。修索引：再 `power_test run` 任意一次 |
| 报告里没数字 / 全 0 | 两次压测里至少一次是 0 成功 → 让用户去 `summary.txt` 看 |
| 纯文本输出乱码 | 跑 `chcp 65001` 后重试，或加 `--no-color` |

---

## 进阶

- **多维度 diff**：跑 3+ 次压测，**两两** compare，挑出最异常的。
- **回归脚本**：把 `power_test run` + `power_test compare` 串成 CI step，PR 跑两次（base / head）→ 自动 diff → 阈值告警。
- **完整 SLA 流程**：上游 + 我方 → 3 报告 = `power-test-onboard`。

---

## 参考

- `references/compare-interpretation.md` — 每个 diff 行的含义、颜色规则、上下游预算表
- `power-test-run` — 怎么跑出 run_id
- `power-test-onboard` — 3 报告 SLA 完整流程
