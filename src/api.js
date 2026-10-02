// 与 Rust 后端通信的唯一入口。
//
// 在浏览器里直接打开时（没有 Tauri 运行时）自动切换到假数据，
// 这样界面可以脱离桌面壳单独预览与调整。

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

// Tauri 框架层的报错是英文（如 invalid args / invalid type），
// 统一翻译成中文并保留原文，避免用户看到无从下手的提示。
function zhError(error) {
  const msg = String(error);
  if (/invalid args .* for command/i.test(msg)) {
    return new Error(`内部请求参数异常（程序缺陷，请反馈此场景）：${msg}`);
  }
  if (/invalid type/i.test(msg) && /expected/i.test(msg)) {
    return new Error(`参数类型不匹配（程序缺陷，请反馈此场景）：${msg}`);
  }
  return error;
}

async function invokeZh(command, args) {
  try {
    return await invoke(command, args);
  } catch (error) {
    throw zhError(error);
  }
}

export const hasTauri =
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const TASK_EVENT = "task://update";

export async function appStatus() {
  if (!hasTauri) return mock.status();
  return invokeZh("app_status");
}

export async function appSettings() {
  if (!hasTauri) return mock.settings();
  return invokeZh("app_settings");
}

export async function updateSettings(settings) {
  if (!hasTauri) return mock.updateSettings(settings);
  return invokeZh("update_settings", { settings });
}

// 解析来源：单条微博链接 → post；用户主页/UID → user；收藏 → favorite
export async function probeSource(input) {
  if (!hasTauri) return mock.probe(input);
  return invokeZh("probe_source", { input });
}

// 继续解析：用户主页按 since_id 往后多拉 want 条。
export async function probeMore(input, want) {
  if (!hasTauri) return mock.probeMore(input, want);
  return invokeZh("probe_more", { input, want });
}

export async function startDownload(req) {
  if (!hasTauri) return mock.start(req);
  return invokeZh("start_download", { req });
}

export async function cancelDownload(taskId) {
  if (!hasTauri) return mock.cancel(taskId);
  return invokeZh("cancel_download", { taskId });
}

export async function loginQrcode() {
  if (!hasTauri) return mock.qrcode();
  return invokeZh("login_qrcode");
}

export async function loginPoll(qrcodeKey) {
  if (!hasTauri) return mock.poll();
  return invokeZh("login_poll", { qrcodeKey });
}

export async function logout() {
  if (!hasTauri) return emptyLogin();
  return invokeZh("logout");
}

export async function chooseOutputDir() {
  if (!hasTauri) return mock.status().output_dir;
  return invokeZh("choose_output_dir");
}

// 「魔法变量」清单由后端提供，界面不再自己写一份——否则界面会列出后端不支持的变量。
export async function namingVariables() {
  if (!hasTauri) return mock.namingVariables();
  return invokeZh("naming_variables");
}

// 批量文件名预览：给每条内容算出文件名，与真实落盘共用同一个渲染器。
export async function previewNames(items, ext) {
  if (!hasTauri) return items.map((item) => `${item.title}.${ext || "txt"}`);
  return invokeZh("preview_names", { items, ext });
}

// 文件名预览走后端同一个渲染器，预览与真实落盘不会不一致。
export async function previewNaming(template, { date, publish_date, ext, dir } = {}) {
  if (!hasTauri) return mock.previewNaming(template, ext);
  return invokeZh("preview_naming", { template, date, publish_date, ext, dir });
}

/// 启动续传：把 `.wbotmp` 里没下完的任务重新入队（设置里打开了才真的做）
export async function resumePending() {
  if (!hasTauri) return mock.resumePending();
  return invokeZh("resume_pending");
}

/// 内容库：读账号里的收藏（我的收藏）与关注（我关注的人）
export async function libraryFolders() {
  if (!hasTauri) return mock.libraryFolders();
  return invokeZh("library_folders");
}

/// 内容库详情：我的收藏按页拉微博
export async function libraryFavPage(page) {
  if (!hasTauri) return mock.libraryFavPage(page);
  return invokeZh("library_fav_page", { page });
}

export async function cleanupTemp() {
  if (!hasTauri) return 0;
  return invokeZh("cleanup_temp");
}

// 更新检测：后端查 GitHub Releases 最新 tag 并与当前版本比较
export async function checkUpdates() {
  if (!hasTauri) return { current: "0.1.0", latest: "", up_to_date: true, error: "" };
  return invokeZh("check_updates");
}

export async function exportDiagnostics() {
  if (!hasTauri) return "";
  return invokeZh("export_diagnostics");
}

/** 网页登录：打开内嵌微博登录页子窗口 */
export async function webLoginOpen() {
  if (!hasTauri) throw new Error("浏览器预览不支持网页登录");
  return invokeZh("web_login_open");
}

/** 网页登录：收割登录 Cookie，返回 LoginInfo（未登录时抛错） */
export async function webLoginCookies() {
  if (!hasTauri) throw new Error("未登录");
  return invokeZh("web_login_cookies");
}

/** 网页登录：关闭内嵌窗口 */
export async function webLoginClose() {
  if (!hasTauri) return;
  return invokeZh("web_login_close").catch(() => {});
}

export async function openPath(path) {
  if (!hasTauri) return;
  return invokeZh("open_path", { path });
}

// 打开文件所在文件夹并选中该文件（资源管理器定位）
export async function revealPath(path) {
  if (!hasTauri) return;
  return invokeZh("reveal_path", { path });
}

export async function onTaskUpdate(handler) {
  if (!hasTauri) return mock.onUpdate(handler);
  return listen(TASK_EVENT, (event) => handler(event.payload));
}

export async function readClipboard() {
  if (!hasTauri) return "";
  try {
    return await navigator.clipboard.readText();
  } catch {
    return "";
  }
}

// ---- 窗口控制（自定义标题栏用）----

async function windowApi() {
  if (!hasTauri) return null;
  const { getCurrentWindow } = await import("@tauri-apps/api/window");
  return getCurrentWindow();
}

export async function minimizeWindow() {
  const win = await windowApi();
  await win?.minimize();
}

export async function toggleMaximizeWindow() {
  const win = await windowApi();
  await win?.toggleMaximize();
}

export async function closeWindow() {
  const win = await windowApi();
  await win?.close();
}

export async function setWindowBackground(color) {
  if (!hasTauri) return;
  try {
    const { getCurrentWindow } = await import("@tauri-apps/api/window");
    await getCurrentWindow().setBackgroundColor(color);
  } catch {
    // 旧运行时不支持时忽略；页面不透明，底色仅影响边缘
  }
}

export async function startWindowDrag() {
  const win = await windowApi();
  await win?.startDragging();
}

function emptyLogin() {
  return { logged_in: false, uname: "", face: "", mid: 0, vip: false, vip_label: "" };
}

// 仅浏览器预览用的假数据（微博口径）
const mock = (() => {
  const listeners = new Set();
  const timers = new Map();

  const status = () => ({
    version: "0.1.0",
    login: {
      logged_in: true,
      uname: "微博用户",
      face: "https://tvax4.sinaimg.cn/crop.0.0.512.512.512/0064zzPFly8gqwq3z2xloj60e80e8glm.jpg",
      mid: 7380874257,
      vip: true,
      vip_label: "微博会员",
    },
    output_dir: "D:\\Zcode\\_data\\webodown\\downloads",
    cookies_path: "D:\\Zcode\\_data\\webodown\\cookies.json",
  });

  const settings = () => ({
    settings: {
      output_dir: "D:\\Zcode\\_data\\webodown\\downloads",
      max_concurrent_tasks: 2,
      chunk_concurrency: 4,
      chunk_mb: 4,
      keep_temp: false,
      naming_template: "{index} {title}.{ext}",
      naming_presets: [
        { name: "示例：带博主", template: "{author} - {title}.{ext}" },
      ],
      folder_template: "{author}/{year}-{month}",
      folder_presets: [
        { name: "示例：只按博主", template: "{author}" },
      ],
      rename_conflict: "skip",
      image_format: "large",
      video_quality: "auto",
      audio_quality: "best",
      download_text: true,
      retry_count: 3,
      speed_limit_mib: 0,
      resume_on_start: false,
      parse_preset: "标准",
      parse_batch: 8,
      parse_batch_wait_ms: 1000,
      parse_rest_every: 100,
      parse_rest_ms: 3000,
      log_level: "info",
      data_dir: "",
      update_check: false,
      onboarded: true,
      proxy: "",
      theme: "system",
    },
    cookies_path: "D:\\Zcode\\_data\\webodown\\cookies.json",
    cookies_saved: true,
    version: "0.1.0",
  });

  const updateSettings = async (next) => {
    const base = settings();
    return { ...base, settings: { ...base.settings, ...next } };
  };

  // 视频清晰度档位：微博移动端播放列表里常见的是 超清/高清/标清
  const qualities = [
    { value: "auto", label: "最佳可用", available: true, hint: "" },
    { value: "hd", label: "高清 720P+", available: true, hint: "" },
    { value: "sd", label: "标清", available: true, hint: "" },
  ];

  function makePost(bid, title, author, created, pics, video) {
    return {
      bid,
      mid: 5000000000000000 + Number(bid.slice(-3), 36),
      title,
      author,
      created_at: created,
      pics,
      has_video: video,
      duration: video ? 62 : 0,
      is_retweet: false,
    };
  }

  const ALL_POSTS = Array.from({ length: 37 }, (_, i) =>
    makePost(
      `Nb${String(i).padStart(3, "0")}xKq${i}`,
      i % 3 === 0 ? `今日份图集分享 第${i}期` : i % 3 === 1 ? `日常记录 ${i}` : `转发抽奖结果 ${i}`,
      "示例博主",
      1759300000 - i * 86400,
      i % 3 === 2 ? 0 : (i % 4) + 1,
      i % 5 === 0
    )
  );

  const probe = async (input) => {
    const lower = (input || "").toLowerCase();
    if (lower.includes("/status") || lower.includes("m.weibo.cn") || /weibo\.com\/\d+\/\w+/i.test(lower)) {
      return {
        kind: "post",
        bid: "NbXxKq1aB",
        mid: 5012345678901234,
        title: "示例单条微博（含 3 图 1 视频）",
        author: "示例博主",
        uid: 7380874257,
        cover: "",
        note: "",
        total: 1,
        loaded: 1,
        exhausted: true,
        qualities,
        items: [makePost("NbXxKq1aB", "示例单条微博（含 3 图 1 视频）", "示例博主", 1759300000, 3, true)],
      };
    }
    // 用户主页 / 裸 UID
    return {
      kind: "user",
      bid: "",
      mid: 0,
      title: "示例博主",
      author: "示例博主",
      uid: 7380874257,
      cover: "",
      note: "",
      total: ALL_POSTS.length,
      loaded: 10,
      exhausted: false,
      qualities,
      items: ALL_POSTS.slice(0, 10),
    };
  };

  function emit(task) {
    listeners.forEach((fn) => fn({ ...task }));
  }

  let counter = 0;

  const start = async (req) => {
    const id = `mock-${++counter}`;
    const imageCount = req.image_count ?? 3;
    const videoCount = req.video_count ?? 1;
    const task = {
      id,
      title: req.title,
      quality_label: req.quality_label || (videoCount ? "视频 · 高清" : `${imageCount} 图`),
      status: "queued",
      image_pct: 0,
      video_pct: 0,
      image_count: imageCount,
      video_count: videoCount,
      downloaded: 0,
      total: 24.6 * 1024 * 1024,
      speed_bps: 0,
      output_path: "",
      message: "排队中",
    };
    const imageTotal = 9.2 * 1024 * 1024;
    const videoTotal = 15.4 * 1024 * 1024;
    emit(task);

    const state = { image: 0, video: 0, phase: 0 };
    const timer = setInterval(() => {
      if (state.phase === 0) {
        state.image += imageTotal * 0.18;
        if (state.image >= imageTotal) {
          state.image = imageTotal;
          state.phase = 1;
        }
        task.image_pct = imageCount ? (state.image / imageTotal) * 100 : 100;
        task.status = "downloading";
        task.message = "下载图片";
        task.speed_bps = 3.1 * 1024 * 1024;
      } else if (state.phase === 1) {
        state.video += videoTotal * 0.16;
        if (state.video >= videoTotal) {
          state.video = videoTotal;
          state.phase = 2;
        }
        task.video_pct = videoCount ? (state.video / videoTotal) * 100 : 100;
        task.message = "下载视频";
      } else {
        task.image_pct = 100;
        task.video_pct = 100;
        task.status = "saving";
        task.message = "保存文案";
        task.speed_bps = 0;
        task.output_path = `${status().output_dir}\\示例博主\\2025-10\\${req.title}.txt`;
        clearInterval(timer);
        timers.delete(id);
        emit({ ...task });
        setTimeout(() => {
          task.status = "done";
          task.message = "已完成";
          emit({ ...task });
        }, 700);
        return;
      }
      task.downloaded = state.image + state.video;
      emit({ ...task });
    }, 260);
    timers.set(id, timer);
    return id;
  };

  const cancel = async (taskId) => {
    const timer = timers.get(taskId);
    if (timer) clearInterval(timer);
    timers.delete(taskId);
  };

  const qrcode = async () => ({
    url: "https://login.sina.com.cn/sso/qrcode/image?entry=weibo&size=180",
    qrcode_key: "preview",
  });

  let polls = 0;
  const poll = async () => {
    polls += 1;
    if (polls < 3) return { state: "pending", login: emptyLogin() };
    if (polls === 3) return { state: "scanned", login: emptyLogin() };
    return { state: "confirmed", login: status().login };
  };

  // 浏览器预览用：从已解析的假清单里继续往后取
  const moreState = new Map();
  const probeMore = async (input, want) => {
    const all = ALL_POSTS;
    const from = moreState.get(input) ?? 10;
    const to = Math.min(from + want, all.length);
    moreState.set(input, to);
    return {
      items: all.slice(from, to),
      loaded: to,
      total: all.length,
      exhausted: to >= all.length,
      note: "",
    };
  };

  const onUpdate = async (handler) => {
    listeners.add(handler);
    return () => listeners.delete(handler);
  };

  // 仅浏览器预览用的兜底：真值在 Rust 的 naming::VARIABLES，
  // 桌面端一律走 naming_variables 命令，这份副本只影响脱离桌面壳的预览。
  const VARIABLES = [
    ["title", "文案摘要", "标题与作者", "微博正文前 30 字"],
    ["author", "博主名", "标题与作者", "发微博的人"],
    ["bid", "BID", "微博标识", "微博短链标识（如 NbXxKq1aB）"],
    ["mid", "MID", "微博标识", "微博数字 ID"],
    ["uid", "博主 UID", "微博标识", "博主的数字 ID"],
    ["publish_date", "发布日期", "时间", "微博发布那天"],
    ["date", "下载日期", "时间", "任务创建那天"],
    ["year", "发布年", "时间", "如 2025"],
    ["month", "发布月", "时间", "如 05（补零）"],
    ["source_kind", "来源类型", "来源与格式", "单条微博 / 用户主页 / 我的收藏"],
    ["index", "序号", "来源与格式", "批次内由旧到新；单条链接为空"],
    ["ext", "扩展名", "来源与格式", "图片/视频/文案各自的后缀"],
  ];

  const SAMPLE = {
    title: "示例微博正文",
    author: "示例博主",
    bid: "NbXxKq1aB",
    mid: 5012345678901234,
    uid: 7380874257,
    date: "2026-10-01",
    publish_date: "2025-10-01",
    year: "2025",
    month: "10",
    source_kind: "用户主页",
    index: 7,
  };

  const resumePending = async () => 0;

  const libraryFolders = async () => ({
    mid: 7380874257,
    created: ALL_POSTS.slice(0, 6).map((post, i) => ({
      id: i + 1,
      bid: post.bid,
      title: post.title,
      created_at: post.created_at,
      pics: post.pics,
      kind: "fav",
    })),
    subscribed: Array.from({ length: 4 }, (_, i) => ({
      id: 60000 + i,
      title: `关注的博主 ${i + 1}`,
      media_count: 120 + i * 33,
      owner: `关注的博主 ${i + 1}`,
      owner_mid: 10000 + i,
      kind: "follow",
    })),
  });

  const libraryFavPage = async (page) => ({
    page,
    has_next: page < 3,
    items: ALL_POSTS.slice((page - 1) * 10, page * 10).map((post, i) => ({
      id: (page - 1) * 10 + i + 1,
      bid: post.bid,
      title: post.title,
      created_at: post.created_at,
      pics: post.pics,
    })),
  });

  const namingVariables = () =>
    VARIABLES.map(([token, label, section, hint]) => ({ token, label, section, hint }));

  // opts: { ext, sample }——sample 提供变量样例值（标题/博主/序号等）
  const previewNaming = (template, opts = {}) => {
    const ext = typeof opts === "string" ? opts : opts.ext || "jpg";
    const sample = (typeof opts === "object" && opts.sample) || {};
    const merged = { ...SAMPLE, ...sample };
    const segments = [];
    let usedExt = false;
    for (const raw of String(template ?? "").split(/[/\\]/)) {
      let out = "";
      let rest = raw;
      while (true) {
        const start = rest.indexOf("{");
        if (start === -1) {
          out += rest;
          break;
        }
        out += rest.slice(0, start);
        const end = rest.indexOf("}", start);
        if (end === -1) {
          out += rest.slice(start);
          break;
        }
        const token = rest.slice(start + 1, end);
        if (token === "ext") {
          usedExt = true;
          out += ext;
        } else if (token in merged) {
          out += merged[token] === "" ? "" : String(merged[token]);
        } else {
          out += `{${token}}`;
        }
        rest = rest.slice(end + 1);
      }
      const cleaned = out
        .replace(/[\\/:*?"<>|]/g, "_")
        .trim()
        .replace(/\.+$/, "")
        .trim();
      if (cleaned && cleaned !== "." && cleaned !== "..") segments.push(cleaned);
    }
    if (!usedExt) {
      if (segments.length) segments[segments.length - 1] += `.${ext}`;
      else segments.push(`weibo.${ext}`);
    }
    return segments.join("/");
  };

  return {
    status,
    settings,
    updateSettings,
    probe,
    probeMore,
    start,
    cancel,
    qrcode,
    poll,
    onUpdate,
    resumePending,
    libraryFolders,
    libraryFavPage,
    namingVariables,
    previewNaming,
  };
})();
