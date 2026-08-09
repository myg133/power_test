# power_test 安装手册（agent 执行用）

> **用途**：用户把这个文件的 URL 发给一个全新的 agent。agent 按下面步骤执行，自动装好 `power_test` 二进制和 3 个配套 Mavis skill，最后告知用户装好了。
>
> **工作目录**：agent 当前所在的工作目录。脚本里所有相对路径都相对它。
>
> **预期结果**：`power_test --help` 能跑通，3 个 skill 在 Mavis skills 目录里就位。

---

## 0. 背景（30 秒读完）

`power_test` 是 Rust 写的 LLM 推理端点压测工具，能：

- 测 TTFT（首 token 延迟）、ITL（token 间延迟）、TPS（生成吞吐）、端到端延迟分位数
- 跑 4 种负载模式：恒定（constant）/ 升压（ramp）/ 尖峰（spike）/ 长跑（soak）
- 输出单文件 HTML 报告 + 文本 summary
- 把历史存到 `~/.power_test/history/<model>/<run-id>/`，方便以后对比

跨平台二进制从 GitHub Releases 拉：windows-x86_64、linux-x86_64、macos-arm64。

3 个 skill 和它们的触发词：

| skill | 用途 | 触发词举例 |
|---|---|---|
| `power-test-run` | 单端点压测，跑一次 → 一个 HTML 报告 | 压测、压一下 QPS、stress test、benchmark |
| `power-test-compare` | 把两次压测并排对比 | 对比压测、A vs B、跑了一次变了多少 |
| `power-test-onboard` | 上游厂商 + 我们端点 → 3 报告 SLA 流程 | 接新模型、接入上游、SLA 验证 |

agent 自己读完要能复述出来。**装完之后用户问什么就引导到什么 skill。**

---

## 1. 检测 OS 和架构

跨平台脚本：

```bash
# 1.1 OS
case "$(uname -s 2>/dev/null || echo "$OS")" in
  Linux*)                 OS=linux ;;
  Darwin*)                OS=macos ;;
  MINGW*|MSYS*|CYGWIN*)   OS=windows ;;
  *)                      OS=windows ;;   # Windows 下 uname 不一定在 PATH
esac

# 1.2 arch
case "$(uname -m 2>/dev/null || echo "$PROCESSOR_ARCHITECTURE")" in
  x86_64|amd64)           ARCH=x86_64 ;;
  arm64|aarch64)          ARCH=arm64 ;;
  *)                      echo "不支持的架构: $(uname -m)"; exit 1 ;;
esac

echo "检测到：OS=$OS ARCH=$ARCH"
```

Windows PowerShell 没有 `uname`。如果脚本在 PowerShell 里跑，agent 自己用 `$env:OS`（值固定是 `Windows_NT`）+ `$env:PROCESSOR_ARCHITECTURE`（值是 `AMD64` 或 `ARM64`）判断。bash 块只是模板，agent 自行翻译到当前 shell。

**如果 OS/arch 不在支持列表，直接告诉用户退出，**别瞎猜。

---

## 2. 装二进制

### 2.1 选资产

| OS | arch | 文件名 |
|---|---|---|
| windows | x86_64 | `power_test-<TAG>-windows-x86_64.zip` |
| linux | x86_64 | `power_test-<TAG>-linux-x86_64.tar.gz` |
| macos | arm64 | `power_test-<TAG>-macos-arm64.tar.gz` |

不支持的组合（如 macos x86_64、linux arm64）告诉用户：当前 release 没编这个目标，等上游补。**别去 cross-compile。**

### 2.2 拉最新版

```bash
TAG=$(curl -fsSL https://api.github.com/repos/myg133/power_test/releases/latest \
      | grep -m1 '"tag_name"' \
      | sed -E 's/.*"v?([^"]+)".*/\1/')
[ -z "$TAG" ] && { echo "拉不到最新 tag，检查网络或代理"; exit 1; }
echo "latest tag: v$TAG"
```

### 2.3 下载 + 解压 + 放到 PATH

```bash
ASSET="power_test-v${TAG}-${OS}-${ARCH}.${EXT}"
URL="https://github.com/myg133/power_test/releases/download/v${TAG}/${ASSET}"

# 装到用户级 bin 目录
case "$OS" in
  windows) BIN_DIR="$USERPROFILE/bin" ;;
  *)       BIN_DIR="$HOME/.local/bin" ;;
esac
mkdir -p "$BIN_DIR"

# 临时目录
TMP="$(mktemp -d)"
cd "$TMP"
curl -fsSL -o "$ASSET" "$URL" || { echo "下载失败: $URL"; exit 1; }

# 解压
if [ "$EXT" = "zip" ]; then
  powershell -NoProfile -Command "Expand-Archive -Force -Path '$ASSET' -DestinationPath ."
else
  tar xzf "$ASSET"
fi

# 移动 + 加执行权限
EXE="power_test$([ "$OS" = "windows" ] && echo .exe)"
mv "$EXE" "$BIN_DIR/"
chmod +x "$BIN_DIR/$EXE" 2>/dev/null || true
echo "二进制装到: $BIN_DIR/$EXE"
```

### 2.4 macOS 额外一步

未签名的二进制会被 Gatekeeper 拦。**给用户两个选项，自己选**：

```bash
# 选项 A：本次放行（agent 直接跑）
xattr -d com.apple.quarantine "$BIN_DIR/power_test"

# 选项 B：永久（"系统设置 → 隐私与安全性 → 仍要打开"，要用户点）
```

agent 跑完把这两条告诉用户。如果 agent 跑 `power_test --version` 报"无法打开，因为 Apple 无法检查恶意软件"，那就是要么 A 没跑、要么版本太新 Gatekeeper 缓存没刷——重新跑 A。

### 2.5 验证

```bash
"$BIN_DIR/$EXE" --version
"$BIN_DIR/$EXE" --help | head -20
```

如果 `--version` 报 "command not found"，那 `$BIN_DIR` 不在 PATH 里。把下面这一行交给用户贴进他们的 shell rc：

```bash
# bash / zsh
echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc   # 或 ~/.zshrc
# PowerShell
[Environment]::SetEnvironmentVariable('Path', "$env:USERPROFILE\bin;$env:Path", 'User')
```

---

## 3. 装 skill

Mavis skills 默认目录：

| OS | 路径 |
|---|---|
| windows | `%USERPROFILE%\.minimax\skills\` |
| macos / linux | `$HOME/.minimax/skills/` |

3 个活的 skill + 1 个 DEPRECATED 重定向：

```bash
SKILL_ROOT="${SKILL_ROOT:-$HOME/.minimax/skills}"
REPO_BASE="https://raw.githubusercontent.com/myg133/power_test/main/docs/skill"

fetch_skill() {
  local skill="$1"; shift
  for path in "$@"; do
    local url="$REPO_BASE/$skill/$path"
    local dest="$SKILL_ROOT/$skill/$path"
    mkdir -p "$(dirname "$dest")"
    curl -fsSL "$url" -o "$dest" || { echo "拉失败: $url"; return 1; }
  done
}

fetch_skill power-test-run \
  SKILL.md \
  references/patterns-and-datasets.md \
  references/report-interpretation.md

fetch_skill power-test-compare \
  SKILL.md \
  references/compare-interpretation.md

fetch_skill power-test-onboard \
  SKILL.md \
  references/onboarding.md

fetch_skill power-test \
  SKILL.md
```

> 如果 GitHub raw 访问不到（企业代理），让用户从已 clone 的仓库 `cp -r docs/skill/* $SKILL_ROOT/`，agent 接着跑。

---

## 4. 验证

```bash
# 4.1 二进制 OK
"$BIN_DIR/$EXE" --version

# 4.2 4 个 skill 都在
for s in power-test-run power-test-compare power-test-onboard power-test; do
  if [ -f "$SKILL_ROOT/$s/SKILL.md" ]; then
    echo "OK   $s/SKILL.md ($(wc -c < "$SKILL_ROOT/$s/SKILL.md") bytes)"
  else
    echo "MISS $s/SKILL.md"
  fi
done

# 4.3 references/ 也齐
ls "$SKILL_ROOT/power-test-run/references/"
```

期望：3 个活的 skill + 1 个 deprecated 重定向都在。

---

## 5. 可选：smoke test

如果用户给了测试端点（OpenAI 兼容、有 key、有 model），跑一个 5 秒的小流量验证：

```bash
"$BIN_DIR/$EXE" run \
  --target <URL> \
  --api openai \
  --api-key <KEY> \
  --model <MODEL> \
  --rps 1 --duration 5 \
  --log-level info
```

成功 → 报告路径打印出来就收。失败 → 把错误原文给用户，让他先排查端点。

**用户没给端点就跳过这步，**别瞎选一个公网 endpoint 测。

---

## 6. 交付

回话给用户，照这个格式：

```
power_test 装好了。

- 二进制：<BIN_DIR>/<EXE>   （已加到 PATH，需要新开 shell 或 source rc）
- 4 个 skill：power-test-run、power-test-compare、power-test-onboard
              + power-test（DEPRECATED，会自动跳到上面 3 个）
- 版本：v<TAG>

可以这样用：
  "压测 https://api.openai.com/v1 的 gpt-4o-mini，2 RPS 跑 60 秒"
  "对比我前两次 qwen36-27B 的压测结果"
  "接一个新模型 DeepSeek-V4，验证 SLA"
```

---

## 7. 故障模式

| 现象 | 原因 | 处理 |
|---|---|---|
| `curl` 拉 GitHub 超时 | 企业代理 / DNS | 告诉用户先 `curl -I https://github.com` 验证，或用本地 clone 方式 |
| `power_test --version` 报 "command not found" | PATH 没生效 | 让用户新开 shell 或 source rc；如果还不行，手动加 PATH |
| macOS 报 "无法检查恶意软件" | Gatekeeper | 跑 `xattr -d com.apple.quarantine`，或用户自己去系统设置放行 |
| 解压报 "Permission denied" | TMP 目录只读 | 换 `$HOME/.cache/power_test-install` 重试 |
| 某个 skill SKILL.md 拉失败 | repo 还没那个 skill | 跳过去，告诉用户该 skill 暂时没装；其他 3 个照常用 |

**任何一步失败，告诉用户卡在哪一步 + 怎么手动继续，别瞎编装好了。**
