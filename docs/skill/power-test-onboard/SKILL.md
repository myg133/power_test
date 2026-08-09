---
name: power-test-onboard
description: |
  Onboard a new upstream model in a token-reseller setup: stress-test
  the vendor endpoint AND our own fronting endpoint with the same
  shape, then produce a 3-report SLA bundle (upstream TPM/RPM, our
  TPM/RPM, latency-overhead diff). Use when the team is its own token
  vendor and needs to validate TPM/RPM against the upstream contract
  AND the downstream SLA. Hands the three reports to
  vendor-management, customer-success, and engineering respectively.
  For a single-endpoint stress test use `power-test-run`. For a
  two-run diff without the SLA scaffolding use `power-test-compare`.
---

# power-test-onboard（上游/我方 3 报告 SLA · 引导式）

> 一次只问用户一件事。等用户回答再进下一步。
> 用户如果一次给齐了所有信息（"接新模型 X，上游 URL=A 我们 URL=B，1 RPS 60 秒"），跳到 Step 2。

**这套流程什么时候用**：你的团队是个 token reseller——你从上游厂商买 token，包装后卖给你的客户。新上一个模型，要同时验证两件事：

- **厂商侧**：厂商承诺的 TPM/RPM，实际能给到吗？
- **我方侧**：我方网关 + 我方的限流 + 我方的转发，实测 TPM/RPM 是多少？要写进给客户的 SLA 合同。
- **diff**：我方栈在厂商基础上加了多少 latency？客户付的"中间商溢价"是多少。

---

## Step 1 · 测试规格（4 件事一次问清）

**问用户**：

> 测试规格定一下，4 件事：
>
> 1. **模型名？**（如 `qwen36-27B`）— **两边必须用一模一样的字符串**（这是 history 的分组 key）
> 2. **压测规模？** RPS / duration / concurrency（默认 **1 RPS × 60s × 并发 2**）
> 3. **max-tokens？**（默认 **32**，压短响应让 cache 行为可见）
> 4. **prompt？**
>    - **builtin pool**（默认，混合长度）
>    - **多轮 TOML**（要测 cache 命中，路径问一下）

**确认**：用户给的 4 件事一致（尤其是 model 字符串一致），再进 Step 2。

---

## Step 2 · 上游厂商端点

**问用户**：

> 上游厂商的：
>   1. **URL？**（如 `https://api.deepseek.com/v1/chat/completions`）
>   2. **API key？** 或"我已经设了 `OPENAI_API_KEY`"
>   3. **协议？** openai / anthropic（默认 openai）

**校验**：URL 合法 + key 非空（除非用户说"环境变量"）。

---

## Step 3 · 跑上游

构造命令：

```bash
power_test run \
  --target <UPSTREAM_URL> \
  --api <KIND> \
  --api-key <UPSTREAM_KEY> \
  --model <MODEL> \
  --rps <R> --duration <S> --concurrency <K> \
  --max-tokens <T> --stream true \
  --dataset <KIND> \
  --tag '<MODEL>-rps<R>-dur<S>-upstream' \
  --log-level info
```

跑。**捕获 run_id = `RUN_UPSTREAM`**。

跑完告诉用户：

```
上游跑完了。
  run_id   : 20260808-11-28-15-1db22a
  report   : ~/.power_test/history/<MODEL>/20260808-11-28-15-1db22a/report.html
  summary  : ~/.power_test/history/<MODEL>/20260808-11-28-15-1db22a/summary.txt
  关键指标 : p50=...ms p99=...ms ttft_p50=...ms rps=...
```

让用户**确认**这组数字合理（没全 401、没全 timeout、p50 数量级对）再进 Step 4。

---

## Step 4 · 我方端点

**问用户**：

> 我方自己的：
>   1. **URL？**（如 `https://api.mycompany.com/v1/chat/completions`）
>   2. **API key？** 或"用环境变量"
>   3. **协议？** 默认跟上游一致

**警告**：提醒用户 key 别搞混——我方 key 调的是我方端点，不会打到上游。

---

## Step 5 · 跑我方

构造命令（**所有规模参数和 Step 1 一字不差**）：

```bash
power_test run \
  --target <OUR_URL> \
  --api <KIND> \
  --api-key <OUR_KEY> \
  --model <MODEL> \
  --rps <R> --duration <S> --concurrency <K> \
  --max-tokens <T> --stream true \
  --dataset <KIND> \
  --tag '<MODEL>-rps<R>-dur<S>-our' \
  --log-level info
```

跑。**捕获 run_id = `RUN_OUR`**。

跟 Step 3 一样给一份"我方关键指标"。

---

## Step 6 · 对比

```bash
power_test compare <RUN_UPSTREAM> <RUN_OUR> --html
```

报告落地：

```
~/.power_test/history/compare-<RUN_UPSTREAM>-vs-<RUN_OUR>-<ts>.html
```

**预期**：header 会写 `different target`（model 一样 URL 不一样）。这是设计内的，别以为出错了。

---

## Step 7 · 提取 SLA 数字

跑 `Get-SlaNumbers` 脚本（已经在 `references/onboarding.md`，agent 跑就行），输出表：

```
  side       RPM   TPM(out)  TPM(total)  p50  p99  TTFT_p50  achieved_rps
  upstream    60    1800      2400        500  1500  200       1.02
  our         58    1740      2310        700  1800  280       0.98
  overhead   -3%   -3%       -4%         +40% +20%  +40%      -4%
```

按 7 项 pass/fail 给出判断（标准在 `references/onboarding.md`）：

| 检查项 | Pass | Investigate | Fail |
|---|---|---|---|
| 达成 RPS / 目标 RPS | ≥ 98% | 95-98% | < 95% |
| 我方 p50 / 上游 p50 | ≤ 1.5× | 1.5-2.0× | > 2.0× |
| 我方 p99 / 上游 p99 | ≤ 2.0× | 2.0-3.0× | > 3.0× |
| 我方 TTFT_p50 / 上游 | ≤ 1.3× | 1.3-1.8× | > 1.8× |
| TPM / SLA 承诺 | ≥ 承诺 | 90-100% | < 90% |
| 错误率 | 0 | < 2% | ≥ 2% |
| skipped ticks | < 5% | 5-15% | > 15% |

**给一个总体判定**：3 报告 SLA 整体 **PASS** / **Investigate** / **FAIL**。

---

## Step 8 · 交付（按角色分）

### 8.1 给"供应商管理"

交付：
- 上游 `summary.txt`
- 上游 `report.html`
- TPM/RPM 实测 vs 厂商宣传值

如果实测 < 厂商承诺：**起草 vendor ticket**（agent 帮你写草稿）：

> 主题：TPM 提升请求 — <MODEL>
>
> 测得 TPM（output）=<X>，低于贵方宣传的 <Y>。
> 测试条件：1 RPS × 60s, max_tokens=32, 模型 <MODEL>。
> 完整 report.html 见附件。
> 请确认实际配额并告知可释放上限。

### 8.2 给"客户成功"

交付：
- 我方 `summary.txt`
- 我方 `report.html`
- 我方 TPM/RPM 实测值

**对照客户 SLA 合同**：
- 实测 ≥ 合同 → 放心用
- 实测 90-100% → 跟客户对齐，必要时改合同或加限流
- 实测 < 90% → 别签这个数，**先优化再签**

### 8.3 给"工程"

交付：
- `compare-*.html`（最重要的就是这个）
- latency overhead 数字（Step 7 的表）

排查方向（按 overhead 大小）：
- overhead < 20%：栈基本透明，看其他项
- 20-50%：我方网关有额外 round-trip / 序列化 / TLS 终止成本，看 trace
- > 50%：通常是我方有同步外调（鉴权、限流、日志）拖累 prefill，看架构

---

## 故障处理

| 现象 | 处理 |
|---|---|
| 厂商报 429 限流 | 降 RPS 重跑，不要死磕 |
| 我方报 connection refused | URL 错了 / 我方服务没起 |
| upstream OK 但 our 大量 5xx | 我方栈问题 → 8.3 给工程 |
| 两次 model 字符串不一致 | history 分到两个目录，compare 找不到。**回到 Step 1 统一** |
| TPM 实测 > 厂商承诺 | 极少见——可能是厂商放开了限流。**和厂商对齐，确认不会回调** |
| 想跑更长时间 | 用 `--pattern soak`（自动写 metrics.json checkpoint），duration 拉到 1h+ |

---

## 进阶

- **批量接入**：3 个模型并行做 3 份 3 报告，给一个模型矩阵总览
- **长跑稳定性**：把 Step 3/5 改成 `--pattern soak --duration 3600` + `--soak-checkpoint 60`
- **区域对比**：同一模型、不同 region 都做 3 报告，画出 overhead-by-region 热图
- **回归基线**：第 N 次接入时，跟第 1 次做 power-test-compare，看 overhead 增长

---

## 参考

- `references/onboarding.md` — 完整 walkthrough、TPM/RPM 公式、SLA 阈值表
- `power-test-run` — 单端点压测的细节（Step 3/5 用的就是它）
- `power-test-compare` — 两次压测的 diff 解读（Step 6 用的就是它）
