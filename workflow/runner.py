#!/usr/bin/env python3
"""
CoreGeek 自动化对战工作流 - 单次运行脚本

状态机:
  PULLCODE → (有更新) → GOBATTLE
  PULLCODE → (无更新, < threshold) → PULLCODE (停留)
  PULLCODE → (无更新, >= threshold, force_battle=1) → GOBATTLE (强制)
  GOBATTLE → (成功) → WAIT_BATTLE
  GOBATTLE → (失败 >= 3 次) → PULLCODE
  WAIT_BATTLE → (对战完成 + 日志下载) → ANALYZE
  WAIT_BATTLE → (超时) → PULLCODE
  ANALYZE → (improve_enabled=1) → IMPROVE
  ANALYZE → (improve_enabled=0, issue_submit_enabled=1) → ISSUESUBMIT
  ANALYZE → (两者都关) → PULLCODE
  IMPROVE → (issue_submit_enabled=1) → ISSUESUBMIT
  IMPROVE → (issue_submit_enabled=0) → PULLCODE
  ISSUESUBMIT → (完成) → PULLCODE

每个定时触发只执行当前 phase，完成后更新 state.json 进入下一阶段。
"""

import json
import os
import random
import shutil
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

# ==================== 配置 ====================
BASE_DIR = Path(__file__).resolve().parent
ROOT_DIR = BASE_DIR.parent  # D:/github/Competition
TOKENS_DIR = BASE_DIR / "tokens"
CONFIG_DIR = BASE_DIR / "config"
STATE_FILE = CONFIG_DIR / "state.json"
LOG_FILE = BASE_DIR / "runner.log"
LOGS_DIR = BASE_DIR / "logs"
ANALYSIS_DIR = BASE_DIR / "analysis"
TOKEN_FILE = TOKENS_DIR / "token.txt"

OUR_TEAM_ID = 4388
GAME_ID = 1
STAGE = "练习赛"
MAP = "attack_map"
BASE_URL = "https://coregeek.rnd.huawei.com:8005"
BATTLE_POLL_INTERVAL = 300  # 轮询对战完成情况的间隔（5分钟，由 cron 触发）
PROXY_FILE = TOKENS_DIR / "proxy.txt"
GITHUB_TOKEN_FILE = TOKENS_DIR / "github_token.txt"
GITHUB_REPO = "gamelife1314/Competition"
WORKFLOW_REQUEST_PATH = "docs/WORKFLOW_REQUEST.md"  # 新诉求文件
CONTROL_FILE = CONFIG_DIR / "control.yaml"  # 控制配置文件

# ==================== 工具函数 ====================

# 配置默认值（control.yaml 不存在或字段缺失时使用）
_CONTROL_DEFAULTS = {
    "paused": 0,
    "force_battle": 0,
    "issue_submit_enabled": 0,
    "improve_enabled": 1,
    "no_update_force_threshold": 4,
    "battle_wait_timeout": 2100,
    "max_battles_per_round": 16,
    "max_processing_battles": 50,
    "gobattle_max_retries": 3,
    "sleep_interval": 3,
    "max_download_per_batch": 200,
    "analysis_wait_timeout": 2400,
    "max_issues_per_batch": 5,
    "max_issues_per_batch_night": 10,
    "issue_night_start_hour": 22,
    "issue_night_end_hour": 8,
    "max_battles_per_round_night": 48,
    "battle_night_start_hour": 20,
    "battle_night_end_hour": 10,
    "improve_timeout": 1800,
}

_control_cache = None

def load_control():
    """加载控制配置文件 (config/control.yaml)，带缓存"""
    global _control_cache
    if _control_cache is not None:
        return _control_cache
    import yaml
    data = dict(_CONTROL_DEFAULTS)
    try:
        if CONTROL_FILE.exists():
            with open(CONTROL_FILE, "r", encoding="utf-8") as f:
                loaded = yaml.safe_load(f)
                if loaded:
                    data.update(loaded)
    except:
        pass
    _control_cache = data
    return data

def save_control(control):
    """保存控制配置文件（保留注释需要用 ruamel.yaml，这里简单覆盖）"""
    global _control_cache
    _control_cache = None  # 清缓存
    import yaml
    try:
        with open(CONTROL_FILE, "w", encoding="utf-8") as f:
            yaml.dump(control, f, default_flow_style=False, allow_unicode=True)
    except:
        pass

def get_config(key, default=None):
    """获取单个配置项"""
    return load_control().get(key, _CONTROL_DEFAULTS.get(key, default))

def is_night_for(prefix):
    """判断当前是否为夜间时段。prefix="issue" 或 "battle"，使用各自的时段配置"""
    now = datetime.now()
    hour = now.hour
    start = int(get_config(f"{prefix}_night_start_hour", 0))
    end = int(get_config(f"{prefix}_night_end_hour", 0))
    if start <= end:
        return start <= hour < end
    # 跨午夜：如 22:00 ~ 08:00
    return hour >= start or hour < end

def get_max_issues():
    """根据时段返回最大 issue 提交数：夜间多提交，白天少提交"""
    if is_night_for("issue"):
        return int(get_config("max_issues_per_batch_night", 10))
    return int(get_config("max_issues_per_batch", 5))

def get_max_battles():
    """根据时段返回最大对战发起数：夜间多发起，白天少发起"""
    if is_night_for("battle"):
        return int(get_config("max_battles_per_round_night", 48))
    return int(get_config("max_battles_per_round", 32))

# ==================== 日志 ====================

def log(msg):
    """记录日志到文件和 stdout"""
    ts = datetime.now().strftime("%Y-%m-%d %H:%M:%S")
    line = f"[{ts}] {msg}"
    print(line, flush=True)
    try:
        with open(LOG_FILE, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    except:
        pass

def get_token():
    """从 workflow/token.txt 获取 token"""
    if os.environ.get("COREGEEK_TOKEN"):
        return os.environ.get("COREGEEK_TOKEN")
    try:
        with open(TOKEN_FILE, "r") as f:
            token = f.read().strip()
        return token if token else ""
    except:
        return ""

def keep_alive():
    """检查 token 有效性，过期则调用 Playwright 自动刷新"""
    log("  检查 token 有效性 ...")
    token = get_token()
    if not token:
        log("  token 为空，尝试自动刷新 ...")
        if _refresh_token():
            log("  token 刷新成功")
            return True
        return False

    # 用 api_get（自带 token 刷新重试）验证
    result = _api_get_raw(
        "/queryTeamInfo",
        {"gameId": GAME_ID, "timestamp": int(time.time() * 1000)},
    )
    if result and result.get("meta", {}).get("isSuccess"):
        log("  token 有效")
        return True

    # token 失效，自动刷新
    log(f"  token 已过期: {result}")
    if _refresh_token():
        # 刷新后重新验证
        result = _api_get_raw(
            "/queryTeamInfo",
            {"gameId": GAME_ID, "timestamp": int(time.time() * 1000)},
        )
        if result and result.get("meta", {}).get("isSuccess"):
            log("  token 刷新成功并验证通过")
            return True
        log(f"  token 刷新后验证仍失败: {result}")
    else:
        log("  token 刷新失败")
    return False

def _refresh_token():
    """调用 Playwright 脚本自动登录刷新 token"""
    import subprocess
    login_dir = "D:/f00596107/2026_competition"
    login_script = "coregeek_login.js"
    remote_token_file = Path(login_dir) / "coregeek_token.txt"

    try:
        proc = subprocess.run(
            ["node", login_script],
            cwd=login_dir,
            capture_output=True,
            text=True,
            timeout=180,
        )
        stdout_text = proc.stdout or ""
        stderr_text = proc.stderr or ""
        if stdout_text:
            log(f"  Playwright stdout: {stdout_text[-300:]}")
        if stderr_text:
            log(f"  Playwright stderr: {stderr_text[-300:]}")

        # Playwright 脚本内部已处理 2FA 等待（NEEDS_2FA），这里不再额外等待

    except subprocess.TimeoutExpired:
        log("  Playwright 脚本超时（180s）")
        return False
    except Exception as e:
        log(f"  Playwright 脚本异常: {e}")
        return False

    if remote_token_file.exists():
        try:
            with open(remote_token_file, "r") as f:
                new_token = f.read().strip()
            if new_token.startswith("Bearer "):
                new_token = new_token[7:]
            if new_token:
                with open(TOKEN_FILE, "w") as f:
                    f.write(new_token)
                log("  新 token 已写入 token.txt")
                return True
        except:
            pass
    log("  未获取到新 token")
    return False

def _is_token_expired(response):
    """检测响应是否表示 token 过期（None 不算 token 过期，可能是网络错误或平台异常）"""
    if response is None:
        return False  # None 不一定是 token 问题，不触发刷新
    # API 返回错误码 102 (SING_TIMEOUT) 或 103 (PERMISSION_DENIED) 表示 token 问题
    code = response.get("code")
    if code in (102, 103):
        return True
    # 检查 meta 中的失败标记
    meta = response.get("meta", {})
    if meta and not meta.get("isSuccess", True):
        msg = meta.get("message", "")
        if "TIMEOUT" in msg or "PERMISSION" in msg or "DENIED" in msg:
            return True
    return False

def _refresh_and_retry(method, path, params=None, data=None, filepath=None, max_retries=1):
    """Token 失效时自动刷新并重试"""
    for attempt in range(max_retries + 1):
        if attempt > 0:
            log(f"  Token 失效，自动刷新后重试 (第 {attempt} 次) ...")
            if not _refresh_token():
                log("  Token 刷新失败，放弃重试")
                return None

        if method == "GET":
            result = _api_get_raw(path, params)
        elif method == "POST":
            result = _api_post_raw(path, data)
        elif method == "DOWNLOAD":
            result = _api_download_raw(path, filepath)
        else:
            return None

        # DOWNLOAD 返回 True/False
        if method == "DOWNLOAD":
            if result:
                return result
            # 下载失败可能是 token 问题，也可能是网络问题
            if attempt < max_retries:
                log("  下载失败，尝试刷新 token 重试 ...")
                continue
            return False

        # GET/POST 返回 dict 或 None
        # 只有响应中明确包含 token 失效信号时才刷新重试
        if result is not None and not _is_token_expired(result):
            return result

        # None 可能是网络错误、非 JSON 响应（如平台 500），不一定是 token 问题
        # 只有 _is_token_expired 明确返回 True 时才刷新
        if result is None:
            # 非-token 相关的失败，直接返回，不刷新
            return None

        if attempt < max_retries:
            continue

    return result

def _api_get_raw(path, params=None):
    """GET 请求（不处理 token 刷新）"""
    import urllib.request, urllib.parse
    url = BASE_URL + path
    if params:
        qs = urllib.parse.urlencode(params)
        url = f"{url}?{qs}"
    token = get_token()
    req = urllib.request.Request(url)
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=30) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        log(f"  API GET 失败: {e.code} - {body[:200]}")
        return None
    except Exception as e:
        log(f"  API GET 异常: {e}")
        return None

def _api_post_raw(path, data):
    """POST 请求（不处理 token 刷新）"""
    import urllib.request
    url = BASE_URL + path
    token = get_token()
    payload = json.dumps(data).encode("utf-8")
    req = urllib.request.Request(url, data=payload, method="POST")
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Content-Type", "application/json;charset=UTF-8")
    req.add_header("Accept", "application/json")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            body = resp.read().decode("utf-8", errors="replace")
            try:
                return json.loads(body)
            except json.JSONDecodeError:
                log(f"  API POST 响应非 JSON: {body[:200]}")
                return None
    except urllib.error.HTTPError as e:
        body = e.read().decode("utf-8", errors="replace")
        log(f"  API POST 失败: {e.code} - {body[:200]}")
        return None
    except Exception as e:
        log(f"  API POST 异常: {e}")
        return None

def _api_download_raw(path, filepath):
    """下载文件（不处理 token 刷新）"""
    import urllib.request
    url = BASE_URL + path
    token = get_token()
    req = urllib.request.Request(url)
    req.add_header("Authorization", f"Bearer {token}")
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            content = resp.read()
            with open(filepath, "wb") as f:
                f.write(content)
        return True
    except Exception as e:
        log(f"  下载失败: {e}")
        return False

def api_get(path, params=None):
    """GET 请求 - token 失效自动刷新重试"""
    return _refresh_and_retry("GET", path, params=params)

def api_post(path, data):
    """POST 请求 - token 失效自动刷新重试"""
    return _refresh_and_retry("POST", path, data=data)

def api_download(path, filepath):
    """下载文件 - token 失效自动刷新重试"""
    return _refresh_and_retry("DOWNLOAD", path, filepath=filepath)

def run_cmd(cmd, cwd=None):
    """运行 shell 命令"""
    try:
        result = subprocess.run(
            cmd, shell=True, cwd=cwd, capture_output=True, text=True, timeout=60
        )
        return result.returncode, result.stdout.strip(), result.stderr.strip()
    except subprocess.TimeoutExpired:
        return -1, "", "Command timed out"
    except Exception as e:
        return -1, "", str(e)

def get_proxy():
    """从 workflow/proxy.txt 读取代理 URL"""
    try:
        with open(PROXY_FILE, "r") as f:
            for line in f:
                line = line.strip()
                if "=" in line:
                    return line.split("=", 1)[1]
                if line:
                    return line
    except:
        pass
    return ""

def create_github_issue(title, body, labels=None):
    """通过 GitHub REST API 创建 Issue（使用代理）"""
    import urllib.request

    token = ""
    try:
        with open(GITHUB_TOKEN_FILE, "r") as f:
            token = f.read().strip()
    except:
        log("  GitHub token 文件不存在，跳过 Issue 创建")
        return None

    if not token:
        log("  GitHub token 为空，跳过 Issue 创建")
        return None

    url = f"https://api.github.com/repos/{GITHUB_REPO}/issues"
    payload = {"title": title, "body": body}
    if labels:
        payload["labels"] = labels
    body_data = json.dumps(payload).encode("utf-8")

    req = urllib.request.Request(url, data=body_data, method="POST")
    req.add_header("Authorization", f"token {token}")
    req.add_header("Accept", "application/vnd.github+json")
    req.add_header("Content-Type", "application/json")

    proxy_url = get_proxy()
    if proxy_url:
        proxy_handler = urllib.request.ProxyHandler({
            "http": proxy_url,
            "https": proxy_url,
        })
        opener = urllib.request.build_opener(proxy_handler)
    else:
        opener = urllib.request.build_opener()

    try:
        with opener.open(req, timeout=30) as resp:
            result = json.loads(resp.read().decode("utf-8"))
            issue_number = result.get("number")
            issue_url = result.get("html_url")
            log(f"  GitHub Issue 创建成功: #{issue_number} - {issue_url}")
            return issue_number
    except urllib.error.HTTPError as e:
        body_text = e.read().decode("utf-8", errors="replace")[:300]
        log(f"  GitHub Issue 创建失败: HTTP {e.code} - {body_text}")
        return None
    except Exception as e:
        log(f"  GitHub Issue 创建异常: {e}")
        return None

# ==================== 状态管理 ====================

def load_state():
    """加载状态文件"""
    if STATE_FILE.exists():
        with open(STATE_FILE, "r", encoding="utf-8") as f:
            return json.load(f)
    return {
        "phase": "PULLCODE",
        "last_commit": "",
        "iteration": 0,
        "pending_battles": [],
        "downloaded_pks": [],
        "analyzed_pks": [],
        "analyzed_opponents": [],
        "last_battle_time": "",
        "last_code_update_time": "",
        "no_update_count": 0,
        "issue_submitted": False,
        "gobattle_failures": 0,
        "initiated_opponents": [],  # 本轮发起挑战的对手列表 [{teamId, teamName}]
        "gobattle_initiated_time": "",  # 本轮发起对战的时间戳
        "wait_start_time": "",  # WAIT_BATTLE 开始时间
        "analyze_start_time": "",
        "issue_pending_pks": [],
        "last_update": "",
    }

def save_state(state):
    """保存状态文件"""
    state["last_update"] = datetime.now().isoformat()
    with open(STATE_FILE, "w", encoding="utf-8") as f:
        json.dump(state, f, indent=2, ensure_ascii=False)

def next_phase(phase):
    """获取下一阶段（仅用于简单推进，复杂跳转由 phase 函数自行管理）"""
    phases = {
        "PULLCODE": "GOBATTLE",
        "GOBATTLE": "WAIT_BATTLE",
        "WAIT_BATTLE": "ANALYZE",
        "ANALYZE": "IMPROVE",
        "IMPROVE": "ISSUESUBMIT",
        "ISSUESUBMIT": "PULLCODE",
    }
    return phases.get(phase, "PULLCODE")

# ==================== Phase 实现 ====================

def phase_pullcode(state):
    """PULLCODE: 拉取代码 + 检查是否有更新
    - paused=1 → 整个工作流暂停，不检查更新，停留 PULLCODE
    - 有更新 → GOBATTLE
    - 无更新，force_battle=1 且连续 >= 4 次 → 强制 GOBATTLE
    - 无更新，force_battle=0 → 停留 PULLCODE
    """
    log(">>> PULLCODE: 拉取代码并检查更新 ...")

    # 暂停检查：paused=1 时整个工作流暂停，不拉代码不检查更新
    control = load_control()
    if control.get("paused", 0) == 1:
        log("  工作流已暂停（paused=1），不检查代码更新")
        log("  编辑 workflow/config/control.yaml 将 paused 改为 0 恢复")
        state["phase"] = "PULLCODE"
        save_state(state)
        return True

    code, out, err = run_cmd("git pull origin main", cwd=str(ROOT_DIR))
    if code == 0:
        log(f"  拉取成功: {out[:200]}")
    else:
        log(f"  拉取失败: {err[:200]}")
        return False

    code, commit, _ = run_cmd("git rev-parse HEAD", cwd=str(ROOT_DIR))
    if code != 0:
        log("  获取 commit 失败")
        return False

    current_commit = commit
    last_commit = state.get("last_commit", "")
    now = datetime.now()

    if not last_commit:
        # 首次运行，记录 commit，进入 GOBATTLE
        log(f"  首次运行，记录 commit: {current_commit[:12]}")
        state["last_commit"] = current_commit
        state["last_code_update_time"] = now.isoformat()
        state["no_update_count"] = 0
        state["phase"] = "GOBATTLE"
        save_state(state)
        return True

    if current_commit == last_commit:
        # 代码无更新
        no_update_count = state.get("no_update_count", 0) + 1
        state["no_update_count"] = no_update_count

        # 强制对战检查
        force_switch_on = control.get("force_battle", 0) == 1
        if force_switch_on:
            threshold = get_config("no_update_force_threshold")
            log(f"  代码无更新 (commit: {current_commit[:12]})，连续 {no_update_count}/{threshold} 次（强制对战已开启）")
            if no_update_count >= threshold:
                log(f"  连续 {no_update_count} 次无更新，强制发起对战")
                state["no_update_count"] = 0
                state["phase"] = "GOBATTLE"
            else:
                log(f"  等待代码更新，停留 PULLCODE")
                state["phase"] = "PULLCODE"
        else:
            log(f"  代码无更新 (commit: {current_commit[:12]})，连续 {no_update_count} 次（强制对战已关闭，仅等待代码更新）")
            log(f"  等待代码更新，停留 PULLCODE")
            state["phase"] = "PULLCODE"
        save_state(state)
        return True

    # 检测到更新
    log(f"  检测到更新! {last_commit[:12]} → {current_commit[:12]}")
    state["last_commit"] = current_commit
    state["last_code_update_time"] = now.isoformat()
    state["no_update_count"] = 0
    state["iteration"] = state.get("iteration", 0) + 1
    log(f"  第 {state['iteration']} 轮迭代")

    # 检查 WORKFLOW_REQUEST.md 是否有更新（这是对 Agent 的新诉求）
    code, wf_diff, _ = run_cmd(
        f"git diff {last_commit}..{current_commit} -- {WORKFLOW_REQUEST_PATH}",
        cwd=str(ROOT_DIR),
    )
    if code == 0 and wf_diff.strip():
        log(f"  >>> WORKFLOW_REQUEST.md 有更新！新诉求如下：")
        log(wf_diff[:3000])
        state["workflow_request_updated"] = True
        state["workflow_request_diff"] = wf_diff[:5000]
    else:
        log(f"  WORKFLOW_REQUEST.md 无更新")
        state["workflow_request_updated"] = False

    state["phase"] = "GOBATTLE"
    save_state(state)
    return True

def phase_gobattle(state):
    """GOBATTLE: 打包上传代码 + 发起对战
    - 失败超过 3 次退回 PULLCODE
    - 成功后进入 WAIT_BATTLE
    """
    log(">>> GOBATTLE: 打包上传 + 发起对战 ...")

    gobattle_failures = state.get("gobattle_failures", 0)

    # 防重复：如果本轮已经上传+发起过对战（同一 commit），直接进 WAIT_BATTLE
    last_gobattle_commit = state.get("gobattle_commit", "")
    last_commit = state.get("last_commit", "")
    if last_gobattle_commit == last_commit and last_commit:
        log(f"  本轮已发起过对战（commit: {last_commit[:7]}），直接进 WAIT_BATTLE")
        state["gobattle_failures"] = 0
        state["phase"] = "WAIT_BATTLE"
        state["wait_start_time"] = datetime.now().isoformat()
        save_state(state)
        return True

    # === 打包上传 ===
    tar_file_abs = str(ROOT_DIR / "CoreGeek.tar.gz")

    log("  打包中 ...")
    code, out, err = run_cmd(
        f'tar -czf CoreGeek.tar.gz --exclude="CoreGeek/rust-toolchain.toml" --exclude="CoreGeek/target" CoreGeek/',
        cwd=str(ROOT_DIR),
    )
    if code != 0:
        log(f"  打包失败: {err[:200]}")
        gobattle_failures += 1
        state["gobattle_failures"] = gobattle_failures
        if gobattle_failures >= get_config("gobattle_max_retries"):
            log(f"  GOBATTLE 连续失败 {gobattle_failures} 次，退回 PULLCODE")
            state["gobattle_failures"] = 0
            state["phase"] = "PULLCODE"
        save_state(state)
        return True

    try:
        size = os.path.getsize(tar_file_abs)
        size_str = f"{size / 1024:.1f}KB"
    except:
        size_str = "未知"
    log(f"  打包完成: CoreGeek.tar.gz ({size_str})")

    log("  上传中 ...")
    import urllib.request
    token = get_token()
    boundary = "----WorkflowBoundary" + str(int(time.time()))
    with open(tar_file_abs, "rb") as f:
        file_data = f.read()

    body = (
        f"--{boundary}\r\n"
        f'Content-Disposition: form-data; name="file"; filename="CoreGeek.tar.gz"\r\n'
        f"Content-Type: application/x-gzip\r\n\r\n"
    ).encode() + file_data + f"\r\n--{boundary}--\r\n".encode()

    url = f"{BASE_URL}/uploadAnswer?gameId={GAME_ID}"
    req = urllib.request.Request(url, data=body, method="POST")
    req.add_header("Authorization", f"Bearer {token}")
    req.add_header("Content-Type", f"multipart/form-data; boundary={boundary}")

    try:
        with urllib.request.urlopen(req, timeout=120) as resp:
            log(f"  上传成功 (HTTP {resp.status})")
    except urllib.error.HTTPError as e:
        log(f"  上传失败 (HTTP {e.code})")
        gobattle_failures += 1
        state["gobattle_failures"] = gobattle_failures
        if gobattle_failures >= get_config("gobattle_max_retries"):
            log(f"  GOBATTLE 连续失败 {gobattle_failures} 次，退回 PULLCODE")
            state["gobattle_failures"] = 0
            state["phase"] = "PULLCODE"
        save_state(state)
        return True
    except Exception as e:
        log(f"  上传异常: {e}")
        gobattle_failures += 1
        state["gobattle_failures"] = gobattle_failures
        if gobattle_failures >= get_config("gobattle_max_retries"):
            log(f"  GOBATTLE 连续失败 {gobattle_failures} 次，退回 PULLCODE")
            state["gobattle_failures"] = 0
            state["phase"] = "PULLCODE"
        save_state(state)
        return True

    log(f"  等待 {get_config("sleep_interval")} 秒 ...")
    time.sleep(get_config("sleep_interval"))

    # === 发起对战 ===
    log("  查询当前 processing 对战 ...")
    pk_records = api_get(
        "/contest/getResultPkList",
        {"gameId": GAME_ID, "stage": STAGE, "pageIndex": 1, "pageSize": 100, "teamAid": OUR_TEAM_ID},
    )
    processing_count = 0
    if pk_records and "record" in pk_records:
        processing_count = len([r for r in pk_records["record"] if r.get("state") == "processing"])
    log(f"  当前 processing 对战: {processing_count} 场")

    if processing_count >= get_config("max_processing_battles"):
        log(f"  processing 对战 >= {get_config("max_processing_battles")}，跳过发起，直接等待")
        state["gobattle_failures"] = 0
        state["phase"] = "WAIT_BATTLE"
        state["wait_start_time"] = datetime.now().isoformat()
        save_state(state)
        return True

    log("  获取排名列表 ...")
    rank_data = api_get(
        "/contest/getRankList",
        {"stage": STAGE, "pageIndex": 1, "pageSize": 64},
    )
    if not rank_data or "record" not in rank_data:
        log("  获取排名失败")
        gobattle_failures += 1
        state["gobattle_failures"] = gobattle_failures
        if gobattle_failures >= get_config("gobattle_max_retries"):
            log(f"  GOBATTLE 连续失败 {gobattle_failures} 次，退回 PULLCODE")
            state["gobattle_failures"] = 0
            state["phase"] = "PULLCODE"
        save_state(state)
        return True

    records = rank_data["record"]
    # 找到我方排名位置
    our_rank = None
    for i, r in enumerate(records):
        if r["teamId"] == OUR_TEAM_ID:
            our_rank = i
            break
    if our_rank is None:
        our_rank = len(records) // 2
        log(f"  未找到我方排名，假设为 {our_rank}")

    log(f"  我方排名: {our_rank + 1}/{len(records)}")

    all_teams = [r for r in records if r["teamId"] != OUR_TEAM_ID]

    # 按排名顺序选队伍（夜间时段可以多发起）
    max_battles = get_max_battles()
    selected = all_teams[:max_battles]

    # 不超过 max_battles - processing_count
    max_new = max_battles - processing_count
    if len(selected) > max_new:
        selected = selected[:max_new]

    night_tag = "（夜间）" if is_night_for("battle") else ""
    log(f"  选中 {len(selected)} 支队伍（排名前 {max_battles}）{night_tag}:")
    for i, r in enumerate(selected):
        log(f"    [{i+1}] teamId={r['teamId']}  {r['teamName']}  score={r['score']}  win={r['win']}")

    success = 0
    initiated_opponents = []  # 记录本轮发起挑战的对手 (teamId, teamName)
    for idx, r in enumerate(selected):
        team_a = r["teamId"]
        team_name = r["teamName"]

        log(f"  发起挑战 {team_name} (teamId={team_a}) ...")
        payload = {
            "teamAid": team_a,
            "teamBid": OUR_TEAM_ID,
            "map": MAP,
            "gameId": GAME_ID,
            "type": "user",
            "typeNumber": None,
            "number": None,
            "stage": STAGE,
        }
        result = api_post("/contest/addResultPk", payload)
        if result:
            log(f"    -> 成功")
            success += 1
        else:
            log(f"    -> API 异常，对战可能已创建（平台已知行为）")
            success += 1
        initiated_opponents.append({"teamId": team_a, "teamName": team_name})

        if idx < len(selected) - 1:
            log(f"    等待 {get_config("sleep_interval")} 秒 ...")
            time.sleep(get_config("sleep_interval"))

    log(f"  对战发起完成: 成功 {success}")
    state["last_battle_time"] = datetime.now().isoformat()
    state["gobattle_failures"] = 0
    state["gobattle_commit"] = state.get("last_commit", "")  # 标记本轮已发起对战，防止重复
    state["initiated_opponents"] = initiated_opponents  # 本轮发起挑战的对手列表
    state["gobattle_initiated_time"] = state["last_battle_time"]  # 发起时间，用于过滤对战记录
    state["issue_submitted"] = False  # 对战已发起，清除 issue 标记
    state["phase"] = "WAIT_BATTLE"
    state["wait_start_time"] = datetime.now().isoformat()
    save_state(state)
    return True

def phase_wait_battle(state):
    """WAIT_BATTLE: 等待所有对战完成 + 下载全部新日志
    - 从 GOBATTLE 进入时记录 wait_start_time
    - 每 5 分钟轮询一次，检查是否还有 processing 状态的对战
    - 所有对战完成（无 processing）或超时（60分钟）后开始收集
    - 下载自上次收集以来所有新的 successful 对战日志（不限批次，全部下载）
    - 按对战场次归档，meta.json 记录详细信息（两队分数、发起时间、完成时间等）
    - 下载完成后进入 ANALYZE
    """
    log(">>> WAIT_BATTLE: 等待所有对战完成 ...")

    wait_start_str = state.get("wait_start_time", "")
    if wait_start_str:
        try:
            wait_start = datetime.fromisoformat(wait_start_str)
            elapsed = (datetime.now() - wait_start).total_seconds()
        except:
            elapsed = 0
            wait_start = datetime.now()
            state["wait_start_time"] = wait_start.isoformat()
    else:
        wait_start = datetime.now()
        state["wait_start_time"] = wait_start.isoformat()
        elapsed = 0

    # 轮询检查是否还有 processing 状态的对战
    records = api_get(
        "/contest/getResultPkList",
        {"gameId": GAME_ID, "stage": STAGE, "pageIndex": 1, "pageSize": 200, "teamAid": OUR_TEAM_ID},
    )
    if not records or "record" not in records:
        log("  获取对战记录失败，下次重试")
        save_state(state)
        return True

    all_records = records["record"]
    processing = [r for r in all_records if r.get("state") == "processing"]
    successful = [r for r in all_records if r.get("state") == "successful"]
    log(f"  对战记录: {len(all_records)} 条（processing {len(processing)} / successful {len(successful)}）, 已等待 {elapsed:.0f}s")

    # 如果还有 processing 且未超时，继续等待
    if processing and elapsed < get_config("battle_wait_timeout"):
        log(f"  仍有 {len(processing)} 场对战进行中，继续等待（超时 {get_config("battle_wait_timeout")}s）...")
        save_state(state)
        return True

    if processing and elapsed >= get_config("battle_wait_timeout"):
        log(f"  超时 {get_config("battle_wait_timeout")}s，仍有 {len(processing)} 场未完成，强制收集已完成的对战")

    log(f"  所有对战已完成（或超时），开始收集日志 ...")

    # 获取对战记录（包含我方发起的和别人挑战我们的）
    records = api_get(
        "/contest/getResultPkList",
        {"gameId": GAME_ID, "stage": STAGE, "pageIndex": 1, "pageSize": 200, "teamAid": OUR_TEAM_ID},
    )
    if not records or "record" not in records:
        log("  获取对战记录失败，下次重试")
        save_state(state)
        return True

    all_records = records["record"]

    # 只下载本轮发起的对战日志（通过对手 teamId 匹配）
    # 平台 addResultPk 不返回 pk ID，所以用发起时记录的对手列表来过滤
    initiated_opponents = state.get("initiated_opponents", [])
    initiated_team_ids = set(opp["teamId"] for opp in initiated_opponents)
    initiated_time_str = state.get("gobattle_initiated_time", "")

    downloaded_pks = set(state.get("downloaded_pks", []))
    new_battles = [r for r in successful if r["id"] not in downloaded_pks]

    if initiated_team_ids:
        # 过滤：对手 teamId 在本轮发起列表中，且 createTime 在发起时间之后
        # 我方是 teamB（发起方），对手是 teamA
        # 也包含别人挑战我们的对战（teamBid == OUR_TEAM_ID, teamAid 不在发起列表）
        our_initiated = []
        challenges_us = []
        for r in new_battles:
            team_a_id = r.get("teamAid")
            team_b_id = r.get("teamBid")
            create_time = r.get("createTime", "")

            # 我方发起的：teamA 是对手，teamB 是我们
            if team_b_id == OUR_TEAM_ID and team_a_id in initiated_team_ids:
                our_initiated.append(r)
            # 别人挑战我们的：teamA 是我们，teamB 是对手
            elif team_a_id == OUR_TEAM_ID:
                challenges_us.append(r)

        new_battles = our_initiated + challenges_us
        log(f"  本轮发起对战: {len(initiated_team_ids)} 个对手, 匹配到 {len(our_initiated)} 场我方发起 + {len(challenges_us)} 场被挑战 = {len(new_battles)} 场")
    else:
        log(f"  无发起记录，下载所有新增对战: {len(new_battles)} 场")

    if not new_battles:
        log("  没有新增对战日志可下载，进入 ANALYZE")
        state["phase"] = "ANALYZE"
        state["wait_start_time"] = ""
        save_state(state)
        return True

    # 下载全部新对战日志（不限批次）
    batch = new_battles[:get_config("max_download_per_batch")]
    log(f"  开始下载 {len(batch)} 场对战日志 ...")

    success = 0
    fail = 0
    collected_pks = []

    for idx, r in enumerate(batch):
        pk_id = r["id"]
        team_a_id = r["teamAid"]
        team_b_id = r["teamBid"]
        team_a_name = r.get("teamAName", str(team_a_id))
        team_b_name = r.get("teamBName", str(team_b_id))
        score_a = r.get("scoreA", "?")
        score_b = r.get("scoreB", "?")
        create_time = r.get("createTime")
        update_time = r.get("updateTime")
        state_str = r.get("state", "?")

        # 判断我方胜负
        our_side = "A" if team_a_id == OUR_TEAM_ID else "B"
        our_score_val = score_a if our_side == "A" else score_b
        opp_score_val = score_b if our_side == "A" else score_a
        if our_score_val is not None and opp_score_val is not None:
            if our_score_val > opp_score_val:
                result_cn = "胜"
            elif our_score_val < opp_score_val:
                result_cn = "负"
            else:
                result_cn = "平"
        else:
            result_cn = "未知"

        opponent_name = team_b_name if our_side == "A" else team_a_name
        log(f"  [{idx+1}/{len(batch)}] pk={pk_id}  {result_cn} vs {opponent_name}  比分={score_a}:{score_b}  发起={create_time}  完成={update_time}")

        pk_dir = LOGS_DIR / f"pk{pk_id}"
        pk_dir.mkdir(parents=True, exist_ok=True)

        # 详细归档 meta.json
        meta = {
            "pk_id": pk_id,
            "state": state_str,
            "result": result_cn,
            "our_side": our_side,
            "team_a_id": team_a_id,
            "team_a_name": team_a_name,
            "team_b_id": team_b_id,
            "team_b_name": team_b_name,
            "score_a": score_a,
            "score_b": score_b,
            "our_score": our_score_val,
            "opp_score": opp_score_val,
            "total_point_a": r.get("totalPointA"),
            "total_point_b": r.get("totalPointB"),
            "point_difference": r.get("pointDifference"),
            "map": r.get("map"),
            "create_time": create_time,
            "update_time": update_time,
            "download_time": datetime.now().isoformat(),
        }
        with open(pk_dir / "meta.json", "w", encoding="utf-8") as f:
            json.dump(meta, f, indent=2, ensure_ascii=False)

        # 下载双方日志
        for side, side_id, side_name in [("teamA", team_a_id, team_a_name), ("teamB", team_b_id, team_b_name)]:
            safe_name = str(side_name).replace("/", "_").replace("\\", "_").replace(" ", "_")
            filename = f"{side}_{side_id}_{safe_name}.log"
            filepath = pk_dir / filename

            if filepath.exists():
                log(f"    -> {side}: 已存在，跳过")
                continue

            url = f"/contest/downFile?type=pk&id={pk_id}&name={side}"
            if api_download(url, str(filepath)):
                size_kb = os.path.getsize(filepath) / 1024
                log(f"    -> {side}: {size_kb:.1f}KB  {filename}")
                success += 1
            else:
                log(f"    -> {side}: 下载失败")
                fail += 1

            time.sleep(get_config("sleep_interval"))

        downloaded_pks.add(pk_id)
        collected_pks.append(pk_id)
        state["downloaded_pks"] = list(downloaded_pks)

    log(f"  下载完成: 成功 {success} / 失败 {fail}，共收集 {len(collected_pks)} 场")

    # 统计失败场次
    loss_pks = []
    for pk_id in collected_pks:
        pk_dir = LOGS_DIR / f"pk{pk_id}"
        meta_file = pk_dir / "meta.json"
        if meta_file.exists():
            try:
                with open(meta_file, "r", encoding="utf-8") as f:
                    m = json.load(f)
                if m.get("result") == "负":
                    loss_pks.append(pk_id)
            except:
                pass
    log(f"  本批失败场次: {len(loss_pks)} 场 (pk: {', '.join(str(p) for p in loss_pks) if loss_pks else '无'})")

    state["phase"] = "ANALYZE"
    state["wait_start_time"] = ""
    save_state(state)
    return True

def phase_analyze(state):
    """ANALYZE: 数据准备阶段
    - 记录上次分析到当前的所有新对战（包括别人挑战我们的）
    - 对每场下载的我方日志跑 collect_log.py 收集六张表
    - 准备 receipt 信息（pk ID、代码版本、胜负、比分等）
    - 不做 LLM 日志分析，只按 WORKFLOW_REQUEST 要求准备数据
    - 完成后进入 ISSUESUBMIT
    """
    log(">>> ANALYZE: 准备对战数据 ...")

    analyzed_pks = set(state.get("analyzed_pks", []))
    downloaded_pks = set(state.get("downloaded_pks", []))
    pending = downloaded_pks - analyzed_pks

    if not pending:
        log("  所有对战已准备完毕")
        existing_issue_pks = state.get("issue_pending_pks", [])
        # 根据开关决定下一阶段
        if get_config("improve_enabled", 1) == 1:
            state["phase"] = "IMPROVE"
        elif get_config("issue_submit_enabled", 0) == 1:
            state["phase"] = "ISSUESUBMIT"
        else:
            state["phase"] = "PULLCODE"
        state["analyze_start_time"] = ""
        save_state(state)
        return True

    log(f"  待准备对战: {len(pending)} 场")
    ready_pks = []

    # 获取当前代码版本
    code, commit_hash, _ = run_cmd("git rev-parse HEAD", cwd=str(ROOT_DIR))
    code, commit_time, _ = run_cmd('git log -1 --format="%ai"', cwd=str(ROOT_DIR))
    commit_short = commit_hash[:7] if commit_hash else "unknown"

    for pk_id in sorted(pending):
        pk_dir = LOGS_DIR / f"pk{pk_id}"
        meta_file = pk_dir / "meta.json"

        if not meta_file.exists():
            log(f"  pk{pk_id}: 缺少 meta.json，跳过")
            analyzed_pks.add(pk_id)
            continue

        with open(meta_file, "r", encoding="utf-8") as f:
            meta = json.load(f)

        our_side = meta.get("our_side", "B")
        our_score = meta.get("score_a") if our_side == "A" else meta.get("score_b")
        opp_score = meta.get("score_b") if our_side == "A" else meta.get("score_a")
        our_point = meta.get("total_point_a") if our_side == "A" else meta.get("total_point_b")
        opp_point = meta.get("total_point_b") if our_side == "A" else meta.get("total_point_a")
        opponent_name = meta.get("team_b_name") if our_side == "A" else meta.get("team_a_name")
        opponent_id = meta.get("team_b_id") if our_side == "A" else meta.get("team_a_id")

        if our_score is not None and opp_score is not None:
            if our_score > opp_score:
                result = "WIN"
            elif our_score < opp_score:
                result = "LOSS"
            else:
                result = "DRAW"
        else:
            result = "UNKNOWN"

        log(f"  pk{pk_id}: {result} vs {opponent_name} (teamId={opponent_id})  比分={our_score}:{opp_score}  积分={our_point}:{opp_point}")

        # 找到我方日志文件
        our_log_file = None
        for f in pk_dir.glob("*.log"):
            if f.name.startswith(f"team{our_side}_") and str(OUR_TEAM_ID) in f.name:
                our_log_file = f
                break

        if not our_log_file:
            log(f"  pk{pk_id}: 找不到我方日志文件，跳过")
            analyzed_pks.add(pk_id)
            continue

        # 跑 collect_log.py 收集六张表
        collect_output_file = pk_dir / "collect_output.txt"
        coach_output_file = pk_dir / "coach_output.txt"

        log(f"  pk{pk_id}: 运行 collect_log.py ...")
        code, out, err = run_cmd(
            f'python tools/collect_log.py "{our_log_file}"',
            cwd=str(ROOT_DIR),
        )
        if code == 0:
            with open(collect_output_file, "w", encoding="utf-8") as f:
                f.write(out)
            log(f"  pk{pk_id}: collect_log.py 完成 ({len(out)} 字节)")
        else:
            log(f"  pk{pk_id}: collect_log.py 失败: {err[:200]}")
            with open(collect_output_file, "w", encoding="utf-8") as f:
                f.write(f"collect_log.py 运行失败:\n{err}")

        # 跑 collect_log.py --coach
        code, out2, err2 = run_cmd(
            f'python tools/collect_log.py "{our_log_file}" --coach',
            cwd=str(ROOT_DIR),
        )
        if code == 0:
            with open(coach_output_file, "w", encoding="utf-8") as f:
                f.write(out2)
            log(f"  pk{pk_id}: collect_log.py --coach 完成")
        else:
            log(f"  pk{pk_id}: collect_log.py --coach 失败: {err2[:200]}")
            with open(coach_output_file, "w", encoding="utf-8") as f:
                f.write(f"collect_log.py --coach 运行失败:\n{err2}")

        # 准备 receipt 数据
        receipt = {
            "match_id": f"pk{pk_id}",
            "base_commit": commit_short,
            "base_commit_time": commit_time.strip() if commit_time else None,
            "opponent": {"team_id": str(opponent_id), "team_name": opponent_name},
            "result": "win" if result == "WIN" else "loss" if result == "LOSS" else "draw" if result == "DRAW" else "unknown",
            "result_source": "judger",
            "score": {"ours": our_score, "enemy": opp_score},
            "battle_time": meta.get("create_time"),
            "our_point": our_point,
            "opp_point": opp_point,
        }

        receipt_file = pk_dir / "receipt.json"
        with open(receipt_file, "w", encoding="utf-8") as f:
            json.dump(receipt, f, indent=2, ensure_ascii=False)

        ready_pks.append(pk_id)
        analyzed_pks.add(pk_id)

    state["analyzed_pks"] = list(analyzed_pks)
    state["issue_pending_pks"] = ready_pks
    state["analyze_start_time"] = ""
    # 根据开关决定下一阶段
    if get_config("improve_enabled", 1) == 1:
        state["phase"] = "IMPROVE"
        log(f"  数据准备完成，进入 IMPROVE（{len(ready_pks)} 场）")
    elif get_config("issue_submit_enabled", 0) == 1:
        state["phase"] = "ISSUESUBMIT"
        log(f"  数据准备完成，进入 ISSUESUBMIT（{len(ready_pks)} 场）")
    else:
        state["phase"] = "PULLCODE"
        log(f"  数据准备完成，improve 和 issue_submit 均关闭，回到 PULLCODE")
    save_state(state)
    return True


def phase_improve(state):
    """IMPROVE: 基于失败对战的 collect/coach 数据，自动优化代码并提交
    - 读取本批失败对战的 collect_output + coach_output + receipt
    - 生成改进方向摘要，写入 analysis/improve_direction.md
    - 调用 codeagent CLI 自动优化代码
    - cargo build 验证编译
    - git commit + push
    - 完成后进入 GOBATTLE 发起挑战
    """
    log(">>> IMPROVE: 自动分析失败 + 优化代码 ...")

    issue_pending_pks = state.get("issue_pending_pks", [])
    if not issue_pending_pks:
        log("  无待改进对战数据，直接进入下一阶段")
        _improve_advance(state)
        return True

    # --- 收集失败场次的诊断数据 ---
    loss_battles = []
    for pk_id in sorted(issue_pending_pks):
        pk_dir = LOGS_DIR / f"pk{pk_id}"
        meta_file = pk_dir / "meta.json"
        receipt_file = pk_dir / "receipt.json"
        collect_file = pk_dir / "collect_output.txt"
        coach_file = pk_dir / "coach_output.txt"

        meta = {}
        if meta_file.exists():
            try:
                with open(meta_file, "r", encoding="utf-8") as f:
                    meta = json.load(f)
            except:
                pass

        receipt = {}
        if receipt_file.exists():
            try:
                with open(receipt_file, "r", encoding="utf-8") as f:
                    receipt = json.load(f)
            except:
                pass

        our_side = meta.get("our_side", "B")
        our_score = meta.get("score_a") if our_side == "A" else meta.get("score_b")
        opp_score = meta.get("score_b") if our_side == "A" else meta.get("score_a")
        opponent_name = meta.get("team_b_name") if our_side == "A" else meta.get("team_a_name")

        if our_score is not None and opp_score is not None:
            if our_score > opp_score:
                continue  # 跳过胜利
            result_cn = "负" if our_score < opp_score else "平"
        else:
            continue

        collect_text = ""
        if collect_file.exists():
            try:
                with open(collect_file, "r", encoding="utf-8", errors="replace") as f:
                    collect_text = f.read()
            except:
                pass

        coach_text = ""
        if coach_file.exists():
            try:
                with open(coach_file, "r", encoding="utf-8", errors="replace") as f:
                    coach_text = f.read()
            except:
                pass

        loss_battles.append({
            "pk_id": pk_id,
            "opponent_name": opponent_name,
            "our_score": our_score,
            "opp_score": opp_score,
            "collect_text": collect_text,
            "coach_text": coach_text,
            "receipt": receipt,
        })

    if not loss_battles:
        log("  本批无失败场次，无需改进")
        _improve_advance(state)
        return True

    log(f"  本批 {len(loss_battles)} 场失败，生成改进方向 ...")

    # --- 生成改进方向摘要 ---
    improve_lines = []
    improve_lines.append(f"# 改进方向 — {datetime.now().strftime('%Y-%m-%d %H:%M')}\n")
    improve_lines.append(f"本批 {len(loss_battles)} 场失败：\n")
    for b in loss_battles:
        improve_lines.append(
            f"- pk{b['pk_id']}: 负 {b['our_score']}:{b['opp_score']} vs {b['opponent_name']}\n"
        )
    improve_lines.append("\n## 各场诊断数据\n")
    for b in loss_battles:
        improve_lines.append(f"\n### pk{b['pk_id']} vs {b['opponent_name']}\n")
        improve_lines.append(f"比分: {b['our_score']}:{b['opp_score']}\n")
        if b["coach_text"]:
            improve_lines.append(f"\n**教练日志:**\n```\n{b['coach_text'][:2000]}\n```\n")
        if b["collect_text"]:
            improve_lines.append(f"\n**六张表:**\n```\n{b['collect_text'][:3000]}\n```\n")

    improve_file = ANALYSIS_DIR / "improve_direction.md"
    ANALYSIS_DIR.mkdir(parents=True, exist_ok=True)
    with open(improve_file, "w", encoding="utf-8") as f:
        f.writelines(improve_lines)
    log(f"  改进方向已写入: {improve_file}")

    # --- 构建给 codeagent 的 prompt ---
    # 只取前 3 场的摘要（避免 prompt 过长）
    summary_parts = []
    for b in loss_battles[:3]:
        part = f"pk{b['pk_id']} 负 {b['our_score']}:{b['opp_score']} vs {b['opponent_name']}"
        if b["coach_text"]:
            # 取 coach 日志前 500 字符
            part += f"\n教练日志:\n{b['coach_text'][:500]}"
        if b["collect_text"]:
            # 取六张表前 800 字符
            part += f"\n六张表:\n{b['collect_text'][:800]}"
        summary_parts.append(part)

    battle_summary = "\n\n---\n\n".join(summary_parts)

    prompt = f"""Based on the following battle analysis data from lost matches, improve the CoreGeek bot strategy code.

## Lost Battles Summary

{battle_summary}

## Instructions

1. Analyze the failure patterns from the coach logs and six-table data above
2. Identify the top 1-2 most impactful improvements to the Rust strategy code in CoreGeek/src/
3. Make the code changes directly — edit the relevant files
4. Run `cargo build --release` to verify compilation
5. Run `cargo test --release -p coregeek --lib --tests` to verify tests pass
6. Do NOT create documentation files or add comments unless directly related to the fix
7. Focus on strategy improvements: combat targeting, economy spending, night defense, task system

The code is a Rust game bot. Key files:
- CoreGeek/src/brain/combat.rs — night combat targeting
- CoreGeek/src/brain/economy.rs — economy and build decisions
- CoreGeek/src/brain/night.rs — night phase strategy
- CoreGeek/src/brain/day.rs — day phase strategy
- CoreGeek/src/brain/task.rs — LLM task system

Make the minimal effective change. Commit is not needed — the workflow will handle git operations.
"""

    # --- 调用 codeagent CLI ---
    log("  调用 codeagent CLI 进行代码优化 ...")
    improve_timeout = int(get_config("improve_timeout", 1800))
    # 查找 codeagent CLI：优先 PATH，回退到已知安装路径
    codeagent_bin = shutil.which("codeagent") or str(
        Path("D:/Program Files/CodeAgentCLI/codeagent")
    )
    cmd = [codeagent_bin, "--print", "--dangerously-skip-permissions", prompt]
    log(f"  命令: {codeagent_bin} (timeout={improve_timeout}s)")

    try:
        proc = subprocess.run(
            cmd,
            cwd=str(ROOT_DIR),
            capture_output=True,
            text=True,
            timeout=improve_timeout,
        )
        log(f"  codeagent 退出码: {proc.returncode}")
        if proc.stdout:
            # 只记录最后 2000 字符
            tail = proc.stdout[-2000:] if len(proc.stdout) > 2000 else proc.stdout
            log(f"  codeagent 输出 (尾部):\n{tail}")
        if proc.stderr:
            tail = proc.stderr[-500:] if len(proc.stderr) > 500 else proc.stderr
            log(f"  codeagent stderr (尾部):\n{tail}")
    except subprocess.TimeoutExpired:
        log(f"  codeagent 超时 ({improve_timeout}s)，使用已有改动继续")
    except FileNotFoundError:
        log("  codeagent CLI 未找到，跳过自动改进")
        _improve_advance(state)
        return True
    except Exception as e:
        log(f"  codeagent 调用异常: {e}")

    # --- 检查是否有代码改动 ---
    code, diff_out, _ = run_cmd("git diff --stat", cwd=str(ROOT_DIR))
    code, untracked_out, _ = run_cmd("git status --porcelain", cwd=str(ROOT_DIR))
    has_changes = bool(diff_out.strip() or untracked_out.strip())

    if not has_changes:
        log("  codeagent 未产生代码改动，跳过编译和提交")
        _improve_advance(state)
        return True

    log(f"  检测到代码改动:\n{diff_out.strip()}\n{untracked_out.strip()}")

    # --- cargo build 验证 ---
    log("  验证编译 ...")
    code, build_out, build_err = run_cmd("cargo build --release", cwd=str(ROOT_DIR))
    if code != 0:
        log(f"  编译失败，放弃本次改动")
        log(f"  build stderr: {build_err[:500]}")
        # 回退改动
        run_cmd("git checkout -- .", cwd=str(ROOT_DIR))
        log("  已回退改动")
        _improve_advance(state)
        return True
    log("  编译通过")

    # --- cargo test 验证 ---
    log("  运行测试 ...")
    code, test_out, test_err = run_cmd(
        "cargo test --release -p coregeek --lib --tests 2>&1",
        cwd=str(ROOT_DIR),
    )
    # 检查是否有 FAILED（排除 workspace_root 的 Cargo.lock 格式问题）
    test_failed = "FAILED" in test_out and "workspace_lockfile" not in test_out
    if test_failed:
        log(f"  测试失败，放弃本次改动")
        log(f"  test output: {test_out[-500:]}")
        run_cmd("git checkout -- .", cwd=str(ROOT_DIR))
        log("  已回退改动")
        _improve_advance(state)
        return True
    log("  测试通过")

    # --- git commit + push ---
    log("  提交改动 ...")
    run_cmd("git add -A", cwd=str(ROOT_DIR))
    commit_msg = f"improve: auto-optimize based on {len(loss_battles)} lost battles (pk{loss_battles[0]['pk_id']}"
    if len(loss_battles) > 1:
        commit_msg += f" etc"
    commit_msg += ")"
    code, _, commit_err = run_cmd(
        f'git commit -m "{commit_msg}"',
        cwd=str(ROOT_DIR),
    )
    if code != 0:
        log(f"  git commit 失败: {commit_err[:300]}")
        _improve_advance(state)
        return True
    log("  git commit 成功")

    log("  推送到远端 ...")
    code, _, push_err = run_cmd("git push origin main", cwd=str(ROOT_DIR))
    if code != 0:
        log(f"  git push 失败: {push_err[:300]}")
    else:
        log("  git push 成功")

    # --- 更新 last_commit，让 GOBATTLE 识别为新代码 ---
    code, new_commit, _ = run_cmd("git rev-parse HEAD", cwd=str(ROOT_DIR))
    if code == 0:
        state["last_commit"] = new_commit
        state["last_code_update_time"] = datetime.now().isoformat()
        log(f"  更新 last_commit: {new_commit[:12]}")

    # --- 清理并进入 GOBATTLE ---
    state["issue_pending_pks"] = []
    state["phase"] = "GOBATTLE"
    state["gobattle_failures"] = 0
    state["no_update_count"] = 0
    log("  IMPROVE 阶段完成，进入 GOBATTLE 发起挑战")
    save_state(state)
    return True


def _improve_advance(state):
    """IMPROVE 阶段无改动时的跳转逻辑"""
    state["issue_pending_pks"] = []
    if get_config("issue_submit_enabled", 0) == 1:
        state["phase"] = "ISSUESUBMIT"
        log("  进入 ISSUESUBMIT")
    else:
        state["phase"] = "PULLCODE"
        log("  回到 PULLCODE")
    save_state(state)


def phase_issuesubmit(state):
    """ISSUESUBMIT: 每场失败对战生成一个独立的 GitHub Issue
    - 只分析失败的场次，跳过胜利和对平
    - 每个 issue 包含一场对战的完整数据：批级概况 + 回执 + 关键事件 + 六张证据表 + 缺口
    - issue_submit_enabled=0 时跳过，直接回 PULLCODE
    - 提交后清空 issue_pending_pks → PULLCODE
    """
    # 开关检查
    if get_config("issue_submit_enabled", 0) != 1:
        log(">>> ISSUESUBMIT: issue_submit_enabled=0，跳过 Issue 提交")
        state["issue_pending_pks"] = []
        state["phase"] = "PULLCODE"
        save_state(state)
        return True

    log(">>> ISSUESUBMIT: 为每场失败对战创建独立 issue ...")

    issue_pending_pks = state.get("issue_pending_pks", [])
    if not issue_pending_pks:
        log("  无待提交对战数据，直接进入 GOBATTLE 重新发起挑战")
        state["phase"] = "GOBATTLE"
        save_state(state)
        return True

    now_str = datetime.now().strftime('%Y-%m-%d %H:%M')

    # --- 获取当前排名与累计战绩（所有 issue 共享） ---
    our_rank_str = "?"
    total_record_str = "?"
    win_rate_str = "?"
    rank_data = api_get("/contest/getRankList", {"stage": STAGE, "pageIndex": 1, "pageSize": 64})
    if rank_data and "record" in rank_data:
        records = rank_data["record"]
        for i, r in enumerate(records):
            if r["teamId"] == OUR_TEAM_ID:
                rank_num = i + 1
                win_n = r.get("win", 0) or 0
                lose_n = r.get("lose", 0) or 0
                tie_n = r.get("tie", 0) or 0
                total = win_n + lose_n + tie_n
                our_rank_str = f"{rank_num} / {len(records)}"
                total_record_str = f"{win_n} 胜 {lose_n} 负 {tie_n} 平"
                rate = (win_n / total * 100) if total > 0 else 0
                win_rate_str = f"{rate:.1f}%"
                break

    # --- 收集每场数据，只保留失败的场次 ---
    loss_battles = []  # list of dicts with all fields needed per battle
    skipped = 0

    for pk_id in sorted(issue_pending_pks):
        pk_dir = LOGS_DIR / f"pk{pk_id}"
        meta_file = pk_dir / "meta.json"
        receipt_file = pk_dir / "receipt.json"
        collect_file = pk_dir / "collect_output.txt"
        coach_file = pk_dir / "coach_output.txt"

        meta = {}
        if meta_file.exists():
            try:
                with open(meta_file, "r", encoding="utf-8") as f:
                    meta = json.load(f)
            except:
                pass

        receipt = {}
        if receipt_file.exists():
            try:
                with open(receipt_file, "r", encoding="utf-8") as f:
                    receipt = json.load(f)
            except:
                pass

        # 结果判定
        our_side = meta.get("our_side", "B")
        our_score = meta.get("score_a") if our_side == "A" else meta.get("score_b")
        opp_score = meta.get("score_b") if our_side == "A" else meta.get("score_a")
        opponent_name = meta.get("team_b_name") if our_side == "A" else meta.get("team_a_name")
        battle_time = meta.get("create_time")

        if our_score is not None and opp_score is not None:
            if our_score > opp_score:
                result_cn = "胜"; skipped += 1
            elif our_score < opp_score:
                result_cn = "负"
            else:
                result_cn = "平"; skipped += 1
        else:
            result_cn = "未知"; skipped += 1

        # 只保留失败场次
        if result_cn != "负":
            log(f"  pk{pk_id}: {result_cn} {our_score}:{opp_score} vs {opponent_name} — 跳过")
            continue

        commit_short = receipt.get("base_commit", "unknown")
        commit_time = receipt.get("base_commit_time", "")

        # 读取 collect / coach 输出
        collect_text = ""
        if collect_file.exists():
            try:
                with open(collect_file, "r", encoding="utf-8", errors="replace") as f:
                    collect_text = f.read()
            except:
                pass

        coach_text = ""
        if coach_file.exists():
            try:
                with open(coach_file, "r", encoding="utf-8", errors="replace") as f:
                    coach_text = f.read()
            except:
                pass

        # 获取对手 teamId 用于排名
        opp_team_id = meta.get("team_b_id") if our_side == "A" else meta.get("team_a_id")

        loss_battles.append({
            "pk_id": pk_id,
            "pk_dir": pk_dir,
            "meta": meta,
            "receipt": receipt,
            "result_cn": result_cn,
            "our_score": our_score,
            "opp_score": opp_score,
            "commit_short": commit_short,
            "commit_time": commit_time,
            "opponent_name": opponent_name,
            "opp_team_id": opp_team_id,
            "battle_time": battle_time,
            "collect_text": collect_text,
            "coach_text": coach_text,
        })

    if skipped:
        log(f"  跳过 {skipped} 场非失败场次（胜/平/未知）")

    if not loss_battles:
        log("  本批无失败场次，无需创建 issue，直接进入 GOBATTLE")
        state["issue_pending_pks"] = []
        state["phase"] = "GOBATTLE"
        save_state(state)
        return True

    # --- 按对手排名（胜率+积分）排序，只取前 5 ---
    # 构建对手排名映射：teamId → (rank, score, win_rate)
    opp_rank_map = {}
    if rank_data and "record" in rank_data:
        for i, r in enumerate(rank_data["record"]):
            w = r.get("win", 0) or 0
            l = r.get("lose", 0) or 0
            t = r.get("tie", 0) or 0
            tot = w + l + t
            opp_rank_map[r["teamId"]] = {
                "rank": i + 1,
                "score": r.get("score", 0),
                "win_rate": (w / tot * 100) if tot > 0 else 0,
            }

    def _opp_sort_key(b):
        """按对手排名排序：排名越靠前（rank 越小）越优先"""
        info = opp_rank_map.get(b.get("opp_team_id"))
        if info:
            return (info["rank"], -info["score"], -info["win_rate"])
        # 未知排名的排最后
        return (99999, 0, 0)

    loss_battles.sort(key=_opp_sort_key)

    # 根据时段决定提交上限（夜间多提交）
    MAX_ISSUES_PER_BATCH = get_max_issues()
    night_mode = is_night_for("issue")
    if len(loss_battles) > MAX_ISSUES_PER_BATCH:
        log(f"  本批 {len(loss_battles)} 场失败，按对手排名只取前 {MAX_ISSUES_PER_BATCH} 场提交 issue{'（夜间模式）' if night_mode else ''}")
        log(f"  选中对手（按排名）：")
        for b in loss_battles[:MAX_ISSUES_PER_BATCH]:
            info = opp_rank_map.get(b.get("opp_team_id"), {})
            if info:
                log(f"    pk{b['pk_id']} vs {b['opponent_name']} (排名={info['rank']}, 积分={info['score']}, 胜率={info['win_rate']:.1f}%)")
            else:
                log(f"    pk{b['pk_id']} vs {b['opponent_name']} (排名未知)")
        loss_battles = loss_battles[:MAX_ISSUES_PER_BATCH]
    else:
        log(f"  本批 {len(loss_battles)} 场失败，全部提交 issue")
        for b in loss_battles:
            info = opp_rank_map.get(b.get("opp_team_id"), {})
            if info:
                log(f"    pk{b['pk_id']} vs {b['opponent_name']} (排名={info['rank']}, 积分={info['score']}, 胜率={info['win_rate']:.1f}%)")

    # --- 为每场失败创建独立 issue ---
    created = 0
    for battle in loss_battles:
        pk_id = battle["pk_id"]
        opponent_name = battle["opponent_name"]
        our_score = battle["our_score"]
        opp_score = battle["opp_score"]
        commit_short = battle["commit_short"]
        commit_time = battle["commit_time"]
        battle_time = battle["battle_time"]
        receipt = battle["receipt"]
        collect_text = battle["collect_text"]
        coach_text = battle["coach_text"]
        pk_dir = battle["pk_dir"]

        # --- Issue 正文 ---
        body = "# 对战数据 — 失败分析\n\n"

        # 第一部分：对战概况
        body += "## 第一部分：对战概况\n\n"
        body += "```text\n"
        body += f"生成时间：{now_str}\n"
        ct = f" ({commit_time})" if commit_time else ""
        body += f"对战代码版本：{commit_short}{ct}\n"
        body += f"当前排名：{our_rank_str}\n"
        body += f"累计战绩：{total_record_str}\n"
        body += f"胜率：{win_rate_str}\n"
        bt_str = f"  发起时间={battle_time}" if battle_time else ""
        body += f"本场结果：pk{pk_id} 负 {our_score}:{opp_score} vs {opponent_name}{bt_str}\n"
        if coach_text:
            body += f"教练日志：{coach_text[:500]}\n"
        body += "```\n\n---\n\n"

        # 第二部分：对战回执 + 关键事件
        body += "## 第二部分：对战回执 + 关键事件\n\n"
        body += f"### pk{pk_id}: 负 vs {opponent_name}  比分 {our_score}:{opp_score}\n\n"
        body += f"对战发起时间：{battle_time}\n\n"
        body += "```json\n"
        body += json.dumps(receipt, indent=2, ensure_ascii=False) + "\n"
        body += "```\n\n"

        # 提取关键事件（我方完整摘要 + 对方关键事件）
        event_summary = _extract_log_events(pk_dir, include_opponent=True)
        if event_summary:
            body += "### 日志关键事件摘要（我方优先 + 对方关键事件）\n\n"
            body += event_summary + "\n\n"
        body += "---\n\n"

        # 第三部分：六张证据表
        body += "## 第三部分：六张证据表\n\n"
        body += "```\n"
        body += collect_text if collect_text else "（collect_log.py 未运行或无输出）\n"
        body += "```\n\n---\n\n"

        # 第四部分：缺口
        body += "## 第四部分：缺口\n\n"
        body += "```text\n"
        body += "receipt.tasks_ours[].score：拿不到——对局详情页只有总分，没有逐 session 分。\n"
        body += "receipt.enemy_seen：拿不到——对手基地等级/塔数/墙数在详情页上没有，我方 stdout 也看不见。\n"
        if not collect_text or "运行失败" in collect_text[:50]:
            body += f"collect_log.py：pk{pk_id} 运行失败或无输出\n"
        body += "```\n\n---\n\n"

        # 第五部分：本轮已改
        body += "## 第五部分：本轮已改（不必再报）\n\n"
        body += "见 docs/WORKFLOW_REQUEST.md §7.5。\n\n"

        # --- 创建 issue ---
        today = datetime.now().strftime('%Y-%m-%d')
        title = f"pk{pk_id} 负 {our_score}:{opp_score} vs {opponent_name} {today}"
        labels = ["改进任务", "对战数据", "改进优先"]

        log(f"  创建 issue: {title}")
        issue_num = create_github_issue(title, body, labels=labels)
        if issue_num:
            log(f"  pk{pk_id} → Issue #{issue_num} 创建成功")
            created += 1
        else:
            log(f"  pk{pk_id} → Issue 创建失败")

        # 控制 API 频率
        time.sleep(get_config("sleep_interval"))

    log(f"  本批 {len(loss_battles)} 场失败，成功创建 {created} 个 issue")

    # 清理并回到 PULLCODE
    state["issue_pending_pks"] = []
    state["phase"] = "PULLCODE"
    state["issue_submitted"] = True
    log("  ISSUESUBMIT 阶段完成，回到 PULLCODE")
    save_state(state)
    return True


def _build_coach_summary(coach_outputs, wins, losses, draws):
    """从各场 coach_output.txt 汇总教练信息"""
    total_moves = 0
    switch_counts = {}
    moved_matches = 0
    for pk_id, text in coach_outputs:
        if not text:
            continue
        # 解析 coach moves 行
        moved_this = 0
        for line in text.split("\n"):
            line = line.strip()
            if "本场汇总" in line and "移动" in line:
                # 尝试提取移动次数
                import re as _re
                m = _re.search(r"移动\s*(\d+)\s*次", line)
                if m:
                    n = int(m.group(1))
                    total_moves += n
                    moved_this = n
            if "switch" in line and "why" in line:
                # 统计开关类型
                import re as _re
                for sw in _re.findall(r'"switch":"(\w+)"', line):
                    switch_counts[sw] = switch_counts.get(sw, 0) + 1
        if moved_this > 0:
            moved_matches += 1

    parts = [f"本批 {len(coach_outputs)} 场", f"移动 {total_moves} 次"]
    if switch_counts:
        sw_detail = " / ".join(f"{k} {v}" for k, v in sorted(switch_counts.items(), key=lambda x: -x[1]))
        parts.append(f"({sw_detail})")
    parts.append(f"至少移动过一次的场次 {moved_matches}/{len(coach_outputs)}")
    parts.append(f"结果分布 胜 {wins} / 负 {losses} / 平 {draws}")
    return " | ".join(parts)


_KEY_EVENT_TYPES = {
    'task_cmd_failed', 'task_answer_submit', 'task_answer_found',
    'task_accept', 'task_ended', 'task_answer_sentinel', 'task_answer_schema',
    'task_point_retired', 'task_accept_deferred', 'task_started',
    'coach_move', 'coach_ready', 'coach_night', 'coach_half',
    'cmd_sent', 'cmd_result', 'llm_resp', 'prompt_sent',
    'wall_gate_open', 'wall_gate_seal', 'tower_unpaired',
    'buy', 'sell', 'door_open', 'door_reseal',
    'startup', 'mine_pick', 'mine_outage',
    'wall_build', 'wall_gate_build', 'wall_seal_build',
    'tower_plan', 'build_info', 'treasure_plan',
    'news_legend', 'news_official', 'night_debug',
    'shopping', 'round', 'pair_recomputed',
    'sop_reuse', 'sop_evicted',
}

_KEY_SHOW_ORDER = [
    'startup', 'round',
    'task_accept', 'task_accept_deferred', 'task_started', 'task_ended',
    'task_answer_found', 'task_answer_sentinel', 'task_answer_schema',
    'task_answer_submit', 'task_cmd_failed', 'task_point_retired',
    'coach_ready', 'coach_move', 'coach_night', 'coach_half',
    'cmd_sent', 'cmd_result', 'llm_resp', 'prompt_sent',
    'wall_build', 'wall_gate_build', 'wall_seal_build',
    'wall_gate_open', 'wall_gate_seal', 'tower_unpaired', 'tower_plan',
    'buy', 'sell', 'door_open', 'door_reseal',
    'mine_pick', 'mine_outage', 'build_info', 'treasure_plan',
    'news_legend', 'news_official',
    'shopping', 'night_debug', 'pair_recomputed',
    'sop_reuse', 'sop_evicted',
]


def _extract_log_summary(log_file, label, max_samples=3, max_events_table=30):
    """从单个日志文件提取关键事件摘要（事件计数表 + 关键事件采样）"""
    if not log_file or not log_file.exists():
        return None

    events = {}
    total_lines = 0
    samples = {}
    try:
        with open(str(log_file), 'r', encoding='utf-8', errors='replace') as f:
            for line in f:
                total_lines += 1
                try:
                    obj = json.loads(line.strip())
                    t = obj.get('type', obj.get('event', 'unknown'))
                    events[t] = events.get(t, 0) + 1
                    if t in _KEY_EVENT_TYPES:
                        if t not in samples:
                            samples[t] = []
                        if len(samples[t]) < max_samples:
                            samples[t].append(json.dumps(obj, ensure_ascii=False))
                except:
                    pass
    except:
        return None

    lines = []
    fname = log_file.name
    fsize = log_file.stat().st_size
    lines.append(f"**{label}日志:** `{fname}` ({fsize/1024:.0f}KB, {total_lines} 行)")
    lines.append("")
    lines.append(f"**{label}事件计数:**")
    lines.append("| 事件 | 次数 | 事件 | 次数 |")
    lines.append("|---|---|---|---|")
    sorted_events = sorted(events.items(), key=lambda x: -x[1])
    for i in range(0, min(len(sorted_events), max_events_table), 2):
        k1, v1 = sorted_events[i]
        if i + 1 < len(sorted_events):
            k2, v2 = sorted_events[i + 1]
            lines.append(f"| {k1} | {v1} | {k2} | {v2} |")
        else:
            lines.append(f"| {k1} | {v1} | | |")
    lines.append("")

    for t in _KEY_SHOW_ORDER:
        if t in samples:
            lines.append(f"**{label}{t}** ({events.get(t, 0)} 次):")
            for s in samples[t]:
                lines.append(f"  - `{s}`")

    return "\n".join(lines)


def _extract_log_events(pk_dir, include_opponent=True):
    """从 pk 目录提取我方+对方日志的关键事件摘要
    - 优先输出我方日志摘要（完整事件计数表 + 关键事件采样）
    - 若 include_opponent=True，追加对方日志的关键事件摘要
    """
    import os as _os

    our_log = None
    opp_log = None
    try:
        for fn in _os.listdir(str(pk_dir)):
            if not fn.endswith('.log'):
                continue
            if '4388' in fn:
                our_log = pk_dir / fn
            else:
                opp_log = pk_dir / fn
    except:
        pass

    if not our_log:
        return None

    # 我方日志：完整摘要
    parts = [_extract_log_summary(our_log, "我方")]

    # 对方日志：关键事件摘要（采样数限制为2以控制体积）
    if include_opponent and opp_log:
        opp_summary = _extract_log_summary(opp_log, "对方", max_samples=2, max_events_table=20)
        if opp_summary:
            parts.append(opp_summary)

    return "\n\n".join(p for p in parts if p)

# ==================== 主循环 ====================

def main():
    log("=" * 60)
    log(f"Runner 启动 (pid={os.getpid()})")

    # 每次运行先保持 token 活跃
    keep_alive()

    state = load_state()
    phase = state.get("phase", "PULLCODE")
    log(f"当前阶段: {phase}")

    # 执行当前 phase
    phases = {
        "PULLCODE": phase_pullcode,
        "GOBATTLE": phase_gobattle,
        "WAIT_BATTLE": phase_wait_battle,
        "ANALYZE": phase_analyze,
        "IMPROVE": phase_improve,
        "ISSUESUBMIT": phase_issuesubmit,
    }

    handler = phases.get(phase)
    if not handler:
        log(f"未知阶段: {phase}，重置为 PULLCODE")
        state["phase"] = "PULLCODE"
        save_state(state)
        handler = phase_pullcode

    try:
        success = handler(state)
        if success:
            # 所有阶段都自己管理 phase（PULLCODE/GOBATTLE/WAIT_BATTLE/ANALYZE/ISSUESUBMIT）
            # main() 不自动推进，只记录当前 phase
            current_phase = state.get("phase", phase)
            if current_phase == phase:
                log(f"阶段完成，phase 保持 {phase}")
            else:
                log(f"阶段完成，下一阶段: {current_phase}")
        else:
            log(f"阶段执行失败")
        save_state(state)
    except Exception as e:
        log(f"阶段执行异常: {e}")
        import traceback
        traceback.print_exc()
        save_state(state)

    log("Runner 结束")

if __name__ == "__main__":
    main()
