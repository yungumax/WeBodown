//! 真实网络集成测试：默认忽略（`cargo test --ignored` 才跑），验证移动端接口与 CDN 可达。

use weibo_core::WeiboClient;

/// 复现应用的登录校验路径：读 cookies.json（数据目录）→ install_cookies → account()。
/// 与桌面端 account() 完全同代码，用来隔离"Cookie 无效"还是"客户端行为差异"。
#[tokio::test]
#[ignore = "需要真实网络与本机 cookies.json"]
async fn verify_saved_cookies_login() {
    let path = std::env::var("APPDATA")
        .map(|a| std::path::PathBuf::from(a).join("webodown").join("cookies.json"))
        .expect("无 APPDATA");
    let text = std::fs::read_to_string(&path).expect("cookies.json 不存在");
    let cookies: Vec<(String, String)> =
        serde_json::from_str(&text).expect("cookies.json 解析失败");
    println!(
        "载入 {} 个 Cookie：{:?}",
        cookies.len(),
        cookies.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
    let client = WeiboClient::new().expect("创建客户端");
    client.install_domains(&weibo_core::client::DomainCookies { m: cookies.clone(), pc: cookies });
    match client.account().await {
        Ok(Some((uid, uname, _))) => println!("✅ 登录校验通过 uid={uid} uname={uname}"),
        Ok(None) => println!("❌ api/config 返回未登录"),
        Err(e) => println!("❌ 接口错误：{e}"),
    }
}

/// A/B 矩阵：同一个 SUB Cookie，穷举请求形态差异，找出 curl 成功 / reqwest 失败的分叉点。
#[tokio::test]
#[ignore = "需要真实网络与本机 cookies.json"]
async fn ab_matrix_login_check() {
    let path = std::env::var("APPDATA")
        .map(|a| std::path::PathBuf::from(a).join("webodown").join("cookies.json"))
        .expect("无 APPDATA");
    let text = std::fs::read_to_string(&path).expect("cookies.json 不存在");
    let cookies: Vec<(String, String)> = serde_json::from_str(&text).expect("解析失败");
    let sub = cookies
        .iter()
        .find(|(n, _)| n == "SUB")
        .map(|(_, v)| v.clone())
        .expect("无 SUB");
    let all: String = cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");

    let ua = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
    let url = "https://m.weibo.cn/api/config";

    let base = || {
        reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(20))
    };

    // A：裸客户端 + 最小三件套（curl 同款，无 Accept-Language/Accept-Encoding）
    let b = base()
        .user_agent(ua)
        .build()
        .unwrap();
    let r = b
        .get(url)
        .header("Cookie", format!("SUB={sub}"))
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Referer", "https://m.weibo.cn/")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    println!("A 裸客户端+SUB → login={:?}", r.pointer("/data/login"));

    // B：A + 默认头样式（Accept-Language）
    let b = base()
        .user_agent(ua)
        .build()
        .unwrap();
    let r = b
        .get(url)
        .header("Cookie", format!("SUB={sub}"))
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Referer", "https://m.weibo.cn/")
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    println!("B 加 Accept-Language → login={:?}", r.pointer("/data/login"));

    // C：应用同款 WeiboClient（默认头全套 + 显式 Cookie）
    let c = WeiboClient::new().unwrap();
    c.install_cookies(&[("SUB".into(), sub.clone())]);
    match c.account().await {
        Ok(Some((uid, uname, _))) => println!("C WeiboClient.account → 通过 uid={uid} uname={uname}"),
        Ok(None) => println!("C WeiboClient.account → 未登录"),
        Err(e) => println!("C WeiboClient.account → 错误：{e}"),
    }

    // D：全量 Cookie + 裸客户端
    let b = base()
        .user_agent(ua)
        .build()
        .unwrap();
    let r = b
        .get(url)
        .header("Cookie", &all)
        .header("X-Requested-With", "XMLHttpRequest")
        .header("Referer", "https://m.weibo.cn/")
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    println!("D 裸客户端+全量Cookie → login={:?}", r.pointer("/data/login"));
}

/// 手动复现网页登录绑定路径：把 webview 三个域的 Cookie 硬编码进此测试跑一次 account()。
/// 用法：把 qrcode_debug.log 里 webview 行的 Cookie 名单对应值填进 WEBCOOKIES 环境变量
/// （格式 "name=value; name=value; ..."），然后 cargo test --ignored --nocapture。
#[tokio::test]
#[ignore = "需要真实网络与 WEBCOOKIES 环境变量"]
async fn manual_webview_cookie_check() {
    let raw = std::env::var("WEBCOOKIES").expect("设置 WEBCOOKIES=\"k=v; k=v\" 再跑");
    let pairs: Vec<(String, String)> = raw
        .split(';')
        .filter_map(|pair| {
            let pair = pair.trim();
            pair.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect();
    println!("载入 {} 个 Cookie", pairs.len());
    let client = WeiboClient::new().unwrap();
    client.install_cookies(&pairs);
    match client.account().await {
        Ok(Some((uid, uname, _))) => println!("✅ 通过 uid={uid} uname={uname}"),
        Ok(None) => println!("❌ 未登录"),
        Err(e) => println!("❌ 错误：{e}"),
    }
}

/// 人民日报（公开账号）时间线第一页：验证接口可达。
/// 匿名访问被微博机会性放行：拿到数据或明确要求登录都算网络层工作正常。
#[tokio::test]
#[ignore = "需要真实网络，手动执行：cargo test -p weibo-core --test network -- --ignored"]
async fn fetch_public_user_timeline() {
    let client = WeiboClient::new().expect("创建客户端");
    client.warmup().await.expect("预热");
    match client.user_timeline(2803301701, "").await {
        Ok(page) => {
            assert!(!page.posts.is_empty(), "时间线第一页不应为空");
            let first = &page.posts[0];
            assert!(!first.bid.is_empty(), "bid 不应为空");
            println!(
                "首条：{}（{} 图，视频 {}）",
                first.text.chars().take(30).collect::<String>(),
                first.pics.len(),
                first.videos.len()
            );
        }
        Err(weibo_core::WeiboError::NeedLogin(msg)) => {
            println!("匿名被拒（符合预期，扫码后可用）：{msg}");
        }
        Err(e) => panic!("意外的网络错误：{e}"),
    }
}

/// 单条微博详情：有时间线数据时验证 statuses/show 与媒体抽取（匿名被拒则跳过）。
#[tokio::test]
#[ignore = "需要真实网络"]
async fn fetch_post_detail_with_media() {
    let client = WeiboClient::new().expect("创建客户端");
    client.warmup().await.expect("预热");
    let page = match client.user_timeline(2803301701, "").await {
        Ok(page) => page,
        Err(weibo_core::WeiboError::NeedLogin(msg)) => {
            println!("匿名被拒（符合预期，扫码后可用）：{msg}");
            return;
        }
        Err(e) => panic!("意外的网络错误：{e}"),
    };
    // 找一条带图的（前若干条内通常都有）
    let Some(with_pic) = page.posts.iter().find(|p| !p.pics.is_empty()) else {
        println!("这一页没有带图微博，跳过详情验证");
        return;
    };
    let detail = client.post_detail(&with_pic.bid).await.expect("单条详情");
    assert_eq!(detail.bid, with_pic.bid);
    assert!(!detail.pics.is_empty(), "详情里的图片列表不应为空");
    assert!(
        detail.pics[0].starts_with("https://"),
        "图片地址应为 https：{}",
        detail.pics[0]
    );
    println!("详情：{} 图，首图 {}", detail.pics.len(), detail.pics[0]);
}
