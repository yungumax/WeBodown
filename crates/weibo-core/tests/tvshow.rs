//! tv/show 播客链路端到端验证：抓播放页 → 提取音频直链 → 落盘。
//! 运行：cargo test -p weibo-core --test tvshow -- --ignored --nocapture

use weibo_core::client::DomainCookies;
use weibo_core::{WeiboClient, WeiboError};

const TV_URL: &str = "https://weibo.com/tv/show/2373717:5349605416828946?from=old_pc_videoshow";

#[tokio::test]
#[ignore = "真实网络：cargo test -p weibo-core --test tvshow -- --ignored --nocapture"]
async fn tvshow_end_to_end() {
    let client = WeiboClient::new().expect("创建客户端");
    // 装登录 Cookie（tv/show 页面要有 PC 域 Cookie 才会渲染出音频数据）
    if let Ok(text) = std::fs::read_to_string(
        std::env::var("APPDATA")
            .map(|a| std::path::PathBuf::from(a).join("webodown").join("cookies.json"))
            .expect("无 APPDATA"),
    ) {
        if let Ok(cookies) = serde_json::from_str::<DomainCookies>(&text) {
            client.install_domains(&cookies);
            println!("已装载登录 Cookie");
        }
    }

    let post = match client.tvshow_audio(TV_URL).await {
        Ok(p) => p,
        Err(WeiboError::Api(m)) => {
            println!("❌ 解析失败：{m}");
            return;
        }
        Err(e) => {
            println!("❌ 其他错误：{e}");
            return;
        }
    };
    println!(
        "✅ 解析：标题={} 作者={} 时长={}秒 音频流 {} 个",
        post.text,
        post.screen_name,
        post.duration,
        post.audios.len()
    );
    for (label, url) in &post.audios {
        println!("   [{label}] {}{}", url.chars().take(90).collect::<String>(), "...");
    }

    // 直接落盘验证首选流
    let Some((_, url)) = post.audios.first() else {
        println!("❌ 没有可下载的音频流");
        return;
    };
    let dest = std::env::temp_dir().join("webodown-tvshow-audio.mp3");
    let opts = weibo_core::download::DownloadOptions { retries: 1 };
    match weibo_core::download::download(
        &client.http,
        url,
        &dest,
        &opts,
        std::sync::Arc::new(|_| {}),
    )
    .await
    {
        Ok(size) => println!("✅ 落盘 {} → {size} bytes", dest.display()),
        Err(e) => println!("❌ 下载失败：{e}"),
    }
}
