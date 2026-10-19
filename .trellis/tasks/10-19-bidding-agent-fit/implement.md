# Implement: Pilo 原生 /new 表单

## 顺序

1. `PiloClient` 接口加 `biddingAgentNew(projectId, purpose, fields)`；
   `tauri-pilo-client.ts` + `remote-client.ts` 双实现。
2. `src-tauri`：新 command（仿 `runtime/git.rs` 的 `request("command.run",
   …)` 模式）——组装 `bidding-agent new --purpose … --set k=v`。
3. `src/components/bidding-new-dialog.tsx`：purpose 选择 + 动态字段
   + required 校验（仅前端提示，权威校验在 stderr 回传）。
4. `NewChatLanding` 加第三 action 卡接线。
5. 创建后闭环：`addProject(outDir)` → 导航打开 → job 弹"开始会话"
   确认 / ad-hoc 直接起会话。
6. i18n（en+zh）+ 移动端布局。
7. 验证：`pnpm format && pnpm check && pnpm build`；
   `cargo fmt --all --check && cargo check`；Windows 实机跑一遍
   job + api-analysis 两条路（用 `pilo-server call` 对照验证产物落盘）。

## 风险点

- 前端找不到 `bidding-agent`（PATH 缺失）→ command.run 报错原文
  toast，不吞错。
- `NewChatLanding` 只在有 project 打开时出现——确认挂载上下文；
  若无 project 态，入口改 project 侧栏"+"。
- project 添加/导航在 remote transport 的等价 API 待确认
  （`projects.ts` 是 invoke 直连桌面侧；remote 侧查 remote-client.ts
  对应实现，若缺则补 PiloClient 方法）。
