---
name: power-test-run
description: |
  Run HTTP-level stress test against ONE LLM inference endpoint with
  the `power_test` CLI and produce a self-contained HTML report. Use
  when the user says "压测"、"压一下 QPS"、"stress test"、
  "benchmark one endpoint"、"测一下 TTFT/ITL/TPS"、or wants to
  validate TPM / RPM against SLA on a single endpoint. Supports
  OpenAI / Anthropic / raw HTTP, with patterns: constant / ramp /
  spike / soak. Do NOT use for: comparing two runs (use
  `power-test-compare`), the 3-report SLA workflow for a new upstream
  (use `power-test-onboard`), Locust/k6/wrk-style non-LLM load tests,
  GPU-side benchmarks (vLLM bench, sglang bench), or comparing
  framework features without sending real traffic.
---

# power-test-run（单端点压测 · 引导式）

> 一次只问用户一件事。等用户回答再进下一步。用户说"exit"/"cancel"/"算了"任何时候停。
> 用户如果一次性给了全部信息（"压一下 X 的 Y，Z RPS 跑 N 秒"），直接跳到 Step 7。

---

## Step 1 · 目标端点

**问用户**：

> 压测哪个端点？把完整 URL 给我。
> 例子：
>   - `https://api.openai.com/v1/chat/completions`
>   - `http://192.168.31.101:8317/v1/chat/completions`
>   - `https://api.anthropic.com/v1/messages`

**校验**：必须以 `http://` 或 `https://` 开头，否则让用户重输。

**默认**：无（必填）。

---

## Step 2 · API 协议

**问用户**：

> 这个端点用哪种协议？
>   1. **openai**（OpenAI 兼容，默认）— `/v1/chat/completions`，vLLM、ollama、TGI、llama.cpp 都算
>   2. **anthropic**（Anthropic Messages 原生）— `/v1/messages`
>   3. **responses**（M9，OpenAI agent 时代新接口）— `/v1/responses`
>   4. **raw**（其他 HTTP，自己拼 body）

**默认**：openai。

**判断小窍门**：
- URL 包含 `/chat/completions` 或 `/v1/` 且厂商是 OpenAI 系 → openai
- URL 是 `/v1/messages` 且厂商是 Anthropic → anthropic
- URL 是 `/v1/responses` 且模型是 o1/o3/gpt-4o+ → **responses**（stateful 多轮便宜）
- 其他情况问用户

**`responses` 模式特殊说明**：
- 多轮走 `previous_response_id`（不重发历史 `input`），token 成本低、首字节更快
- 适合 agent 场景（每次只发新工具结果、新指令）
- 配合 `--dataset custom` + TOML 多轮配置用 `dynamic_multi` 测对话链
- 老的 gpt-3.5 / 非 OpenAI 端点大概率不识别这个 URL，选 openai

---

## Step 3 · 认证

**问用户**：

> API key 怎么办？三选一：
>   1. **直接给我**（我用 `--api-key` 传，**不会写进 history**）
>   2. **我已设了 `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` 环境变量**（你说"用环境变量"就行）
>   3. **这个端点不用认证**（本地 ollama 等）

**默认**：1（直接给）。如果用户选 1，agent 记下来用命令行传，不写进配置文件。

---

## Step 4 · 模型名

**问用户**：

> 模型名是什么？按上游厂商的写法。
> 例子：`gpt-4o-mini`、`claude-3-5-sonnet-20240620`、`qwen36-27B`

**校验**：非空字符串。

---

## Step 5 · 负载模式 + 规模

**问用户**（4 选 1）：

> 用哪种压测模式？
>
> | 选项 | 模式 | 适用问题 | 追问规模 |
> |---|---|---|---|
> | **A** | constant（恒定） | 稳态吞吐/延迟 | RPS？持续秒数？ |
> | **B** | ramp（升压） | 延迟随压力怎么变 | 起始 RPS → 结束 RPS？持续秒数？ |
> | **C** | spike（尖峰） | 突发流量扛不扛得住 | 基线 RPS？尖峰 RPS？尖峰时刻？持续秒数？ |
> | **D** | soak（长跑） | 1 小时后还稳不稳 | RPS？持续秒数？ |

**默认**：A。

**默认值（用户不答时用）**：

| 字段 | 默认 |
|---|---|
| RPS | 1 |
| duration | 60s |
| concurrency | = RPS（让测试自然，不人为挤队列） |
| pattern | constant |

按用户选择追问具体规模。

### Step 5b · 停止条件（time-based vs count-based）

**问用户**（2 选 1）：

> 跑多久？
>   1. **跑到 N 秒就停**（默认，`--duration N`）— 适合"测 X 秒看稳态"或"压到崩"
>   2. **跑到 N 个请求就停**（`--max-requests N`）— 适合"发够 N 个看延迟分布"或"小流量配额测试"
>
> 第二种选 2 的话，再追问：要发够多少个？常见：50、100、300、1000。

**默认**：1（time-based）。只有用户明确说"发 N 个就停"/"打满 N 个"/"配额测试"才选 2。

**count-based 的两种典型用法**（在 进阶 也写了一份）：

```bash
# 1) "打到 N 个并发，请求都返回就结束"（突发饱和）
power_test run --rps 1000 --concurrency N --max-requests N --duration 600 ...

# 2) "每 10s 50 个，发满 300 个就结束"（配额）
power_test run --rps 5 --max-requests 300 --duration 600 ...
```

⚠️ **小流量/私部模型的警告**：突发饱和模式对模型 server 杀伤力大，**先小 N 试水**（4-8 个并发）确认 server 不会 OOM / 不会卡 GPU，再放大。私部模型（自己机器上的 vLLM / ollama）尤其要慢点升。

---

## Step 6 · prompt 来源

**问用户**（4 选 1）：

> 用什么 prompt？
>   1. **builtin**（默认）— 内置 10 个混合长度 prompt，覆盖中英文
>   2. **literal**（我给你一段文字）— 单条 static prompt
>   3. **JSON/JSONL 文件** — 自定义 prompt 列表
>   4. **ShareGPT 文件** — ShareGPT 格式
>   5. **多轮 TOML** — 测 cache 命中

**默认**：1（builtin）。

按答案追问：
- 2：追问 prompt 文本
- 3/4：追问文件绝对路径
- 5：追问 TOML 路径 + `static_multi` / `dynamic_multi` 哪个

---

## Step 7 · Review & Run

把拼好的命令回显给用户。模板：

```bash
power_test run \
  --target <URL> \
  --api <KIND> \
  --api-key <KEY>        # 或省略，如果用环境变量 \
  --model <NAME> \
  --rps <R> --duration <S> \
  --dataset <KIND> \
  --tag '<model>-rps<R>-dur<S>' \
  --log-level info
```

**问用户**：

> 看起来对吗？
>   1. **跑**
>   2. **改** — 告诉我要改什么（哪一项改成什么）
>   3. **取消**

跑 `power_test run <...>`。

**跑完后给用户 4 样东西**：

1. run_id 后 6 位（够在对话里指代这次跑）
2. `report.html` 的绝对路径（用户浏览器打开看图）
3. `summary.txt` 里的 p50 / p99，**原文照抄**（不要"四舍五入"、不要从图表上读数）
4. 一句话说明这是哪种测试：
   - "短 prompt（≤16 token）延迟基准" — 适合 SLA 对话
   - "全 prompt（256 token 默认）端到端吞吐" — 适合容量规划

**接下来问用户**：

> 要不要：
>   - 对比另一次压测？ → 交给 `power-test-compare`
> - 拿这组数字去验证 SLA（接新模型）？ → 交给 `power-test-onboard`
> - 收工

---

## 附：默认值速查

| 字段 | 默认值 |
|---|---|
| `--api` | openai |
| `--pattern` | constant |
| `--rps` | 1 |
| `--duration` | 60s |
| `--concurrency` | = RPS |
| `--dataset` | builtin |
| `--max-tokens` | 256 |
| `--stream` | true |
| `--tag` | `<model>-rps<R>-dur<S>` |
| `--api-key` | `$OPENAI_API_KEY` / `$ANTHROPIC_API_KEY` 环境变量 |
| `--log-level` | info |

任何字段用户不答 → 用默认值。

---

## 故障处理

| 现象 | 处理 |
|---|---|
| 用户答"我不知道" / "随便" | 用默认值，问"按默认跑？" |
| URL 不可达 | 让用户先 `curl -I <URL>` 验证 |
| model 端点不识别 | 让用户先 `curl -H "Authorization: Bearer $KEY" <URL>/models` 验证 |
| 跑出来全 401 | key 不对 / 没传 / 环境变量没 export |
| 跑出来全 4xx | 大概率 `--api` 选错了，回到 Step 2 |
| 用户中途要"对比" | 收尾，交给 `power-test-compare` |
| 用户中途要"接新模型" | 收尾，交给 `power-test-onboard` |
| 用户要"保存这次配置" | 引导：`power_test run --print-config > ./power_test.toml` 存盘 |

---

## 进阶

- **复用配置**：把上面的命令存成 `power_test.toml`，下次 `power_test run --config ./power_test.toml`。
- **批量轮换 prompt**：用 `--dataset custom --custom-path <file.jsonl>` 跑真业务流量。
- **长时间稳定性**：用 `--pattern soak`，自动每 N 秒写一次 `metrics.json` checkpoint。
- **跨环境对比**：跑两次（环境 A / 环境 B）后用 `power-test-compare`。
- **agent 多轮（Responses）**：用 `--api responses` + `--dataset custom` 配多轮 TOML（`dynamic_multi`），session pool 自动用 `previous_response_id` 接续，省 token。
- **count-based stop**（`--max-requests N`）—— 达到 N 个请求就停，不等时间。两种典型：
  - **突发饱和**：`--rps 1000 --concurrency N --max-requests N`，把 N 个并发一次性打出去，等它们都返回就结束。`--duration` 留大点（600s）当兜底。
  - **配额速率**：`--rps 5 --max-requests 300`，按 5 RPS（= 50/10s）持续发，到 300 个就停。`--duration` 留 60s 当理论时长，cap 会更早触发。
  - 触发后 in-flight 请求会走 10s drain 才退（拿到所有结果才出报告）。如果 server 太慢，drain 会超时并把还没返回的标记成 `interrupted: yes`。
  - **私部/小流量模型**：突发饱和会把 server 打挂。先 `--concurrency 4 --max-requests 8` 试水，确认 server 没事再放大。`power-test-run` v0.2.4+ 才有这个 flag。

---

## 参考

- `references/patterns-and-datasets.md` — 每个 pattern × 每个 dataset 组合的 flag 和适用场景
- `references/report-interpretation.md` — TTFT/ITL/TPS/p99 怎么读、怎么算 TPM/RPM
- `power-test-compare` — 两次压测并排 diff
- `power-test-onboard` — 上游 + 我方 3 报告 SLA 流程
