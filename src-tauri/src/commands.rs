//! 命令层：把 weibo-core 的能力暴露给界面。所有命令返回 Result<T, String>，
//! 错误一律转成人话（界面 toast 直接显示）。

use crate::naming;
use crate::state::{AppState, BatchCache, Settings};
use crate::types::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tauri::Emitter;
use tauri::Manager;
use tauri::State;
use tauri_plugin_dialog::DialogExt;
use weibo_core::api::PostData;
use weibo_core::download as dl;
use weibo_core::parser::{parse_input, SourceTarget};
use weibo_core::WeiboClient;

const TASK_EVENT: &str = "task://update";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

fn err<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

type PauseFlag = Arc<std::sync::atomic::AtomicBool>;

fn flag_set(flag: &PauseFlag) -> bool {
    flag.load(std::sync::atomic::Ordering::Relaxed)
}

/// 把头像下载下来转成 data URL 内嵌进 LoginInfo：
/// sinaimg CDN 在 WebView2 里偶发加载失败（curl 直连却 200，实测踩过），
/// 后端用 reqwest（已验证可靠）下载一次，前端就与 CDN 彻底解耦。
async fn inline_avatar(client: &WeiboClient, face_url: &str) -> String {
    use base64::Engine as _;
    if face_url.is_empty() {
        return String::new();
    }
    let resp = client.http.get(face_url).send().await;
    let Ok(resp) = resp else { return face_url.to_string() };
    if !resp.status().is_success() {
        return face_url.to_string();
    }
    let Ok(bytes) = resp.bytes().await else { return face_url.to_string() };
    // 只内嵌合理大小的图（头像不会超过 1 MiB），异常大文件退回原 URL
    if bytes.len() > 1024 * 1024 {
        return face_url.to_string();
    }
    let mime = if bytes.starts_with(&[0x89, b'P']) {
        "image/png"
    } else if bytes.starts_with(b"GIF8") {
        "image/gif"
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        "image/jpeg"
    } else if bytes.len() > 10 && &bytes[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "image/jpeg"
    };
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(&bytes)
    )
}

/// 校验通过的登录态 → LoginInfo（头像已内嵌）。
async fn build_login(client: &WeiboClient, uid: u64, uname: String, face: String) -> LoginInfo {
    LoginInfo {
        logged_in: true,
        uname,
        face: inline_avatar(client, &face).await,
        mid: uid,
        ..Default::default()
    }
}

// ───────────────────────── 应用状态 ─────────────────────────

#[tauri::command]
pub async fn app_status(
    _app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<AppStatus, String> {
    warmup_once(&state).await;
    let settings = state.settings_snapshot();

    // 登录态：有 Cookie 文件就验证一次（10 秒内不给结果就当未登录，不卡启动）。
    // 冷启动要连两跳（api/config + 用户信息），5 秒偶发不够（实测踩过）。
    let mut login = LoginInfo::default();
    if state.cookies_saved() {
        let client = state.client();
        let started = std::time::Instant::now();
        let probe = tokio::time::timeout(Duration::from_secs(10), async move {
            client.account().await
        })
        .await;
        match probe {
            Ok(Ok(Some((uid, uname, face)))) => {
                let client = state.client();
                login = build_login(&client, uid, uname, face).await;
                *state.login.write().unwrap() = Some(login.clone());
                weibo_core::login::debug_log(&format!(
                    "启动恢复登录 uid={uid} 用时 {}ms",
                    started.elapsed().as_millis()
                ));
            }
            Ok(Ok(None)) => {
                weibo_core::login::debug_log("启动恢复登录：api/config 判未登录");
            }
            Ok(Err(e)) => {
                weibo_core::login::debug_log(&format!("启动恢复登录：接口错误 {e}"));
            }
            Err(_) => {
                weibo_core::login::debug_log("启动恢复登录：10 秒超时");
            }
        }
    } else {
        weibo_core::login::debug_log("启动恢复登录：无 cookies.json");
    }

    Ok(AppStatus {
        version: APP_VERSION.to_string(),
        login,
        output_dir: settings.output_dir.to_string_lossy().to_string(),
        cookies_path: settings.cookies_path().to_string_lossy().to_string(),
    })
}

/// 会话预热：启动后访问一次 m.weibo.cn 首页，把访客 Cookie 放进会话。
async fn warmup_once(state: &State<'_, AppState>) {
    if state.warmed.swap(true, Ordering::SeqCst) {
        return;
    }
    let client = state.client();
    let _ = client.warmup().await;
}

#[tauri::command]
pub fn app_settings(state: State<'_, AppState>) -> Result<SettingsEnv, String> {
    let settings = state.settings_snapshot();
    Ok(SettingsEnv {
        cookies_path: settings.cookies_path().to_string_lossy().to_string(),
        cookies_saved: state.cookies_saved(),
        version: APP_VERSION.to_string(),
        settings,
    })
}

#[tauri::command]
pub fn update_settings(
    state: State<'_, AppState>,
    settings: Settings,
) -> Result<SettingsEnv, String> {
    let mut next = settings;
    next.clamp();

    let old = state.settings_snapshot();
    let proxy_changed = old.proxy != next.proxy;
    let slots_changed = old.max_concurrent_tasks != next.max_concurrent_tasks;

    if proxy_changed {
        let client = WeiboClient::with_proxy(Some(&next.proxy)).map_err(err)?;
        // 换会话不丢登录态：把现有 Cookie 搬过去
        let cookies = state.client().export_cookies();
        client.install_cookies(&cookies);
        *state.client.write().unwrap() = Arc::new(client);
    }
    if slots_changed {
        state.rebuild_slots();
    }

    next.save().map_err(err)?;
    *state.settings.lock().unwrap() = next.clone();
    // 数据目录可能变了，Cookie 跟着搬
    state.save_cookies();

    Ok(SettingsEnv {
        cookies_path: next.cookies_path().to_string_lossy().to_string(),
        cookies_saved: true,
        version: APP_VERSION.to_string(),
        settings: next,
    })
}

// ───────────────────────── 来源解析 ─────────────────────────

/// PostData → 表格条目
fn entry_of(post: &PostData) -> PostEntry {
    PostEntry {
        bid: post.bid.clone(),
        mid: post.mid.clone(),
        title: naming::title_of(&post.text),
        author: post.screen_name.clone(),
        created_at: post.created_at,
        pics: post.pics.len() as u32,
        has_video: !post.videos.is_empty(),
        is_audio: post.is_audio,
        media_url: post.audios.first().map(|(_, u)| u.clone()).unwrap_or_default(),
        duration: post.duration,
        is_retweet: post.is_retweet,
    }
}

/// 视频清晰度选项：auto 永远在，hd/sd 按实际有没有给。
fn quality_options(posts: &[PostData]) -> Vec<QualityOption> {
    let has_hd = posts
        .iter()
        .any(|p| p.videos.iter().any(|(label, _)| is_hd(label)));
    let has_sd = posts
        .iter()
        .any(|p| !p.videos.is_empty());
    vec![
        QualityOption { value: "auto".into(), label: "最佳可用".into(), available: true, hint: String::new() },
        QualityOption { value: "hd".into(), label: "高清".into(), available: has_hd, hint: if has_hd { String::new() } else { "该来源没有高清档".into() } },
        QualityOption { value: "sd".into(), label: "标清".into(), available: has_sd, hint: String::new() },
    ]
}

fn is_hd(label: &str) -> bool {
    let l = label.to_lowercase();
    l.contains("720") || l.contains("1080") || l.contains("高清") || l.contains("超清")
}

#[tauri::command]
pub async fn probe_source(
    state: State<'_, AppState>,
    input: String,
) -> Result<Probe, String> {
    warmup_once(&state).await;
    let client = state.client();

    // 归一输入；t.cn 短链与 weibo.com/n/昵称 链接都要先跟随 302 解析成最终形态。
    // 原始输入保留一份用于缓存别名：probe_more 拿到的还是这条原始链接。
    let raw_input = input.clone();
    let input = match parse_input(&input) {
        Ok(target) => target,
        Err(e) if e.to_string().contains("T_CN_SHORTLINK")
            || e.to_string().contains("NEED_RESOLVE:") =>
        {
            let resolved = client.resolve_redirect(input.trim()).await.map_err(err)?;
            parse_input(&resolved).map_err(err)?
        }
        Err(e) => return Err(err(e)),
    };

    // tv/show 音频/播客播放页：不在常规微博体系，直接抓内嵌音频直链
    if let SourceTarget::TvShow(tv_url) = &input {
        let post = client.tvshow_audio(tv_url).await.map_err(err)?;
        let media_url = post.audios.first().map(|(_, u)| u.clone()).unwrap_or_default();
        let entry = PostEntry {
            bid: post.bid.clone(),
            mid: String::new(),
            title: naming::title_of(&post.text),
            author: post.screen_name.clone(),
            created_at: post.created_at,
            pics: 0,
            has_video: false,
            is_audio: true,
            media_url: media_url.clone(),
            duration: post.duration,
            is_retweet: false,
        };
        let probe = Probe {
            kind: "tvshow".into(),
            bid: post.bid.clone(),
            mid: String::new(),
            title: naming::title_of(&post.text),
            author: post.screen_name.clone(),
            uid: 0,
            cover: String::new(),
            note: String::new(),
            total: 1,
            loaded: 1,
            exhausted: true,
            qualities: quality_options(&[]),
            items: vec![entry],
        };
        weibo_core::login::debug_log(&format!(
            "tv/show 播客解析：标题={} 音频流 {} 个",
            probe.title,
            post.audios.len()
        ));
        return Ok(probe);
    }

    match input {
        SourceTarget::TvShow(_) => unreachable!("tv/show 已在上方分支处理"),
        SourceTarget::Post(bid) => {
            let post = retry(2, || client.post_detail(&bid)).await.map_err(err)?;
            let entries = vec![entry_of(&post)];
            Ok(Probe {
                kind: "post".into(),
                bid: post.bid.clone(),
                mid: post.mid.clone(),
                title: naming::title_of(&post.text),
                author: post.screen_name.clone(),
                uid: post.user_id,
                cover: String::new(),
                note: String::new(),
                total: 1,
                loaded: 1,
                exhausted: true,
                qualities: quality_options(&[post]),
                items: entries,
            })
        }
        SourceTarget::User(uid) => {
            let info = client.user_info(uid).await.map_err(err)?;
            let page = retry(2, || client.user_timeline(uid, "")).await.map_err(err)?;
            let entries: Vec<PostEntry> = page.posts.iter().map(entry_of).collect();
            let exhausted = page.since_id.is_empty() || entries.is_empty();
            let note = if entries.is_empty() {
                "这个博主没有可解析的微博（或仅对粉丝可见）".to_string()
            } else {
                String::new()
            };
            // 总数 = 博主主页的微博总数（statuses_count）：时间线接口本身不给总数，
            // 没有它界面只能显示"已加载 10 项"，用户会误以为解析少了（实测踩过）
            let probe = Probe {
                kind: "user".into(),
                bid: String::new(),
                mid: String::new(),
                title: info.screen_name.clone(),
                author: info.screen_name.clone(),
                uid,
                cover: info.avatar.clone(),
                note,
                total: info.statuses_count as usize,
                loaded: entries.len(),
                exhausted,
                qualities: quality_options(&page.posts),
                items: entries,
            };
            weibo_core::login::debug_log(&format!(
                "probe user={uid} 本页 {} 条 since_id={:?} exhausted={}",
                probe.items.len(),
                page.since_id,
                probe.exhausted
            ));
            let cache = BatchCache {
                uid,
                items: probe.items.clone(),
                since_id: page.since_id,
                is_favorite: false,
                exhausted,
            };
            let key = input_key(SourceTarget::User(uid));
            state.cache_batch(key.clone(), cache.clone());
            // 原始链接（/n/昵称、t.cn 等）也挂一份别名：probe_more 直接用它查缓存
            if raw_input.trim() != key {
                state.cache_batch(raw_input.trim().to_string(), cache);
            }
            Ok(probe)
        }
    }
}

fn input_key(target: SourceTarget) -> String {
    match target {
        SourceTarget::Post(bid) => format!("post:{bid}"),
        SourceTarget::User(uid) => format!("user:{uid}"),
        SourceTarget::TvShow(url) => format!("tvshow:{url}"),
    }
}

#[tauri::command]
pub async fn probe_more(
    state: State<'_, AppState>,
    input: String,
    want: usize,
) -> Result<ProbeMore, String> {
    let client = state.client();
    // 输入可能是任意链接形态，归一成缓存键
    let key = match parse_input(&input) {
        Ok(target) => input_key(target),
        // /n/昵称 链接与 t.cn 短链要先重定向解析成 UID/BID，才能命中解析时的缓存键
        Err(e) if e.to_string().contains("NEED_RESOLVE:")
            || e.to_string().contains("T_CN_SHORTLINK") =>
        {
            let resolved = client.resolve_redirect(input.trim()).await.map_err(err)?;
            input_key(parse_input(&resolved).map_err(err)?)
        }
        Err(_) => input.trim().to_string(),
    };

    // 锁内只做快照，网络请求放到锁外（std Mutex 跨 await 会让 future 失去 Send）
    let (mut is_favorite, mut uid, mut since_id, mut exhausted) = {
        let batches = state.batches.lock().unwrap();
        let Some(cache) = batches.get(&key) else {
            return Err("这个来源的解析缓存已过期，请回到输入页重新解析".into());
        };
        (
            cache.is_favorite,
            cache.uid,
            cache.since_id.clone(),
            cache.exhausted,
        )
    };

    if exhausted {
        return Ok(ProbeMore {
            items: Vec::new(),
            loaded: state
                .batches
                .lock()
                .unwrap()
                .get(&key)
                .map(|c| c.items.len())
                .unwrap_or(0),
            total: 0,
            exhausted: true,
            note: String::new(),
        });
    }

    let mut added: Vec<PostEntry> = Vec::new();
    let mut guard = 0;
    while added.len() < want && !exhausted && guard < 20 {
        guard += 1;
        let page = if is_favorite {
            let page_no: u32 = since_id.parse().unwrap_or(1);
            client.favourites_wap_page(uid, page_no).await.map_err(err)?
        } else {
            client.user_timeline(uid, &since_id).await.map_err(err)?
        };

        let entries: Vec<PostEntry> = page.posts.iter().map(entry_of).collect();
        let new_since = page.since_id.clone();

        {
            let mut batches = state.batches.lock().unwrap();
            let Some(cache) = batches.get_mut(&key) else { break };
            for entry in entries {
                if cache.items.iter().any(|item| item.bid == entry.bid) {
                    continue;
                }
                cache.items.push(entry.clone());
                added.push(entry);
            }
            if page.posts.is_empty() && new_since.is_empty() {
                cache.exhausted = true;
            } else if new_since.is_empty() {
                cache.exhausted = true;
            } else {
                cache.since_id = new_since;
            }
            is_favorite = cache.is_favorite;
            uid = cache.uid;
            since_id = cache.since_id.clone();
            exhausted = cache.exhausted;
        }
    }

    let (loaded, exhausted) = {
        let batches = state.batches.lock().unwrap();
        match batches.get(&key) {
            Some(cache) => (cache.items.len(), cache.exhausted),
            None => (0, true),
        }
    };

    Ok(ProbeMore {
        loaded,
        total: 0, // 微博接口不给总数，前端只显示已加载数
        exhausted,
        note: String::new(),
        items: added,
    })
}

/// 简单重试包装：网络抖一下不至于让整个来源解析失败。
async fn retry<T, F, Fut>(times: usize, mut f: F) -> weibo_core::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = weibo_core::Result<T>>,
{
    let mut last = Err(weibo_core::WeiboError::Unavailable("未执行".into()));
    for attempt in 0..=times {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(600)).await;
        }
        last = f().await;
        if last.is_ok() {
            return last;
        }
    }
    last
}

// ───────────────────────── 下载任务 ─────────────────────────

#[tauri::command]
pub async fn start_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    req: DownloadReq,
) -> Result<String, String> {
    let id = state.next_task_id();
    let task = TaskUpdate {
        id: id.clone(),
        title: req.title.clone(),
        quality_label: req.quality_label.clone(),
        status: "queued".into(),
        image_count: req.image_count,
        video_count: req.video_count,
        is_audio: req.is_audio,
        message: "排队中".into(),
        ..Default::default()
    };
    emit_task(&app, &task);

    let pause = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let handle = tokio::spawn(run_task(app.clone(), req.clone(), id.clone()));
    state.tasks.lock().unwrap().insert(
        id.clone(),
        crate::state::TaskEntry {
            snapshot: Arc::new(Mutex::new(task)),
            abort: Some(handle.abort_handle()),
            req,
            pause,
        },
    );
    Ok(id)
}

#[tauri::command]
pub fn cancel_download(app: tauri::AppHandle, state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    let snapshot = {
        let tasks = state.tasks.lock().unwrap();
        tasks.get(&task_id).map(|e| {
            if let Some(abort) = &e.abort {
                abort.abort();
            }
            e.snapshot.clone()
        })
    };
    if let Some(snapshot) = snapshot {
        let mut task = snapshot.lock().unwrap();
        // 必须写回快照本体：孤儿 reporter 每 500ms 会重播快照，
        // 只改克隆的话「已取消」立刻被覆盖回「下载中」
        if !matches!(task.status.as_str(), "done" | "failed" | "canceled") {
            task.status = "canceled".into();
            task.message = "已取消".into();
            task.speed_bps = 0.0;
        }
        emit_task(&app, &task.clone());
    }
    Ok(())
}

fn emit_task(app: &tauri::AppHandle, task: &TaskUpdate) {
    let _ = app.emit(TASK_EVENT, task);
}

/// 一条微博的下载流水线：图片 → 视频 → 文案，进度按阶段汇报。
async fn run_task(app: tauri::AppHandle, req: DownloadReq, id: String) {
    let app_for_direct = app.clone();
    let state = app.state::<AppState>();
    let snapshot = state
        .tasks
        .lock()
        .unwrap()
        .get(&id)
        .map(|e| e.snapshot.clone());
    let Some(snapshot) = snapshot else { return };
    let (pause, entry_req) = {
        let tasks = state.tasks.lock().unwrap();
        match tasks.get(&id) {
            Some(entry) => (entry.pause.clone(), Some(entry.req.clone())),
            None => (Arc::new(std::sync::atomic::AtomicBool::new(false)), None),
        }
    };
    // 创建后立刻被暂停（如「全部暂停」在排队期间触发）：直接进入暂停态
    if flag_set(&pause) {
        let task = {
            let mut task = snapshot.lock().unwrap();
            task.status = "paused".into();
            task.message = "已暂停".into();
            task.clone()
        };
        emit_task(&app, &task);
        return;
    }

    let settings = state.settings_snapshot();
    let client = state.client();
    let _slot = state.slots().acquire_owned().await;

    // done 与各 progress 闭包各自持有独立克隆，避免借用与 move 冲突
    let app_done = app.clone();
    let snapshot_done = snapshot.clone();

    let done = move |status: &str, message: &str, output: &str| {
        let task = {
            // 必须写回快照本体：只改克隆的话，后端账本停在旧状态，
            // 「全部暂停」的守卫会把已完成任务再翻成已暂停（实测踩过）
            let mut t = snapshot_done.lock().unwrap();
            t.status = status.to_string();
            t.message = message.to_string();
            t.speed_bps = 0.0;
            if !output.is_empty() {
                t.output_path = output.to_string();
            }
            t.clone()
        };
        emit_task(&app_done, &task);
    };

    // tv/show 播客等直链任务：解析时已拿到媒体地址，跳过详情直接下载
    if !req.media_url.is_empty() {
        download_direct(app_for_direct, &state, &snapshot, &req, &settings, &client).await;
        return;
    }

    // 取详情（重试几次，拿最新的图片/视频地址）
    let post = match retry(settings.retry_count as usize, || client.post_detail(&req.bid)).await {
        Ok(post) => post,
        Err(e) => {
            done("failed", &err(e), "");
            return;
        }
    };

    // 视频档位增强：1080P/原画只在 PC 域接口给出（移动端 playback_list 常为空）。
    // 有 weibo.com 登录 Cookie 就尝试 PC 详情，把它的视频档位并进来。
    let mut post = post;
    let pc_has_sub = client
        .export_domains()
        .pc
        .iter()
        .any(|(name, _)| name == "SUB");
    if pc_has_sub {
        match client.post_detail_pc(&req.bid).await {
            Ok(pc_post) => {
                let pc_count = pc_post.videos.len();
                for (label, url) in pc_post.videos {
                    if !post.videos.iter().any(|(_, u)| u == &url) {
                        post.videos.push((label, url));
                    }
                }
                // 图片：PC 的 pic_infos 是网页「原图」同款规格，有就用它替换移动端列表
                if !pc_post.pics.is_empty() {
                    post.pics = pc_post.pics.clone();
                }
                weibo_core::login::debug_log(&format!(
                    "PC 增强：并入 {pc_count} 个视频档位，图片 {} 张，视频合计 {} 个档位",
                    post.pics.len(),
                    post.videos.len()
                ));
            }
            Err(e) => {
                weibo_core::login::debug_log(&format!("PC 档位增强失败（不影响下载）：{e}"));
            }
        }
    }

    let title = {
        let t = naming::title_of(&post.text);
        if t.is_empty() { req.title.clone() } else { t }
    };

    // 渲染路径：文件夹层级 + 条目名（不带扩展名的公共前缀）
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("title".into(), title.clone());
    vars.insert("author".into(), if req.author.is_empty() { post.screen_name.clone() } else { req.author.clone() });
    vars.insert("bid".into(), req.bid.clone());
    vars.insert("mid".into(), req.mid.clone());
    vars.insert("uid".into(), req.uid.to_string());
    vars.insert("publish_date".into(), req.naming.publish_date.clone());
    vars.insert("date".into(), req.naming.date.clone());
    vars.insert("year".into(), req.naming.year.clone());
    vars.insert("month".into(), req.naming.month.clone());
    vars.insert("source_kind".into(), req.naming.source_kind.clone());
    vars.insert("index".into(), naming::padded_index(req.naming.index, req.naming.index_pad));
    vars.insert("ext".into(), String::new());

    let folder = naming::render(&settings.folder_template, &vars, true);
    let prefix = naming::render(&settings.naming_template, &vars, false);
    let base_dir = settings.output_dir.join(&folder);
    if let Err(e) = tokio::fs::create_dir_all(&base_dir).await {
        done("failed", &format!("创建目录失败：{e}"), "");
        return;
    }

    // 重名处理
    let conflict = settings.rename_conflict.clone();
    let final_path = |name: &str, ext: &str| -> PathBuf {
        let mut path = base_dir.join(format!("{name}.{ext}"));
        if conflict == "auto" {
            let mut seq = 1;
            while path.exists() {
                path = base_dir.join(format!("{name} ({seq}).{ext}"));
                seq += 1;
            }
        }
        path
    };

    // skip 模式：文案或首个媒体已存在 → 整条跳过（不重复下载）
    if conflict == "skip" {
        let text_probe = base_dir.join(format!("{prefix}.txt"));
        let media_probe = if !post.videos.is_empty() {
            base_dir.join(format!("{prefix}.mp4"))
        } else if post.pics.len() == 1 {
            base_dir.join(format!("{prefix}.jpg"))
        } else if !post.pics.is_empty() {
            base_dir.join(format!("{prefix}-1.jpg"))
        } else {
            text_probe.clone()
        };
        if media_probe.exists() || text_probe.exists() {
            // 找出该条目已落盘的主文件（跳过时 output 指向文件而非目录）
            let existing = std::fs::read_dir(&base_dir)
                .ok()
                .and_then(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.path())
                        .find(|p| {
                            p.file_name()
                                .map(|n| n.to_string_lossy().starts_with(&prefix))
                                .unwrap_or(false)
                        })
                });
            {
                let mut task = snapshot.lock().unwrap();
                task.image_pct = 100.0;
                task.video_pct = 100.0;
                // 回填文件大小：跳过的任务不显示「0 B」
                if let Some(size) = existing
                    .as_ref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .map(|m| m.len())
                    .filter(|s| *s > 0)
                {
                    task.downloaded = size;
                    task.total = size;
                }
            }
            done(
                "done",
                "文件已存在，跳过下载",
                &existing
                    .unwrap_or(base_dir)
                    .to_string_lossy(),
            );
            return;
        }
    }

    // 主文件：完成后「打开」直接打开它（视频 > 音频 > 首图 > 文案）
    let mut primary: Option<PathBuf> = None;

    // 阶段一：图片
    let image_spec = if req.image.is_empty() { settings.image_format.clone() } else { req.image.clone() };
    let pic_total = post.pics.len();
    let opts = dl::DownloadOptions { retries: settings.retry_count as usize };
    let mut downloaded_total: u64 = 0;

    if pic_total > 0 {
        {
            let mut task = snapshot.lock().unwrap();
            task.status = "downloading".into();
            task.message = "下载图片".into();
        }
        for (index, pic) in post.pics.iter().enumerate() {
            let url = dl::image_variant(pic, &image_spec);
            let ext = url_ext(&url);
            let name = if pic_total == 1 { prefix.clone() } else { format!("{prefix}-{}", index + 1) };
            let dest = final_path(&name, &ext);

            let snap = snapshot.clone();
            let app_c = app.clone();
            let base_before = downloaded_total;
            let pic_index = index;
            let pic_total_c = pic_total;
            let progress: dl::ProgressFn = Arc::new(move |p| {
                let mut task = snap.lock().unwrap();
                // 任务已终局（被取消/暂停）：孤儿 reporter 不再把旧进度播回 UI
                if matches!(task.status.as_str(), "done" | "failed" | "canceled" | "paused") {
                    return;
                }
                task.downloaded = base_before + p.downloaded;
                task.total = base_before + p.total;
                task.speed_bps = p.speed_bps;
                task.image_pct = (pic_index as f64 + p.downloaded as f64 / p.total.max(1) as f64)
                    / pic_total_c as f64
                    * 100.0;
                emit_task(&app_c, &task.clone());
            });

            if dest.exists() {
                // 续传：图片已完成，跳过
                let size = std::fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
                downloaded_total += size;
                if primary.is_none() {
                    primary = Some(dest.clone());
                }
                continue;
            }
            if flag_set(&pause) {
                let task = {
                    let mut task = snapshot.lock().unwrap();
                    task.status = "paused".into();
                    task.message = "已暂停".into();
                    task.clone()
                };
                emit_task(&app, &task);
                return;
            }
            // 图片也走断点续传：暂停能在一个分块内停下（普通下载不检查旗标，
            // 会出现「UI 已暂停、底层还在下」），中止也不会留下半个损坏的图片文件
            match dl::download_resumable(&client.http, &url, &dest, &opts, progress, Some(pause.clone())).await
            {
                Ok(dl::DownloadEnd::Completed(size)) => {
                    downloaded_total += size;
                    if primary.is_none() {
                        primary = Some(dest.clone());
                    }
                }
                Ok(dl::DownloadEnd::Paused(size)) => {
                    let task = {
                        let mut task = snapshot.lock().unwrap();
                        task.status = "paused".into();
                        task.message = "已暂停".into();
                        task.downloaded = downloaded_total + size;
                        task.speed_bps = 0.0;
                        task.clone()
                    };
                    emit_task(&app, &task);
                    return;
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&dest).await;
                    done("failed", &format!("图片下载失败：{e}"), "");
                    return;
                }
            }
        }
        {
            let mut task = snapshot.lock().unwrap();
            task.image_pct = 100.0;
            task.speed_bps = 0.0;
            emit_task(&app, &task.clone());
        }
    }

    // 阶段二：音频（声音帖子，单流直下；没有视频档位概念）
    if !post.audios.is_empty() {
        {
            let mut task = snapshot.lock().unwrap();
            task.status = "downloading".into();
            task.message = "下载音频".into();
            task.is_audio = true;
        }
        let want_best = req.audio_quality.is_empty() || req.audio_quality == "best";
        // 先按请求挑，挑不到回退另一档，再不行取第一个
        let (label, url) = post
            .audios
            .iter()
            .find(|(l, _)| if want_best { l == "高品质" } else { l == "标准" })
            .or_else(|| post.audios.first())
            .cloned()
            .unwrap_or_default();
        let ext = url_ext(&url);
        let dest = final_path(&prefix, &ext);

        let snap = snapshot.clone();
        let app_c = app.clone();
        let base_before = downloaded_total;
        let progress: dl::ProgressFn = Arc::new(move |p| {
            let mut task = snap.lock().unwrap();
            // 任务已终局（被取消/暂停）：孤儿 reporter 不再把旧进度播回 UI
            if matches!(task.status.as_str(), "done" | "failed" | "canceled" | "paused") {
                return;
            }
            task.downloaded = base_before + p.downloaded;
            task.total = base_before + p.total;
            task.speed_bps = p.speed_bps;
            task.video_pct = if p.total > 0 { p.downloaded as f64 / p.total as f64 * 100.0 } else { 0.0 };
            emit_task(&app_c, &task.clone());
        });

        match dl::download_resumable(&client.http, &url, &dest, &opts, progress, Some(pause.clone())).await
        {
            Ok(dl::DownloadEnd::Completed(size)) => {
                downloaded_total += size;
                primary = Some(dest.clone());
                let mut task = snapshot.lock().unwrap();
                task.video_pct = 100.0;
                task.speed_bps = 0.0;
                task.quality_label = format!("音频 · {label}");
                emit_task(&app, &task.clone());
            }
            Ok(dl::DownloadEnd::Paused(size)) => {
                let task = {
                    let mut task = snapshot.lock().unwrap();
                    task.status = "paused".into();
                    task.message = "已暂停（点击继续以恢复）".into();
                    task.downloaded = base_before + size;
                    task.speed_bps = 0.0;
                    task.clone()
                };
                emit_task(&app, &task);
                // 暂停后立即返回，流水线停止（.part 保留进度，继续时从断点恢复）
                return;
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&dest).await;
                done("failed", &format!("音频下载失败：{e}"), "");
                return;
            }
        }
    }

    // 阶段二：视频
    if !post.videos.is_empty() {
        {
            let mut task = snapshot.lock().unwrap();
            task.status = "downloading".into();
            task.message = "下载视频".into();
            if task.quality_label.is_empty() {
                task.quality_label = "视频".into();
            }
        }
        let (label, url) = pick_video(&post.videos, &req.quality);
        {
            weibo_core::login::debug_log(&format!(
                "视频挑选：quality={} 选「{label}」，候选 {:?}",
                req.quality,
                post.videos.iter().map(|(l, _)| l).collect::<Vec<_>>()
            ));
        }
        let dest = final_path(&prefix, "mp4");

        let snap = snapshot.clone();
        let app_c = app.clone();
        let base_before = downloaded_total;
        let progress: dl::ProgressFn = Arc::new(move |p| {
            let mut task = snap.lock().unwrap();
            // 任务已终局（被取消/暂停）：孤儿 reporter 不再把旧进度播回 UI
            if matches!(task.status.as_str(), "done" | "failed" | "canceled" | "paused") {
                return;
            }
            task.downloaded = base_before + p.downloaded;
            task.total = base_before + p.total;
            task.speed_bps = p.speed_bps;
            task.video_pct = if p.total > 0 { p.downloaded as f64 / p.total as f64 * 100.0 } else { 0.0 };
            emit_task(&app_c, &task.clone());
        });

        match dl::download_resumable(&client.http, &url, &dest, &opts, progress, Some(pause.clone())).await
        {
            Ok(dl::DownloadEnd::Completed(size)) => {
                downloaded_total += size;
                primary = Some(dest.clone());
                let mut task = snapshot.lock().unwrap();
                task.video_pct = 100.0;
                task.speed_bps = 0.0;
                if !label.is_empty() {
                    task.quality_label = format!("视频 · {label}");
                }
                emit_task(&app, &task.clone());
            }
            Ok(dl::DownloadEnd::Paused(size)) => {
                let task = {
                    let mut task = snapshot.lock().unwrap();
                    task.status = "paused".into();
                    task.message = "已暂停（点击继续以恢复）".into();
                    task.downloaded = base_before + size;
                    task.speed_bps = 0.0;
                    task.clone()
                };
                emit_task(&app, &task);
                // 暂停后立即返回，流水线停止（.part 保留进度，继续时从断点恢复）
                return;
            }
            Err(e) => {
                let _ = tokio::fs::remove_file(&dest).await;
                done("failed", &format!("视频下载失败：{e}"), "");
                return;
            }
        }
    }

    // 阶段三：文案
    if settings.download_text {
        {
            let mut task = snapshot.lock().unwrap();
            task.status = "saving".into();
            task.message = "保存文案".into();
            task.video_pct = 100.0;
            emit_task(&app, &task.clone());
        }
        let link = if !req.bid.is_empty() {
            format!("https://m.weibo.cn/status/{}", req.bid)
        } else {
            String::new()
        };
        let date_text = if post.created_at > 0 {
            let (y, m, d) = civil_from_unix(post.created_at + 8 * 3600);
            format!("{y}-{m:02}-{d:02}")
        } else {
            String::new()
        };
        let body = format!(
            "{}\n\n——\n作者：{}\n发布时间：{}\n链接：{}\n下载于：WeBodown v{}\n",
            post.text, post.screen_name, date_text, link, APP_VERSION
        );
        let dest = final_path(&prefix, "txt");
        if let Err(e) = tokio::fs::write(&dest, body).await {
            done("failed", &format!("文案写入失败：{e}"), "");
            return;
        }
    }

    {
        let mut task = snapshot.lock().unwrap();
        task.downloaded = task.total.max(downloaded_total);
    }
    if primary.is_none() && settings.download_text {
        // 纯文字微博：主文件就是文案
        primary = Some(base_dir.join(format!("{prefix}.txt")));
    }
    weibo_core::login::debug_log(&format!(
        "任务完成：bid={} 主文件={}",
        req.bid,
        primary.as_ref().map(|p| p.display().to_string()).unwrap_or_default()
    ));
    done(
        "done",
        "已完成",
        &primary.unwrap_or(base_dir).to_string_lossy(),
    );
}

/// 清晰度档位排名（越大越高）：4K/原画 > 1080 > 720 > 480 > 其余。
fn quality_rank(label: &str) -> u32 {
    let l = label.to_lowercase();
    if l.contains("4k") || l.contains("原画") {
        5
    } else if l.contains("1080") {
        4
    } else if l.contains("720") || l.contains("高清") || l.contains("超清") {
        3
    } else if l.contains("480") || l.contains("标清") {
        2
    } else {
        1
    }
}

/// 按清晰度挑播放地址：auto 取最高档，hd 优先 ≥720 的最高档，sd 取最低档。
fn pick_video(videos: &[(String, String)], quality: &str) -> (String, String) {
    if videos.is_empty() {
        return Default::default();
    }
    // 档位按清晰度从高到低排（同档保持原顺序）
    let mut sorted: Vec<&(String, String)> = videos.iter().collect();
    sorted.sort_by(|a, b| quality_rank(&b.0).cmp(&quality_rank(&a.0)));

    match quality {
        "hd" => sorted
            .iter()
            .find(|(label, _)| quality_rank(label) >= 3)
            .or_else(|| sorted.first())
            .map(|&(l, u)| (l.clone(), u.clone()))
            .unwrap_or_default(),
        "sd" => sorted
            .iter()
            .rev()
            .find(|(label, _)| quality_rank(label) <= 2)
            .or_else(|| sorted.last())
            .map(|&(l, u)| (l.clone(), u.clone()))
            .unwrap_or_default(),
        // auto：最高档（合并了 PC 域档位后通常就是 1080P/原画）
        _ => sorted
            .first()
            .map(|&(l, u)| (l.clone(), u.clone()))
            .unwrap_or_default(),
    }
}

fn url_ext(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path.rsplit_once('.') {
        Some((_, ext)) if ext.len() <= 5 => ext.to_ascii_lowercase(),
        _ => "jpg".to_string(),
    }
}

/// Unix 秒 → （年, 月, 日）（东八区口径由调用方加好偏移）。
fn civil_from_unix(secs: i64) -> (i64, u32, u32) {
    let days = secs.div_euclid(86400);
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ───────────────────────── 扫码登录 ─────────────────────────

#[tauri::command]
pub async fn login_qrcode(state: State<'_, AppState>) -> Result<QrCode, String> {
    warmup_once(&state).await;
    let client = state.client();
    let session = client.qr_create().await.map_err(err)?;
    Ok(QrCode {
        url: session.image_url,
        qrcode_key: session.qrid,
    })
}

#[tauri::command]
pub async fn login_poll(
    state: State<'_, AppState>,
    qrcode_key: String,
) -> Result<PollResult, String> {
    let settings = state.settings_snapshot();
    let client = state.client();
    let (qr_state, alt) = client.qr_poll(&qrcode_key).await.map_err(err)?;
    let state_text = match qr_state {
        weibo_core::login::QrState::Pending => "pending",
        weibo_core::login::QrState::Scanned => "scanned",
        weibo_core::login::QrState::Expired => "expired",
        weibo_core::login::QrState::Confirmed => "confirmed",
    };

    let mut login = LoginInfo::default();
    if qr_state == weibo_core::login::QrState::Confirmed {
        // 登录链用 jar 客户端：Set-Cookie 自动入罐、跳转自动带前一步 Cookie
        let chain = WeiboClient::with_cookie_jar(Some(&settings.proxy)).map_err(err)?;
        if let Some(alt) = alt.filter(|s| !s.is_empty()) {
            chain.crossdomain_login(&alt).await.map_err(err)?;
        }
        // 链上的 Cookie 本来就按域落在 jar 里：按域导出 → 按域装回会话客户端
        let domains = chain.export_domains();
        weibo_core::login::debug_log(&format!(
            "登录链收集：m 域 {} 个 / pc 域 {} 个 Cookie",
            domains.m.len(),
            domains.pc.len()
        ));
        let session = WeiboClient::with_proxy(Some(&settings.proxy)).map_err(err)?;
        session.install_domains(&domains);
        login = match session.account().await {
            Ok(Some((uid, uname, face))) => {
                // 校验通过才把新会话换上去，之后所有请求都带登录态
                let login = build_login(&session, uid, uname, face).await;
                *state.client.write().unwrap() = Arc::new(session);
                state.save_cookies();
                *state.login.write().unwrap() = Some(login.clone());
                weibo_core::login::debug_log(&format!("登录校验通过 uid={uid}"));
                login
            }
            Ok(None) => return Err("扫码成功但登录态校验未通过，请重试".into()),
            Err(e) => return Err(format!("登录态校验失败：{e}")),
        }
    }

    Ok(PollResult { state: state_text.into(), login })
}

#[tauri::command]
pub async fn logout(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<LoginInfo, String> {
    // 只在确有登录态时才清 Cookie 文件：误触发会把扫码/网页登录的成果毁掉
    let was_logged_in = state.login.read().unwrap().is_some();
    let settings = state.settings_snapshot();
    if was_logged_in {
        let _ = std::fs::remove_file(settings.cookies_path());
        weibo_core::login::debug_log("logout：已清除登录态");
    }
    // 关掉登录小窗（async 命令里窗口操作可靠；同步命令里会静默失效——实测踩过）。
    // 残留数据无需清：下次开窗本来就用全新临时数据目录。
    if let Some(window) = app.get_webview_window("web-login") {
        let _ = window.close();
    }
    let client = WeiboClient::with_proxy(Some(&settings.proxy)).map_err(err)?;
    *state.client.write().unwrap() = Arc::new(client);
    *state.login.write().unwrap() = None;
    Ok(LoginInfo::default())
}

// ───────────────────────── 网页登录（内嵌官方页） ─────────────────────────

#[tauri::command]
pub async fn web_login_open(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::{WebviewUrl, WebviewWindowBuilder};
    if let Some(window) = app.get_webview_window("web-login") {
        let _ = window.set_focus();
        return Ok(());
    }
    // 每次开窗用全新临时数据目录：天然无历史 Cookie/缓存残留，
    // 退出登录后重新打开也绝不会"自动登录回去"（clear_all_browsing_data
    // 在同步命令里会静默失效，实测踩过）。
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let data_dir = std::env::temp_dir().join(format!("webodown-weblogin-{stamp}"));
    WebviewWindowBuilder::new(
        &app,
        "web-login",
        WebviewUrl::External("https://passport.weibo.com/sso/signin".parse().unwrap()),
    )
    .title("微博登录")
    .inner_size(460.0, 620.0)
    .resizable(true)
    .center()
    .data_directory(data_dir)
    .build()
    .map_err(err)?;
    Ok(())
}

#[tauri::command]
pub async fn web_login_cookies(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<LoginInfo, String> {
    let settings = state.settings_snapshot();

    // 从 webview 引擎层按域取 Cookie（HttpOnly 也能拿到）。
    // m.weibo.cn 与 weibo.com 是两套不同值的登录凭据，必须分桶收集、分桶安装。
    let Some(window) = app.get_webview_window("web-login") else {
        return Err("登录窗口已关闭，请重新打开网页登录".into());
    };
    let mut m_cookies: Vec<(String, String)> = Vec::new();
    let mut pc_cookies: Vec<(String, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let domains: [(&str, bool); 3] = [
        ("https://m.weibo.cn/", true),
        ("https://weibo.com/", false),
        ("https://passport.weibo.com/", false),
    ];
    for (origin, is_m) in domains {
        let Ok(url) = tauri::Url::parse(origin) else { continue };
        let cookies = match window.cookies_for_url(url) {
            Ok(list) => list,
            Err(e) => {
                weibo_core::login::debug_log(&format!("cookies_for_url({origin}) 失败：{e}"));
                continue;
            }
        };
        let names: Vec<String> = cookies.iter().map(|c| c.name().to_string()).collect();
        weibo_core::login::debug_log(&format!("webview {origin} → {names:?}"));
        for c in cookies {
            let pair = (c.name().to_string(), c.value().to_string());
            if seen.insert(format!("{origin}{}", pair.0)) {
                if is_m {
                    m_cookies.push(pair);
                } else {
                    pc_cookies.push(pair);
                }
            }
        }
    }

    // 全新会话：避免旧访客 SUB 与登录 SUB 同域共存
    let session = WeiboClient::with_proxy(Some(&settings.proxy)).map_err(err)?;
    session.install_cookies_for("https://m.weibo.cn/", &m_cookies);
    session.install_cookies_for("https://weibo.com/", &pc_cookies);

    let has_sub = session
        .export_cookies()
        .iter()
        .any(|(name, _)| name == "SUB");
    if !has_sub {
        weibo_core::login::debug_log("web_login_cookies：未发现 SUB");
        return Err("还没有检测到登录 Cookie（SUB）——请确认已在小窗里完成登录，再点一次「完成绑定」".into());
    }
    weibo_core::login::debug_log(&format!(
        "web_login_cookies：m 域 {} 个 / pc 域 {} 个 Cookie（均含各自 SUB），开始校验",
        m_cookies.len(),
        pc_cookies.len()
    ));
    match session.account().await {
        Ok(Some((uid, uname, face))) => {
            // 校验通过才换上新会话并落盘，然后关掉登录小窗
            let login = build_login(&session, uid, uname, face).await;
            *state.client.write().unwrap() = Arc::new(session);
            state.save_cookies();
            weibo_core::login::debug_log(&format!("web_login_cookies：校验通过 uid={uid}，已落盘"));
            *state.login.write().unwrap() = Some(login.clone());
            if let Some(window) = app.get_webview_window("web-login") {
                let _ = window.close();
            }
            Ok(login)
        }
        Ok(None) => {
            weibo_core::login::debug_log("web_login_cookies：account() 判未登录（Cookie 无效）");
            Err("登录态校验未通过（api/config 判未登录），稍后再试".into())
        }
        Err(e) => {
            weibo_core::login::debug_log(&format!("web_login_cookies：account() 错误 {e}"));
            Err(format!("登录态校验失败：{e}"))
        }
    }
}

#[tauri::command]
pub async fn web_login_close(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("web-login") {
        let _ = window.close();
    }
    Ok(())
}

// ───────────────────────── 内容库 ─────────────────────────

#[tauri::command]
pub async fn library_folders(state: State<'_, AppState>) -> Result<LibraryFolders, String> {
    warmup_once(&state).await;
    let client = state.client();

    let login = state.login.read().unwrap().clone().unwrap_or_default();
    let mid = if login.mid > 0 {
        login.mid
    } else {
        client
            .account()
            .await
            .map_err(err)?
            .map(|(uid, _, _)| uid)
            .ok_or_else(|| "收藏与关注需要先登录".to_string())?
    };

    // 收藏分页拉全量（wap 收藏页每页约 10 条，page 链接由解析器识别；20 页上限防失控）
    let mut fav_posts = Vec::new();
    let mut fav_page_no: u32 = 1;
    loop {
        let page = client.favourites_wap_page(mid, fav_page_no).await.map_err(err)?;
        let next = page.since_id.clone();
        let got = page.posts.len();
        fav_posts.extend(page.posts);
        if next.is_empty() || got == 0 || fav_page_no >= 20 {
            break;
        }
        fav_page_no = next.parse().unwrap_or(fav_page_no + 1);
    }
    weibo_core::login::debug_log(&format!(
        "收藏拉取：共 {} 条（{} 页）",
        fav_posts.len(),
        fav_page_no
    ));
    let created: Vec<FavItem> = fav_posts
        .iter()
        .enumerate()
        .map(|(index, post)| FavItem {
            id: index as u64 + 1,
            bid: post.bid.clone(),
            title: naming::title_of(&post.text),
            created_at: post.created_at,
            pics: post.pics.len() as u32,
            kind: "fav".into(),
        })
        .collect();
    // 收藏的平台限制说明：wap 接口只开放最近若干条（实测上限 ~39），无法翻到更早
    let fav_note = format!("微博接口仅开放最近 {} 条收藏，更早的只有官方客户端可见", created.len());

    let (follows, follow_total) = client.following_all(mid).await.map_err(err)?;
    let follow_note = if follow_total > follows.len() {
        format!(
            "微博接口仅开放最近 {} 人（账号共关注 {} 人，更早的只有官方客户端可见）",
            follows.len(),
            follow_total
        )
    } else {
        String::new()
    };
    let subscribed: Vec<FollowItem> = follows
        .iter()
        .map(|user| FollowItem {
            id: user.uid,
            title: user.screen_name.clone(),
            media_count: user.statuses_count,
            owner: user.screen_name.clone(),
            owner_mid: user.uid,
            kind: "follow".into(),
        })
        .collect();

    Ok(LibraryFolders {
        mid,
        created,
        subscribed,
        fav_note,
        follow_note,
    })
}

/// 直链任务流水线（tv/show 播客等）：解析时已拿到媒体地址，
/// 直接渲染命名 → 下载 → （可选）写文案，不查微博详情。
async fn download_direct(
    app: tauri::AppHandle,
    state: &State<'_, AppState>,
    snapshot: &Arc<Mutex<TaskUpdate>>,
    req: &DownloadReq,
    settings: &Settings,
    client: &WeiboClient,
) {
    // 调用方 run_task 已持有并发槽，这里不再重复 acquire（双占会在 max=1 时死锁）
    // 暂停旗标：按快照里的任务 id 从任务表取
    let pause = {
        let task_id = snapshot.lock().unwrap().id.clone();
        state
            .tasks
            .lock()
            .unwrap()
            .get(&task_id)
            .map(|e| e.pause.clone())
            .unwrap_or_else(|| Arc::new(std::sync::atomic::AtomicBool::new(false)))
    };
    let app_done = app.clone();
    let snapshot_done = snapshot.clone();

    let done = move |status: &str, message: &str, output: &str| {
        let task = {
            // 必须写回快照本体：只改克隆的话，后端账本停在旧状态，
            // 「全部暂停」的守卫会把已完成任务再翻成已暂停（实测踩过）
            let mut t = snapshot_done.lock().unwrap();
            t.status = status.to_string();
            t.message = message.to_string();
            t.speed_bps = 0.0;
            if !output.is_empty() {
                t.output_path = output.to_string();
            }
            t.clone()
        };
        emit_task(&app_done, &task);
    };

    let title = {
        let t = naming::title_of(&req.title);
        if t.is_empty() { req.title.clone() } else { t }
    };
    let mut vars: HashMap<String, String> = HashMap::new();
    vars.insert("title".into(), title.clone());
    vars.insert("author".into(), req.author.clone());
    vars.insert("bid".into(), req.bid.clone());
    vars.insert("mid".into(), req.mid.clone());
    vars.insert("uid".into(), req.uid.to_string());
    vars.insert("publish_date".into(), req.naming.publish_date.clone());
    vars.insert("date".into(), req.naming.date.clone());
    vars.insert("year".into(), req.naming.year.clone());
    vars.insert("month".into(), req.naming.month.clone());
    vars.insert("source_kind".into(), req.naming.source_kind.clone());
    vars.insert("index".into(), naming::padded_index(req.naming.index, req.naming.index_pad));
    vars.insert("ext".into(), String::new());

    let folder = naming::render(&settings.folder_template, &vars, true);
    let prefix = naming::render(&settings.naming_template, &vars, false);
    let base_dir = settings.output_dir.join(&folder);
    if let Err(e) = tokio::fs::create_dir_all(&base_dir).await {
        done("failed", &format!("创建目录失败：{e}"), "");
        return;
    }

    let ext = {
        let raw = req.media_url.split(['?', '#']).next().unwrap_or("");
        match raw.rsplit_once('.') {
            Some((_, e)) if e.len() <= 5 => e.to_ascii_lowercase(),
            _ => "mp3".into(),
        }
    };
    // 重名处理：skip 时已存在则跳过
    let mut dest = base_dir.join(format!("{prefix}.{ext}"));
    if settings.rename_conflict == "auto" {
        let mut seq = 1;
        while dest.exists() {
            dest = base_dir.join(format!("{prefix} ({seq}).{ext}"));
            seq += 1;
        }
    } else if settings.rename_conflict == "skip" && dest.exists() {
        let task = {
            let mut task = snapshot.lock().unwrap();
            task.image_pct = 100.0;
            task.video_pct = 100.0;
            task.clone()
        };
        emit_task(&app, &task);
        done("done", "文件已存在，跳过下载", &dest.to_string_lossy());
        return;
    }

    {
        let task = {
            let mut task = snapshot.lock().unwrap();
            task.status = "downloading".into();
            task.message = "下载音频".into();
            task.is_audio = true;
            task.clone()
        };
        emit_task(&app, &task);
    }

    let opts = dl::DownloadOptions { retries: settings.retry_count as usize };
    let snap = snapshot.clone();
    let app_c = app.clone();
    let progress: dl::ProgressFn = Arc::new(move |p| {
        let mut task = snap.lock().unwrap();
        // 任务已终局（被取消/暂停）：孤儿 reporter 不再把旧进度播回 UI
        if matches!(task.status.as_str(), "done" | "failed" | "canceled" | "paused") {
            return;
        }
        task.downloaded = p.downloaded;
        task.total = p.total;
        task.speed_bps = p.speed_bps;
        task.video_pct = if p.total > 0 { p.downloaded as f64 / p.total as f64 * 100.0 } else { 0.0 };
        emit_task(&app_c, &task.clone());
    });

    // 直链的签名（ssig）绑定抓取时的桌面 UA：必须用桌面 UA + weibo.com Referer
    // 下载，会话客户端的 iPhone UA 会被 CDN 判 403（实测踩过）
    let mut builder = reqwest::Client::builder()
        .user_agent(weibo_core::client::PC_UA)
        .default_headers({
            let mut h = reqwest::header::HeaderMap::new();
            h.insert(
                reqwest::header::REFERER,
                reqwest::header::HeaderValue::from_static("https://weibo.com/"),
            );
            h
        })
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(300));
    // 用户设置了代理就要带上：直链下载同样走代理（此前绕过，限速/内网环境会失效）
    let proxy_url = settings.proxy.trim().to_string();
    if !proxy_url.is_empty() {
        match reqwest::Proxy::all(&proxy_url) {
            Ok(p) => builder = builder.proxy(p),
            Err(e) => {
                weibo_core::login::debug_log(&format!("直链代理解析失败，按直连处理：{e}"));
            }
        }
    }
    let pc_http = builder.build();
    let pc_http = match pc_http {
        Ok(c) => c,
        Err(e) => {
            weibo_core::login::debug_log(&format!("PC 下载客户端构建失败，回退会话客户端：{e}"));
            client.http.clone()
        }
    };

    // 直链也走断点续传：暂停能在一个分块内停下，中止不损坏目标文件
    match dl::download_resumable(&pc_http, &req.media_url, &dest, &opts, progress, Some(pause.clone())).await {
        Ok(dl::DownloadEnd::Completed(_)) => {}
        Ok(dl::DownloadEnd::Paused(size)) => {
            let task = {
                let mut task = snapshot.lock().unwrap();
                task.status = "paused".into();
                task.message = "已暂停".into();
                task.downloaded = size;
                task.speed_bps = 0.0;
                task.clone()
            };
            emit_task(&app, &task);
            return;
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&dest).await;
            done("failed", &format!("音频下载失败：{e}"), "");
            return;
        }
    }

    let size = tokio::fs::metadata(&dest).await.map(|m| m.len()).unwrap_or(0);
    {
        let task = {
            let mut task = snapshot.lock().unwrap();
            task.video_pct = 100.0;
            task.image_pct = 100.0;
            task.downloaded = size;
            task.total = size;
            task.clone()
        };
        emit_task(&app, &task);
    }

    // 文案
    if settings.download_text {
        {
            let mut task = snapshot.lock().unwrap();
            task.status = "saving".into();
            task.message = "保存文案".into();
            let task = task.clone();
            emit_task(&app, &task);
        }
        let link = if req.page_url.is_empty() {
            req.media_url.clone()
        } else {
            req.page_url.clone()
        };
        let body = format!(
            "{}

——
作者：{}
链接：{}
下载于：WeBodown v{}
",
            req.title, req.author, link, APP_VERSION
        );
        let dest = base_dir.join(format!("{prefix}.txt"));
        if let Err(e) = tokio::fs::write(&dest, body).await {
            done("failed", &format!("文案写入失败：{e}"), "");
            return;
        }
    }

    done("done", "已完成", &dest.to_string_lossy());
}

/// 资源管理器定位：打开文件所在文件夹并选中该文件（目录则直接打开目录）。
#[tauri::command]
pub fn reveal_path(path: String) -> Result<(), String> {
    // 规范化并剥掉 canonicalize 加的 \\?+\\ 前缀：
    // 不剥的话资源管理器解析不了（回退打开“此电脑”，实测踩过）
    let mut target = std::fs::canonicalize(&path)
        .unwrap_or_else(|_| std::path::PathBuf::from(&path));
    let s = target.to_string_lossy().to_string();
    if let Some(rest) = s.strip_prefix(r"\\?\\UNC\\") {
        target = std::path::PathBuf::from(format!(r"\\{rest}"));
    } else if let Some(rest) = s.strip_prefix(r"\\?\\") {
        target = std::path::PathBuf::from(rest.to_string());
    }
    weibo_core::login::debug_log(&format!(
        "reveal_path：{} → {}",
        path,
        target.display()
    ));
    if !target.exists() {
        return Err(format!("路径不存在：{}", target.display()));
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        if target.is_dir() {
            std::process::Command::new("explorer.exe")
                .arg(&target)
                .creation_flags(0x08000000)
                .spawn()
                .map_err(err)?;
        } else {
            std::process::Command::new("explorer.exe")
                .args(["/select,", &target.to_string_lossy()])
                .creation_flags(0x08000000)
                .spawn()
                .map_err(err)?;
        }
        return Ok(());
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("xdg-open")
            .arg(target.parent().unwrap_or(&target))
            .spawn()
            .map_err(err)?;
        Ok(())
    }
}

// ───────────────────────── 暂停 / 继续 ─────────────────────────

fn set_paused(app: &tauri::AppHandle, state: &State<'_, AppState>, task_id: &str) -> Result<(), String> {
    let tasks = state.tasks.lock().unwrap();
    let Some(entry) = tasks.get(task_id) else {
        return Err("任务不存在或已结束".into());
    };
    entry.pause.store(true, std::sync::atomic::Ordering::Relaxed);
    let mut task = entry.snapshot.lock().unwrap();
    if matches!(task.status.as_str(), "queued" | "downloading" | "saving") {
        task.status = "paused".into();
        task.message = "已暂停".into();
        task.speed_bps = 0.0;
        // 排队中的任务没有下载循环替我们发事件，这里立即广播暂停态
        emit_task(app, &task.clone());
    }
    Ok(())
}

#[tauri::command]
pub fn pause_download(app: tauri::AppHandle, state: State<'_, AppState>, task_id: String) -> Result<(), String> {
    set_paused(&app, &state, &task_id)
}

#[tauri::command]
pub fn pause_all_downloads(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<usize, String> {
    let tasks = state.tasks.lock().unwrap();
    let mut count = 0;
    for entry in tasks.values() {
        let mut task = entry.snapshot.lock().unwrap();
        if matches!(task.status.as_str(), "queued" | "downloading" | "saving") {
            entry.pause.store(true, std::sync::atomic::Ordering::Relaxed);
            task.status = "paused".into();
            task.message = "已暂停".into();
            task.speed_bps = 0.0;
            emit_task(&app, &task.clone());
            count += 1;
        }
    }
    Ok(count)
}

/// 从暂停态恢复：用保存的请求重跑流水线（视频/音频走 .part 断点续传）。
/// 必须是 async：同步命令跑在主线程，tokio::spawn 在主线程会 panic（应用闪退）。
#[tauri::command]
pub async fn resume_download(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    task_id: String,
) -> Result<(), String> {
    let (req, snapshot) = {
        let tasks = state.tasks.lock().unwrap();
        let Some(entry) = tasks.get(&task_id) else {
            return Err("任务不存在或已结束".into());
        };
        if entry.snapshot.lock().unwrap().status != "paused" {
            return Err("任务不在暂停状态".into());
        }
        (entry.req.clone(), entry.snapshot.clone())
    };
    {
        let mut task = snapshot.lock().unwrap();
        task.status = "queued".into();
        task.message = "排队中".into();
        // 立即广播：恢复的任务可能要等并发槽，不发事件 UI 会一直停在「已暂停」
        emit_task(&app, &task.clone());
    }
    // 先换新旗标再 spawn：重跑的 run_task 起始就读旗标，
    // 旧旗标还停在 true 时会立即再次暂停（表现为「点了继续没反应」）
    let fresh_pause = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let mut tasks = state.tasks.lock().unwrap();
        let Some(entry) = tasks.get_mut(&task_id) else {
            return Err("任务不存在或已结束".into());
        };
        entry.pause = fresh_pause;
        // 先中止旧的 run_task 再 spawn：旗标只让下载循环在分块边界退出，
        // 旧任务的收尾窗口里还占着并发槽，不中止会让新任务一直停在排队中
        if let Some(old) = entry.abort.take() {
            old.abort();
        }
    }
    let handle = tokio::spawn(run_task(app, req, task_id.clone()));
    if let Some(entry) = state.tasks.lock().unwrap().get_mut(&task_id) {
        entry.abort = Some(handle.abort_handle());
    }
    Ok(())
}

#[tauri::command]
pub async fn resume_all_downloads(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<usize, String> {
    let paused: Vec<String> = {
        let tasks = state.tasks.lock().unwrap();
        tasks
            .iter()
            .filter(|(_, e)| e.snapshot.lock().unwrap().status == "paused")
            .map(|(id, _)| id.clone())
            .collect()
    };
    let count = paused.len();
    for task_id in paused {
        // resume_download 是 async：在 async 命令里逐个 await，
        // 内部只是换旗标 + spawn，不会真的阻塞
        let _ = resume_download(app.clone(), state.clone(), task_id).await;
    }
    Ok(count)
}

/// 一次重命名动作的结果（预演与实操共用）。
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenamePlan {
    pub renamed: usize,
    pub skipped: usize,
    pub missing: usize,
    pub details: Vec<String>,
    pub dry_run: bool,
}

/// 按当前命名规则，把已下载的条目重命名成新编号。
///
/// 只改"条目名"（编号 + 标题），**不动目录层级**：层级由「文件夹」规则决定，
/// 磁盘上的层级是历史结果，重算它需要重新联网确认归属（代价高、会触发风控）。
///
/// 匹配方式：在输出目录里递归找"去掉数字前缀后与条目标题相同"的文件，
/// 命中就原地改名；找不到的条目记进 missing，不做任何猜测。
#[tauri::command]
pub async fn rename_downloaded(
    state: State<'_, AppState>,
    input: String,
    total: usize,
    dry_run: bool,
) -> Result<RenamePlan, String> {
    let settings = state.settings_snapshot();
    let key = match parse_input(&input) {
        Ok(SourceTarget::User(uid)) => format!("user:{uid}"),
        _ => input.trim().to_string(),
    };
    let cache = {
        let batches = state.batches.lock().unwrap();
        batches.get(&key).cloned()
    };
    let Some(cache) = cache else {
        return Err("这个来源的解析结果已过期，请重新解析后再试".into());
    };
    let count = cache.items.len();
    let pad = format!("{total}").len().max(2);

    let mut plan = RenamePlan { dry_run, ..Default::default() };

    for (position, item) in cache.items.iter().enumerate() {
        let index = if total > 0 { std::cmp::max(total - position, 1) } else { position + 1 };
        let mut vars: HashMap<String, String> = HashMap::new();
        vars.insert("title".into(), item.title.clone());
        vars.insert("author".into(), item.author.clone());
        vars.insert("bid".into(), item.bid.clone());
        vars.insert("mid".into(), item.mid.clone());
        vars.insert("uid".into(), cache.uid.to_string());
        let (y, m, d) = {
            let secs = item.created_at + 8 * 3600;
            let days = secs.div_euclid(86400);
            let z = days + 719468;
            let era = if z >= 0 { z } else { z - 146096 } / 146097;
            let doe = z - era * 146097;
            let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
            let yr = yoe + era * 400;
            let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
            let mp = (5 * doy + 2) / 153;
            let dd = (doy - (153 * mp + 2) / 5 + 1) as u32;
            let mm = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
            (if mm <= 2 { yr + 1 } else { yr }, mm, dd)
        };
        vars.insert("publish_date".into(), format!("{y}-{m:02}-{d:02}"));
        vars.insert("year".into(), format!("{y}"));
        vars.insert("month".into(), format!("{m:02}"));
        vars.insert("date".into(), format!("{y}-{m:02}-{d:02}"));

        // 条目名：视频/音频是文件名，图文是文件夹名（这里都是文件）
        let wanted_stem = naming::render(&settings.naming_template, &vars, false);

        let found =
            find_downloaded_by_title(&settings.output_dir, &item.title, &wanted_stem).await;
        match found {
            Some(found) => {
                // 扩展名沿用原文件（音频是 m4a、视频是 mp4，按封装去猜会改错）
                let wanted = match found.extension() {
                    Some(ext) if !ext.is_empty() => {
                        format!(
                            "{}.{}",
                            std::path::Path::new(&wanted_stem)
                                .file_stem()
                                .map(|s| s.to_string_lossy().to_string())
                                .unwrap_or_default(),
                            ext.to_string_lossy()
                        )
                    }
                    _ => wanted_stem.clone(),
                };
                if found
                    .file_name()
                    .map(|s| s.to_string_lossy() == wanted)
                    .unwrap_or(false)
                {
                    plan.skipped += 1;
                    continue;
                }
                let target = found.with_file_name(&wanted);
                if target.exists() {
                    plan.skipped += 1;
                    plan.details.push(format!("跳过（同名已存在）：{wanted}"));
                    continue;
                }
                let from = found
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                plan.details.push(format!("{from}  →  {wanted}"));
                if !dry_run {
                    tokio::fs::rename(&found, &target)
                        .await
                        .map_err(|e| format!("重命名失败 {from}: {e}"))?;
                }
                plan.renamed += 1;
            }
            None => {
                plan.missing += 1;
            }
        }
    }
    Ok(plan)
}

/// 在输出目录里递归找"去掉数字前缀后与标题相同"的文件。
///
/// 只认两种形态：`标题` 与 `数字前缀 + 标题（可带扩展名）`，其余一律不动——
/// 宁可少改，也不猜错。
async fn find_downloaded_by_title(
    root: &std::path::Path,
    title: &str,
    wanted: &str,
) -> Option<std::path::PathBuf> {
    let wanted_stem = std::path::Path::new(wanted)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| wanted.to_string());
    let wanted_title = strip_number_prefix(&wanted_stem);
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries = match tokio::fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            if is_dir {
                stack.push(path.clone());
                continue;
            }
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| name.clone());
            let bare = strip_number_prefix(&stem);
            if bare == wanted_title || bare == title || stem == wanted_stem {
                return Some(path);
            }
        }
    }
    None
}

/// 去掉开头的"编号 + 分隔符"：`001 标题` / `12-标题` / `3 标题` 都还原成标题。
fn strip_number_prefix(name: &str) -> String {
    let trimmed = name.trim_start();
    let digits: String = trimmed.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return trimmed.trim().to_string();
    }
    trimmed[digits.len()..]
        .trim_start_matches([' ', '-', '_', '.', '、', '·'])
        .trim()
        .to_string()
}

// ───────────────────────── 应用更新 ─────────────────────────

/// 更新源仓库：发布 Release 后此处即可在线检测（tag 形如 v0.2.0）。
const UPDATE_REPO: &str = "yungumax/WeBodown";

/// 三段式版本号比较：a >= b 返回 true（逐段数值比较，缺段按 0）。
fn version_at_least(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.trim_start_matches('v')
            .split('.')
            .map(|p| p.trim().parse().unwrap_or(0))
            .collect()
    };
    let (va, vb) = (parse(a), parse(b));
    for i in 0..3 {
        let x = va.get(i).copied().unwrap_or(0);
        let y = vb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    true
}

#[tauri::command]
pub async fn check_updates() -> Result<UpdateCheck, String> {
    let current = APP_VERSION.to_string();
    let url = format!("https://api.github.com/repos/{UPDATE_REPO}/releases/latest");
    let http = reqwest::Client::builder()
        .user_agent(format!("WeBodown/{APP_VERSION}"))
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(err)?;
    let resp = http
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(err)?;
    let status = resp.status();
    if status.as_u16() == 404 {
        return Ok(UpdateCheck {
            current,
            latest: String::new(),
            up_to_date: true,
            error: format!("仓库 {UPDATE_REPO} 尚未发布 Release"),
        });
    }
    if !status.is_success() {
        return Ok(UpdateCheck {
            current,
            latest: String::new(),
            up_to_date: false,
            error: format!("检测失败 HTTP {status}"),
        });
    }
    let value: serde_json::Value = resp.json().await.map_err(err)?;
    let tag = value
        .get("tag_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let latest = tag.trim_start_matches('v').to_string();
    if latest.is_empty() {
        return Ok(UpdateCheck {
            current,
            latest: String::new(),
            up_to_date: true,
            error: "Release 未打版本标签".into(),
        });
    }
    let up_to_date = version_at_least(&current, &latest);
    Ok(UpdateCheck {
        current,
        latest,
        up_to_date,
        error: String::new(),
    })
}

// ───────────────────────── 命名预览 ─────────────────────────

#[tauri::command]
pub fn naming_variables() -> Vec<NamingVar> {
    naming::VARIABLES
        .iter()
        .map(|(token, label, section, hint)| NamingVar {
            token: token.to_string(),
            label: label.to_string(),
            section: section.to_string(),
            hint: hint.to_string(),
        })
        .collect()
}

fn preview_vars(date: &str, publish_date: &str, ext: &str) -> HashMap<String, String> {
    let mut vars = HashMap::new();
    vars.insert("title".into(), "示例微博正文".into());
    vars.insert("author".into(), "示例博主".into());
    vars.insert("bid".into(), "NbXxKq1aB".into());
    vars.insert("mid".into(), "5012345678901234".into());
    vars.insert("uid".into(), "7380874257".into());
    vars.insert("date".into(), date.to_string());
    vars.insert("publish_date".into(), publish_date.to_string());
    let (year, month) = publish_date
        .split_once('-')
        .map(|(y, rest)| (y.to_string(), rest.split('-').next().unwrap_or("01").to_string()))
        .unwrap_or((date[..4.min(date.len())].to_string(), "01".to_string()));
    vars.insert("year".into(), year);
    vars.insert("month".into(), month);
    vars.insert("source_kind".into(), "用户主页".into());
    vars.insert("index".into(), "07".into());
    vars.insert("ext".into(), ext.to_string());
    vars
}

#[tauri::command]
pub fn preview_naming(
    template: String,
    date: Option<String>,
    publish_date: Option<String>,
    ext: Option<String>,
    dir: Option<bool>,
) -> Result<String, String> {
    let today = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let (y, m, d) = civil_from_unix(now + 8 * 3600);
        format!("{y}-{m:02}-{d:02}")
    };
    let vars = preview_vars(
        &date.unwrap_or_else(|| today.clone()),
        &publish_date.unwrap_or_else(|| "2025-10-01".into()),
        &ext.unwrap_or_else(|| if dir.unwrap_or(false) { String::new() } else { "jpg".into() }),
    );
    Ok(naming::render(&template, &vars, dir.unwrap_or(false)))
}

#[tauri::command]
pub fn preview_names(
    items: Vec<serde_json::Value>,
    ext: Option<String>,
) -> Result<Vec<String>, String> {
    let today = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let (y, m, d) = civil_from_unix(now + 8 * 3600);
        format!("{y}-{m:02}-{d:02}")
    };
    let mut out = Vec::new();
    for item in items {
        let mut vars = preview_vars(&today, &publish_of(&item), &ext.clone().unwrap_or_default());
        if let Some(title) = item.get("title").and_then(|v| v.as_str()) {
            vars.insert("title".into(), naming::title_of(title));
        }
        if let Some(naming) = item.get("naming").and_then(|v| v.as_object()) {
            for (key, value) in naming {
                if let Some(text) = value.as_str() {
                    if !text.is_empty() {
                        vars.insert(key.clone(), text.to_string());
                    }
                } else if let Some(num) = value.as_u64() {
                    if *key == "index" {
                        let pad = item
                            .pointer("/naming/index_pad")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(2) as usize;
                        vars.insert("index".into(), naming::padded_index(num as u32, pad));
                    } else {
                        vars.insert(key.clone(), num.to_string());
                    }
                }
            }
        }
        out.push(naming::render(
            &crate::state::Settings::load().naming_template,
            &vars,
            false,
        ));
    }
    Ok(out)
}

fn publish_of(item: &serde_json::Value) -> String {
    item.pointer("/naming/publish_date")
        .and_then(|v| v.as_str())
        .unwrap_or("2025-10-01")
        .to_string()
}

// ───────────────────────── 系统交互与维护 ─────────────────────────

#[tauri::command]
pub async fn choose_output_dir(app: tauri::AppHandle) -> Result<String, String> {
    let folder = app
        .dialog()
        .file()
        .blocking_pick_folder()
        .map(|path| path.to_string());
    match folder {
        Some(path) => Ok(path),
        // 用户取消不报错：返回当前设置的原值由前端兜底
        None => Err("已取消选择".into()),
    }
}

#[tauri::command]
pub fn open_path(path: String) -> Result<(), String> {
    // 规范化：统一反斜杠 + 解析成绝对路径（混入的正斜杠会让资源管理器行为异常）
    let mut target = std::fs::canonicalize(&path).unwrap_or_else(|_| std::path::PathBuf::from(&path));
    // canonicalize 会加 Win32 命名空间前缀 \\?\：剥掉它，普通路径形式对 explorer 更友好
    let s = target.to_string_lossy().to_string();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        target = PathBuf::from(format!(r"\\{stripped}"));
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        target = PathBuf::from(stripped.to_string());
    }
    weibo_core::login::debug_log(&format!("open_path：{} → {}", path, target.display()));
    if !target.exists() {
        return Err(format!("路径不存在：{}", target.display()));
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const NO_WINDOW: u32 = 0x0800_0000;
        if target.is_dir() {
            // 目录：资源管理器打开目录
            std::process::Command::new("explorer.exe")
                .arg(&target)
                .creation_flags(NO_WINDOW)
                .spawn()
                .map_err(err)?;
        } else {
            // 文件：系统默认应用打开（视频→播放器、图片→看图器）
            std::process::Command::new("cmd")
                .args(["/C", "start", "", &target.to_string_lossy()])
                .creation_flags(NO_WINDOW)
                .spawn()
                .map_err(err)?;
        }
        return Ok(());
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::process::Command::new("xdg-open").arg(&target).spawn().map_err(err)?;
        Ok(())
    }
}

#[tauri::command]
pub fn resume_pending() -> Result<u32, String> {
    // 微博媒体是小文件直下，任务不落盘跨会话续传；保留接口与前端对齐。
    Ok(0)
}

#[tauri::command]
pub fn cleanup_temp(state: State<'_, AppState>) -> Result<u32, String> {
    let settings = state.settings_snapshot();
    let mut removed = 0;
    if let Ok(entries) = std::fs::read_dir(&settings.output_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(".wbotmp") {
                if std::fs::remove_dir_all(entry.path()).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    Ok(removed)
}

#[tauri::command]
pub fn export_diagnostics(state: State<'_, AppState>) -> Result<String, String> {
    let settings = state.settings_snapshot();
    let dir = settings.data_root();
    std::fs::create_dir_all(&dir).map_err(err)?;
    let cookies = state.client().export_cookies();
    let names: Vec<&str> = cookies.iter().map(|(n, _)| n.as_str()).collect();
    let text = format!(
        "WeBodown v{}\n时间：{}\n\n设置：\n{}\n\n会话 Cookie 名称：{:?}\n",
        APP_VERSION,
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let (y, m, d) = civil_from_unix(now + 8 * 3600);
            format!("{y}-{m:02}-{d:02}")
        },
        serde_json::to_string_pretty(&settings).unwrap_or_default(),
        names,
    );
    let path = dir.join("diagnostics.txt");
    std::fs::write(&path, text).map_err(err)?;
    Ok(path.to_string_lossy().to_string())
}
