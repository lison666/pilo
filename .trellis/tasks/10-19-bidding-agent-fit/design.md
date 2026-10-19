# Design: Pilo 原生 /new 表单（贴合 bidding-agent）

## 架构边界

Pilo 不重新实现 new 的语义——表单只是 `bidding-agent new` 的壳：
schema/校验/文件写入的权威都在 bidding-agent（防 C9 契约漂移）。
Pilo 侧负责：收集字段 → `command.run` 调用 → project 闭环。

## 数据流

```
BiddingNewDialog (共享组件)
  └─ purpose select + 动态字段（镜像 PURPOSES 字段表，仅字段名）
  └─ submit
      └─ PiloClient.biddingAgentNew(projectId, purpose, fields)
          ├─ Tauri: 新 command `bidding_agent_new`
          │         → server_client.request("command.run", {program:"bidding-agent", args:[...]})
          │           （仿 git.rs:139 既有模式，零新 server 方法）
          └─ Remote(WebUI): remote-client 直连 server "command.run"
      └─ 成功 → addProject(<out dir>) → 打开 project
          ├─ purpose=job     → 弹确认"开始会话？"（烧 token 要人确认）→ pi.start
          └─ 其他 purpose    → 直接 pi.start（ad-hoc 会话）
```

## 关键决策

1. **执行走 `command.run`**：`resolve_program` 在目标环境 PATH 找
   `bidding-agent` ——Local/WSL/SSH 三端兼容；远端没装时 command.run
   报错原样透传。不加 connection 设置项（V1 用 PATH）。
2. **`--out` 不传**：`new` 的 defaultDir 已按 purpose 分流
   （`workspaces/<job_id>` / `ad-hoc/<date>-<slug>`），command.run 以
   project 为 cwd → 产物落在当前打开的 hub project 下。这正是想要的
   语义：hub project = 含 workspaces/ 的根。
3. **字段表镜像**：前端硬编码 PURPOSES 字段表（required/optional），
   只用于渲染表单控件；真校验由 `bidding-agent new` stderr/exit code
   回传——失败时 toast 原文。
4. **入口**：`NewChatLanding` 第三个 action 卡（`onNewChat`/
   `onNewTemporaryChat` 旁）"Bidding task…"，共享组件自动覆盖
   桌面+WebUI；移动端用 dialog 全屏化。
5. **job 不自动跑**：确认对话框是 token 闸门；ad-hoc 廉价直接起。

## PURPOSES 字段表（镜像自 new-command.ts）

| purpose | required | optional |
|---|---|---|
| job | job_id, task_name, entry_url, spider_name, script_file, objective | — |
| troubleshoot | slug, question | job_ref |
| analysis | slug, question | job_ref |
| api-analysis | slug, api_url, question | evidence_goal, job_ref |

## 兼容/回滚

- 纯增量：不动既有 session/chat 路径；fork 面 = 1 Tauri command +
  1 client method + 1 组件 + landing 接线。
- 回滚 = revert commit，无状态迁移。
- i18n：src/i18n 双语；移动端 dialog 全屏 + 触控目标 ≥44px。
EOF_MARKER

## 补充：61 注册表解析流

```
BiddingNewDialog (purpose=job)
  fields: [接口名称*, 入口URL(选填)]
  └─ biddingAgentNew → command.run:
       bidding-agent new --purpose job --from-registry
         --set interface_name=GJ_25839_4_ZhaobGgService
         [--set entry_url=https://...]
       │
       └─ bidding-agent 内部:
            GET /api/spider-tasks?... 按 interface_name 匹配
            → source_snapshot { source_model_link, spider_name, ... }
            → entry_url = 用户值 ?? source_model_link
            POST /api/script-entry/resolve {spider_name}
            → script_entry → script_file
            → 写 workspaces/<job_id>/task.json + 脚本骨架
```

- 61 base URL：`SPIDER_CORE_61_HTTP_BASE_URL` env 覆盖，默认
  `http://192.168.2.61:8080`（与 query-61-http-server skill 同约）。
- 查询只读（GET/POST resolve 均为只读语义）。
- 匹配规则：优先精确等于 `source_web_interface_name`；多个候选 →
  报错列出候选 task_id 让用户收窄（不猜）。
- spider_name 为 null 的 task（未生成脚本）→ script-entry resolve 会
  失败或 spider_name 缺失 → 报错"该接口尚无脚本"，不臆造。

## job purpose 的两形态

- `--from-registry`：2 字段表单（接口名+可选URL）。
- 无 `--from-registry`：原 6 字段完整表单保留（手搓入口不删）。
