# PRD: Pilo 原生 /new 表单（bidding-agent 贴合定制 · 第一轮）

## Goal

在 Pilo 里用表单创建 bidding-agent 会话（job + 三类 ad-hoc），
替代会话内 `/new` 逐项弹窗；创建后自动添加 project、打开、按类型
决定是否直接起会话。Pilo 只做壳——schema 权威留在 `bidding-agent new`。

## Confirmed facts

- server `command.run {project, program, args[], timeoutMs}` 已存在，
  `resolve_program` 走目标环境 PATH；Local/WSL/SSH 三端兼容。
- `src-tauri` 侧已有 `request("command.run", …)` 先例（`runtime/git.rs:139`）。
- `bidding-agent new --purpose <p> --set k=v --out <dir>` 非交互可用；
  字段表 `new-command.ts:21-46`：job 6 required / troubleshoot·analysis
  2+1 / api-analysis 3+2。
- `--out` 省略时 `new` 按 purpose 落 `workspaces/<job_id>` 或
  `ad-hoc/<date>-<slug>`（相对 command cwd = hub project）。
- `projects.ts` 有 `addProject`/`touchProject`；会话入口在
  `NewChatLanding`（onNewChat/onNewTemporaryChat）。
- 约束：双 UI（桌面+WebUI `src/remote`）共享组件，移动端适配必需；
  不复制 Pi/agent 已有能力；fork 面要小。
- `bidding-agent` 经全局 npm link 已在 PATH（本机实测）。

## Requirements

1. `NewChatLanding` 增加 "Bidding task…" 入口 → `BiddingNewDialog`。
2. Dialog：purpose 选择 → 按字段表动态渲染 required/optional 输入；
   前端只做非空校验。
3. 提交 → `biddingAgentNew` client 方法 → `command.run` 执行
   `bidding-agent new --purpose … --set …`（cwd=当前 project）。
4. 成功 → `addProject` + 打开；purpose=job 弹"开始会话"确认
   （不自动烧 token），其余 purpose 直接 `pi.start` 起会话。
5. 失败 → toast 原文展示 `bidding-agent new` 的 stderr。
6. 双端组件共享；i18n 双语；移动端可用。

## Acceptance criteria

- job 路径：填 6 字段提交 → `workspaces/<job_id>/task.json`+script
  落盘 → project 自动添加并打开 → 出现"开始会话"确认。
- api-analysis 路径：提交 → `ad-hoc/<date>-<slug>/brief.json` 落盘
  → project 打开 → 会话自动起来（`[api-analysis] <slug>` 侧栏可见）。
- 缺 required 字段提交 → `bidding-agent new` 报错透传 toast。
- WebUI 同流程可走完；窄屏 dialog 可用。

## Out of scope

- 会话内 `/new` 扩展继续保留，不动。
- 只读运维视图（分组/verdict 徽标/artifacts 面板）——后续迭代。
- `bidding_agent_executable` 设置项——V1 走 PATH。
- `session.watch` 实时刷新——后续迭代。

## Resolved decisions

- 首轮 scope = /new 表单（用户选）。
- 创建后 = 全自动闭环到会话，job 加人工确认闸门（用户选）。
- 执行通道 = 既有 `command.run`，不加 server 方法（架构证据）。
- 表单不写 JSON 本体，schema 权威在 bidding-agent（C9 防漂移）。

## Amendment: 61 注册表补齐（用户新需求）

- 表单 job 路径收敛为 2 字段：**接口名称***（= 61 task 的
  `source_web_interface_name`，如 `GJ_25839_4_ZhaobGgService`）+
  **入口URL**（选填，留空取注册表 `source_snapshot.source_model_link`）。
- 补齐逻辑在 `bidding-agent new` 内部：新增 `--from-registry`（或等价）
  触发对 61 server 的只读查询（`GET /api/spider-tasks` 按名称过滤 +
  `POST /api/script-entry/resolve`），产出完整 task.json 字段：
  job_id/task_name ← 接口名派生；entry_url/spider_name ← source_snapshot；
  script_file ← script-entry resolve；objective ← 任务描述/remark 模板。
- 61 server = `http://192.168.2.61:8080`（env `SPIDER_CORE_61_HTTP_BASE_URL`
  可覆盖）；只读查询，不动远端状态；查询失败→错误原文透传。
- Pilo 表单仍薄壳：只传 interface_name/entry_url 两个 set 值。
- 注意：远程环境（WSL/SSH）可能不通内网 61 ——报错即可，不 fallback。
