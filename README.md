# WeBodown

微博图片 / 视频 / 音频 / 文案下载器。界面与交互对齐 BILIdown，微博橙主题。

![平台](https://img.shields.io/badge/platform-Windows%2010%2B-blue) ![版本](https://img.shields.io/badge/version-0.2.0-orange)

## 功能

### 下载

- **视频**：最高 1080P（PC 域接口档位合并，移动端缺失档位自动增强）
- **图片**：原图规格，多图按 `-1 / -2 / -3` 编号
- **音频**：声音帖子双档音质（高品质 / 标准），播客页（tv/show）直链下载
- **文案**：独立 `.txt`，含正文、作者、发布时间与原链

### 解析

支持直接粘贴，每行一个来源：

| 形态 | 示例 |
| --- | --- |
| 单条微博 | `https://weibo.com/1234567/NbXxKq1aB` · `https://m.weibo.cn/status/NbXxKq1aB` |
| 用户主页 / UID | `https://weibo.com/u/1234567` · `1234567` |
| 昵称链接 | `https://weibo.com/n/博主昵称`（自动 302 解析） |
| 视频落地页 | `https://weibo.com/tv/show/...`（播客音频直链） |
| 短链 | `t.cn/...`（自动跟随重定向） |

多行混合粘贴、去重、分页继续解析（主页按时间线翻页，序号由旧到新、最旧为 1）。

### 登录

- 扫码登录 / 网页登录（内嵌微博官方页，应用不接触密码）
- 登录后可下载自己的收藏、关注列表（内容库一键解析），并解锁更高视频档位
- 凭据只存本机（`%APPDATA%\webodown\cookies.json`）

### 传输管理

- 三段进度（图片 / 视频 / 文案）、速度与体积实时显示
- 「打开」用系统默认应用打开文件本体；「打开位置」在资源管理器中定位文件
- 重名处理：跳过 / 覆盖 / 自动追加序号

### 设置

- 命名模板与文件夹层级模板（魔法变量面板点击插入，内置预设 + 自定义预设）
- 媒体：图片规格、视频清晰度、音频音质、文案开关
- 代理、限速、日志级别、数据目录、应用更新检测
- 首启引导页：安装后第一次打开即引导配置保存目录与媒体选项

## 安装

从 [Releases](https://github.com/yungumax/WeBodown/releases) 下载 `WeBodown_x.y.z_x64-setup.exe`，
双击安装（Windows 10+，需系统已安装 WebView2 运行时，Win10/11 默认自带）。

## 从源码构建

前置：Node.js 22+、Rust stable（MSVC）、Windows 10+。

```bat
git clone https://github.com/yungumax/WeBodown.git
cd WeBodown
npm install
dev.bat
```

`dev.bat` 会加载便携链接环境（`scripts/msvc-env.sh` / 内嵌 `.lldbin`，详见脚本注释）
并启动开发模式；`dev.bat build` 直接出 NSIS 安装包。

> 本仓库的便携链接环境方案（无 VS Build Tools 时的 Rust MSVC 链接）见
> `scripts/msvc-env.sh` 与 `.lldbin` 的组成说明，可移植到其它 Tauri 项目。

## 技术栈

Tauri 2 · Rust · Vue 3 · Vite。界面一套设计令牌（浅 / 深 / 跟随系统），
Rust 侧核心库 `weibo-core` 负责接口、登录与下载引擎。

## 免责声明

仅供个人学习与离线观看使用，请遵守微博用户协议，不要用于传播或商业用途。
内容版权归原作者所有。
