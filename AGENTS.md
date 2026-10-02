# WeBodown 项目规则（对任何代理/会话永久生效）

## 发布规则（用户明确要求）

- **禁止自动发布装机版**：不得在未经用户明确确认的情况下创建 GitHub Release、
  打 tag 触发发布工作流、或对外分发任何安装包。
- 构建（cargo build / tauri build）仅在本地产出，属开发行为，不受此限。
- 用户确认发布后再执行：打 tag → 推送 → 等待 CI 产出安装包 → 核对 Release 内容。

## 测试版规则

- **每轮代码修改完成后，默认重新构建并打开调试版（测试版）exe** 供用户验收：
  `cmd //c start "" "D:\Zcode\WeBodown\target\debug\webodown.exe"`
- 构建顺序：先 `npm run build` 再 `cargo build --features custom-protocol`（反了会嵌旧 dist）。
- 构建环境：先 `source scripts/msvc-env.sh`（便携 MSVC 工具链，见 scripts/msvc-env.sh 注释）。

## 其他

- 用户数据/设置在 %APPDATA%\webodown\；调试日志 qrcode_debug.log 同目录。
- 微博接口的域限制与字段坑见 crates/weibo-core 各模块顶部注释（since_id int、
  PC 域才有 1080P、Cookie 按域分桶等）。
