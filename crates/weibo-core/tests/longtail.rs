//! 长尾真实测试台：用本机 cookies.json（登录态）把应用的核心路径在多种真实内容上
//! 全部走一遍，输出结构化报告。运行：
//!   cargo test -p weibo-core --test longtail -- --ignored --nocapture
//!
//! 覆盖：登录恢复 / 收藏 / 关注 / 三类博主时间线 / 逐条详情（移动+PC 合并）/
//! 视频档位分布 / 长文 / 转发结构 / 视频 1080P 落盘 / 图片原图落盘。

use weibo_core::client::DomainCookies;
use weibo_core::download as dl;
use weibo_core::{WeiboClient, WeiboError};

fn load_client() -> Option<WeiboClient> {
    let path = std::env::var("APPDATA")
        .map(|a| std::path::PathBuf::from(a).join("webodown").join("cookies.json"))
        .expect("无 APPDATA");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => {
            println!("❌ 无 cookies.json（先在应用里登录一次）");
            return None;
        }
    };
    let cookies: DomainCookies = serde_json::from_str(&text).expect("cookies.json 解析失败");
    let client = WeiboClient::new().expect("创建客户端");
    client.install_domains(&cookies);
    Some(client)
}

fn report(line: &str) {
    println!("{line}");
}

/// 全流程长尾扫描（单测体，顺序执行各场景并汇总）。
#[tokio::test]
#[ignore = "真实网络长尾测试：cargo test -p weibo-core --test longtail -- --ignored --nocapture"]
async fn longtail_scan() {
    let Some(client) = load_client() else { return };

    // ── 0. 登录态 ──
    let (uid, uname) = match client.account().await {
        Ok(Some((uid, uname, _))) => {
            report(&format!("✅ 登录态 uid={uid} ({uname})"));
            (uid, uname)
        }
        _ => {
            report("❌ 登录态失效，后续需要登录的场景全部跳过");
            return;
        }
    };
    let _ = uname;

    // ── 1. 收藏 / 关注（登录专属接口）──
    match client.favourites_wap_page(uid, 1).await {
        Ok(page) => report(&format!(
            "✅ 收藏第一页 {} 条（since_id={:?}）",
            page.posts.len(),
            page.since_id
        )),
        Err(e) => report(&format!("⚠️ 收藏读取失败：{e}")),
    }
    match client.following_all(uid).await {
        Ok((list, total)) => report(&format!(
            "✅ 关注列表（PC 全量）{} 人（接口声称总数 {}）",
            list.len(),
            total
        )),
        Err(e) => report(&format!("⚠️ 关注列表失败：{e}")),
    }

    // ── 2. 三类博主：综合资讯（视频多）、个人博主（转发多）、图片博 ──
    // 人民日报 / Violetxpter（用户实测过的）/ 央视新闻
    let bloggers: [(u64, &str); 3] = [
        (2803301701, "人民日报"),
        (6458148211, "Violetxpter"),
        (2656274875, "央视新闻"),
    ];

    let mut video_posts = 0;
    let mut downloaded_video = false;
    let mut downloaded_image = false;
    let tmp = std::env::temp_dir().join("webodown-longtail");
    let _ = std::fs::create_dir_all(&tmp);

    for (buid, bname) in bloggers {
        report(&format!("── 博主 {bname}（{buid}）──"));
        let page = match client.user_timeline(buid, "").await {
            Ok(p) => p,
            Err(WeiboError::NeedLogin(m)) => {
                report(&format!("  ⚠️ 需要登录：{m}"));
                continue;
            }
            Err(e) => {
                report(&format!("  ❌ 时间线失败：{e}"));
                continue;
            }
        };
        report(&format!(
            "  时间线第一页 {} 条（since_id={:?}）",
            page.posts.len(),
            page.since_id
        ));

        for post in page.posts.iter().take(6) {
            // 与应用同路径：移动详情 + PC 增强
            let mut detail = match client.post_detail(&post.bid).await {
                Ok(d) => d,
                Err(e) => {
                    report(&format!("  ❌ {} 详情失败：{}", post.bid, e));
                    continue;
                }
            };
            let mut pc_videos = 0;
            match client.post_detail_pc(&post.bid).await {
                Ok(pc) => {
                    pc_videos = pc.videos.len();
                    for (label, url) in pc.videos {
                        if !detail.videos.iter().any(|(_, u)| u == &url) {
                            detail.videos.push((label, url));
                        }
                    }
                    if !pc.pics.is_empty() {
                        detail.pics = pc.pics;
                    }
                }
                Err(e) => report(&format!("  ⚠️ {} PC 增强失败：{}", post.bid, e)),
            }

            let kind = if detail.is_retweet {
                "转发"
            } else if !detail.videos.is_empty() {
                "视频"
            } else if !detail.pics.is_empty() {
                "图文"
            } else {
                "文字"
            };
            let labels: Vec<&str> = detail.videos.iter().map(|(l, _)| l.as_str()).collect();
            report(&format!(
                "  {} [{kind}] 视频{}档{labels:?} 图{}张 长文标记{}",
                detail.bid,
                detail.videos.len(),
                detail.pics.len(),
                pc_videos
            ));
            if !detail.videos.is_empty() {
                video_posts += 1;
            }

            // 每博主只做一次落盘验证：1080P 视频 + 一张原图
            if !downloaded_video && detail.videos.iter().any(|(l, _)| l.contains("1080")) {
                let mut sorted = detail.videos.clone();
                sorted.sort_by(|a, b| {
                    let rank = |l: &str| {
                        let l = l.to_lowercase();
                        if l.contains("4k") || l.contains("原画") { 5 }
                        else if l.contains("1080") { 4 }
                        else if l.contains("720") { 3 }
                        else if l.contains("480") { 2 }
                        else { 1 }
                    };
                    rank(&b.0).cmp(&rank(&a.0))
                });
                let (label, url) = sorted[0].clone();
                let dest = tmp.join(format!("longtail-video-{}.mp4", detail.bid));
                let opts = dl::DownloadOptions { retries: 2 };
                match dl::download(
                    &client.http,
                    &url,
                    &dest,
                    &opts,
                    std::sync::Arc::new(|_| {}),
                )
                .await
                {
                    Ok(size) => {
                        report(&format!(
                            "  ✅ 视频落盘 [{label}] {} → {} bytes",
                            dest.display(),
                            size
                        ));
                        downloaded_video = true;
                    }
                    Err(e) => report(&format!("  ❌ 视频落盘失败：{e}")),
                }
            }
            if !downloaded_image && detail.pics.len() >= 2 {
                let url = weibo_core::download::image_variant(&detail.pics[0], "large");
                let dest = tmp.join(format!("longtail-img-{}.jpg", detail.bid));
                let opts = dl::DownloadOptions { retries: 2 };
                match dl::download(
                    &client.http,
                    &url,
                    &dest,
                    &opts,
                    std::sync::Arc::new(|_| {}),
                )
                .await
                {
                    Ok(size) => {
                        report(&format!("  ✅ 图片落盘 {} → {} bytes", dest.display(), size));
                        downloaded_image = true;
                    }
                    Err(e) => report(&format!("  ❌ 图片落盘失败：{e}")),
                }
            }
        }
    }

    // ── 3. 汇总 ──
    report("── 汇总 ──");
    report(&format!("扫描到含视频微博：{video_posts} 条"));
    report(&format!(
        "落盘验证：视频 {} / 图片 {}",
        if downloaded_video { "✅" } else { "（本轮没有 1080P 样本）" },
        if downloaded_image { "✅" } else { "（本轮没有多图样本）" }
    ));
    let _ = uid;
}
