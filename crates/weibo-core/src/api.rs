//! m.weibo.cn 接口封装：单条微博、用户时间线、收藏、关注、登录态。
//!
//! 全部走 Value 抽取而不是完整反序列化：微博接口字段冗多且各端不一致，
//! 按需取值对改版最稳（缺字段就是没有，不会整条解析失败）。

use crate::client::WeiboClient;
use crate::error::{Result, WeiboError};
use serde_json::Value;

/// 一条微博解析出的干净结构（图片/视频/文案全齐）。
#[derive(Debug, Clone, Default)]
pub struct PostData {
    pub bid: String,
    pub mid: String,
    /// 正文纯文本（已去 HTML）
    pub text: String,
    pub created_at: i64,
    pub user_id: u64,
    pub screen_name: String,
    pub pics: Vec<String>,
    /// 播放地址（label, url），按清晰度从高到低排
    pub videos: Vec<(String, String)>,
    /// 音频流（声音帖子）(label, url)：hd→"高品质"，sd→"标准"
    pub audios: Vec<(String, String)>,
    pub is_audio: bool,
    pub is_retweet: bool,
    pub duration: i64,
}

/// 博主信息。
#[derive(Debug, Clone, Default)]
pub struct UserInfo {
    pub uid: u64,
    pub screen_name: String,
    pub avatar: String,
    pub verified: bool,
    pub followers: String,
    /// 微博数（关注列表卡片显示用；接口不给就是 0）
    pub statuses_count: u32,
    pub description: String,
}

/// 时间线一页。
#[derive(Debug, Clone, Default)]
pub struct TimelinePage {
    pub posts: Vec<PostData>,
    pub since_id: String,
    /// 接口没有给总数（微博不给），恒 0；前端只显示已加载数。
    pub total: usize,
}

impl WeiboClient {
    /// 单条微博详情：`statuses/show`。
    pub async fn post_detail(&self, bid: &str) -> Result<PostData> {
        let url = format!("https://m.weibo.cn/statuses/show?id={bid}");
        let value = self.fetch_json(&url).await?;
        let data = value
            .get("data")
            .ok_or_else(|| WeiboError::Decode("statuses/show 没有 data 字段".into()))?;
        let post = parse_post(data);
        if post.bid.is_empty() {
            return Err(WeiboError::Api("这条微博可能已被删除或仅自己可见".into()));
        }
        Ok(post)
    }

    /// 单条微博详情（PC 域 weibo.com/ajax）：视频档位齐全（1080P/原画只在 PC 给出），
    /// pic_infos 也是网页"原图"同款规格。需要 weibo.com 登录 Cookie。
    /// 注意：PC 接口的微博对象在**顶层**（无 data 包裹，只有 ok 字段伴生）。
    pub async fn post_detail_pc(&self, bid: &str) -> Result<PostData> {
        let url = format!("https://weibo.com/ajax/statuses/show?id={bid}");
        let value = self.fetch_json_pc(&url).await?;
        let post = parse_post(&value);
        if post.bid.is_empty() {
            return Err(WeiboError::Api("PC 接口未返回有效微博".into()));
        }
        Ok(post)
    }

    /// tv/show 音频/播客播放页：页面是 Next.js SSR，HTML 里直接内嵌
    /// 带签名的音频直链（podcast.video.weibocdn.com，\u0026 转义）与元信息。
    /// 这类内容不在常规微博体系里（statuses/show 会说不存在）。
    pub async fn tvshow_audio(&self, url: &str) -> Result<PostData> {
        let html = match self.fetch_text_pc(url, "https://weibo.cn/").await {
            Ok(text) => text,
            Err(_) => self.fetch_text(url, "https://m.weibo.cn/").await?,
        };

        // 音频直链：RSC 数据里 & 被转义成 \u0026（带反斜杠的序列），
        // 正则必须容纳转义序列，否则 URL 在第一个 & 处截断、丢失全部签名参数（实测 403）
        let stream_re = regex::Regex::new(
            r#"https://podcast\.video\.weibocdn\.com/(?:[^"\\\s]|\\u[0-9a-fA-F]{4})+"#
        )
        .unwrap();
        let mut streams: Vec<String> = stream_re
            .find_iter(&html)
            .map(|m| unescape_unicode(m.as_str()))
            .collect();
        streams.sort();
        streams.dedup();
        if streams.is_empty() {
            return Err(WeiboError::Api(
                "播放页里没有找到音频直链（可能已下架或需付费）".into(),
            ));
        }
        let audios: Vec<(String, String)> = streams
            .iter()
            .enumerate()
            .map(|(i, u)| {
                (
                    if i == 0 { "高品质".to_string() } else { format!("备用 {}", i) },
                    u.clone(),
                )
            })
            .collect();

        let media_id = audios
            .first()
            .and_then(|(_, u)| {
                u.split("media_id=")
                    .nth(1)
                    .map(|rest| rest.split('&').next().unwrap_or(rest).to_string())
            })
            .unwrap_or_default();

        // 标题：取"微博音频 · 播放页"之外的那个 title 字段，缺省用摘要
        let title_re =
            regex::Regex::new(r#""title","[^"]*",\{"children":"([^"]{2,80})""#).unwrap();
        let title = title_re
            .captures_iter(&html)
            .find_map(|c| c.get(1).map(|g| unescape_unicode(g.as_str())))
            .filter(|t| !t.contains("播放页") && !t.contains("could not be found"))
            .unwrap_or_else(|| "微博音频".to_string());

        // 作者：RSC 的 source 字段
        let author = regex::Regex::new(r#""source":"([^"]{1,40})""#)
            .unwrap()
            .captures(&html)
            .and_then(|c| c.get(1))
            .map(|g| unescape_unicode(g.as_str()))
            .unwrap_or_default();

        // 时长 MM:SS → 秒
        let duration = regex::Regex::new(r#""duration":"(\d{1,2}):(\d{2})""#)
            .unwrap()
            .captures(&html)
            .and_then(|c| {
                let m: u64 = c.get(1)?.as_str().parse().ok()?;
                let s: u64 = c.get(2)?.as_str().parse().ok()?;
                Some(m * 60 + s)
            })
            .unwrap_or(0) as i64;

        Ok(PostData {
            bid: if media_id.is_empty() { url.to_string() } else { media_id },
            mid: String::new(),
            text: title,
            created_at: 0,
            user_id: 0,
            screen_name: author,
            pics: Vec::new(),
            videos: Vec::new(),
            audios,
            is_audio: true,
            is_retweet: false,
            duration,
        })
    }

    /// 长文：`statuses/extend`（isLongText 时正文被截断，这里取全文）。
    pub async fn long_text(&self, bid: &str) -> Result<String> {
        let url = format!("https://m.weibo.cn/statuses/extend?id={bid}");
        let value = self.fetch_json(&url).await?;
        let text = value
            .pointer("/data/longTextContent")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        Ok(strip_html(text))
    }

    /// 博主信息（也用来校验 UID 是否存在）。
    pub async fn user_info(&self, uid: u64) -> Result<UserInfo> {
        let url = format!("https://m.weibo.cn/api/container/getIndex?type=uid&value={uid}");
        let value = self.fetch_json(&url).await?;
        let info = value
            .pointer("/data/userInfo")
            .ok_or_else(|| WeiboError::Api("这个 UID 不存在或不可见".into()))?;
        Ok(UserInfo {
            uid,
            screen_name: str_at(info, &["screen_name"]).unwrap_or_default(),
            // 头像：多字段回退 + http 归一（http 链接会被 WebView 拦掉显示成破图）
            avatar: ["avatar_hd", "avatar_large", "profile_image_url"]
                .iter()
                .find_map(|key| str_at(info, &[key]))
                .map(|u| https(&u))
                .unwrap_or_default(),
            verified: info.get("verified").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
            followers: str_at(info, &["followers_count"]).unwrap_or_default(),
            statuses_count: info.get("statuses_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            description: str_at(info, &["description"]).unwrap_or_default(),
        })
    }

    /// 用户时间线（最新在前，since_id 向更旧翻页）。
    pub async fn user_timeline(&self, uid: u64, since_id: &str) -> Result<TimelinePage> {
        let containerid = format!("107603{uid}");
        let url = format!(
            "https://m.weibo.cn/api/container/getIndex?type=uid&value={uid}&containerid={containerid}&since_id={since_id}"
        );
        let value = self.fetch_json(&url).await?;
        self.timeline_from_value(&value).await
    }

    /// 我的收藏（需要登录态）：weibo.cn 经典 wap 收藏页（HTML 列表）。
    /// m 域没有收藏容器（个人主页只有精选/微博/相册三个标签）、PC ajax 全 404——
    /// wap 页是唯一稳定入口（实测踩过）。只提取 bid/作者/文案摘要：
    /// 内容库点卡片时会按 bid 走完整详情，轻量字段足够。
    pub async fn favourites_wap_page(&self, uid: u64, page: u32) -> Result<TimelinePage> {
        let url = if page <= 1 {
            format!("https://weibo.cn/fav/{uid}")
        } else {
            format!("https://weibo.cn/fav/{uid}?page={page}")
        };
        let html = self.fetch_text(&url, "https://m.weibo.cn/").await?;
        Ok(parse_favourites_html(&html, page))
    }

    /// 我关注的人（完整列表）：PC 域与 m 域并集（两域返回的"最近名单"取并集）。
    /// 注意：微博接口只开放最近的一部分关注（next_cursor=0 无更多页，
    /// has_filtered_attentions=true 表示有被过滤的）——更早的关注只有官方客户端可见。
    /// 返回（列表, 接口声称的总数）。
    pub async fn following_all(&self, uid: u64) -> Result<(Vec<UserInfo>, usize)> {
        let mut all: Vec<UserInfo> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        let mut reported_total = 0usize;

        // PC 域（含 total_number 与 next_cursor）
        if let Ok(value) = self.following_pc_raw(uid).await {
            reported_total = value
                .pointer("/data/total_number")
                .and_then(|v| v.as_u64())
                .unwrap_or(0) as usize;
            let users = value
                .pointer("/data/follows/users")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for user in &users {
                let id = user.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                if seen.insert(id) {
                    all.push(Self::user_info_from_value(user));
                }
            }
        }
        // m 域容器翻页补齐（与 PC 取并集）
        if let Ok(list) = self.following().await {
            for info in list {
                if seen.insert(info.uid) {
                    all.push(info);
                }
            }
        }
        let count = all.len();
        Ok((all, reported_total.max(count)))
    }

    /// PC 域关注列表原始响应（含 total_number / next_cursor）。
    async fn following_pc_raw(&self, uid: u64) -> Result<Value> {
        let url = format!(
            "https://weibo.com/ajax/profile/followContent?uid={uid}&page=1"
        );
        self.fetch_json_pc(&url).await
    }

    /// 从接口的 user JSON 构造 UserInfo。
    fn user_info_from_value(user: &Value) -> UserInfo {
        UserInfo {
            uid: user.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
            screen_name: str_at(user, &["screen_name"]).unwrap_or_default(),
            avatar: ["avatar_hd", "avatar_large", "profile_image_url"]
                .iter()
                .find_map(|key| str_at(user, &[key]))
                .map(|u| https(&u))
                .unwrap_or_default(),
            verified: user.get("verified").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
            followers: str_at(user, &["followers_count"]).unwrap_or_default(),
            statuses_count: user.get("statuses_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
            description: str_at(user, &["description"]).unwrap_or_default(),
        }
    }

    /// 我关注的人（需要登录态）：关注用户嵌在 `card_group` 里
    /// （只扫顶层会漏光，实测踩过）。容器按页翻（每页约 10-20 人），
    /// 这里自动翻完所有页（最多 50 页防失控）。
    pub async fn following(&self) -> Result<Vec<UserInfo>> {
        let mut raw_users: Vec<Value> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for page in 1..=50u32 {
            let url = format!(
                "https://m.weibo.cn/api/container/getIndex?containerid=231093_-_selffollowed&page={page}"
            );
            let value = self.fetch_json(&url).await?;
            let cards = value
                .pointer("/data/cards")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let mut page_new = 0;
            for card in &cards {
                if let Some(user) = card.get("user") {
                    let id = user.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                    if seen.insert(id) {
                        raw_users.push(user.clone());
                        page_new += 1;
                    }
                }
                for g in card
                    .get("card_group")
                    .and_then(|v| v.as_array())
                    .into_iter()
                    .flatten()
                {
                    if let Some(user) = g.get("user") {
                        let id = user.get("id").and_then(|v| v.as_u64()).unwrap_or(0);
                        if seen.insert(id) {
                            raw_users.push(user.clone());
                            page_new += 1;
                        }
                    }
                }
            }
            // 本页没有新面孔 = 到底了（微博翻页末页会重复返回最后一批）
            if page_new == 0 {
                break;
            }
        }
        Ok(raw_users
            .iter()
            .map(|user| UserInfo {
                uid: user.get("id").and_then(|v| v.as_u64()).unwrap_or(0),
                screen_name: str_at(user, &["screen_name"]).unwrap_or_default(),
                avatar: ["avatar_hd", "avatar_large", "profile_image_url"]
                    .iter()
                    .find_map(|key| str_at(user, &[key]))
                    .map(|u| https(&u))
                    .unwrap_or_default(),
                verified: user.get("verified").and_then(|v| v.as_i64()).unwrap_or(0) > 0,
                followers: str_at(user, &["followers_count"]).unwrap_or_default(),
                statuses_count: user.get("statuses_count").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
                description: str_at(user, &["description"]).unwrap_or_default(),
            })
            .collect())
    }

    /// 登录态与当前账号：`api/config`。
    pub async fn account(&self) -> Result<Option<(u64, String, String)>> {
        let value = self.fetch_json("https://m.weibo.cn/api/config").await?;
        let data = value.get("data").cloned().unwrap_or_default();
        // login 是 JSON 布尔（true/false），别用 as_i64 读——布尔读成 None 会永远判未登录
        let login = data
            .get("login")
            .map(|v| v.as_bool().unwrap_or_else(|| v.as_i64().unwrap_or(0) == 1))
            .unwrap_or(false);
        if !login {
            return Ok(None);
        }
        let uid = data.get("uid").and_then(|v| v.as_str()).and_then(|s| s.parse().ok()).unwrap_or(0);
        let info = self.user_info(uid).await?;
        Ok(Some((uid, info.screen_name, info.avatar)))
    }

    /// 通用：从 getIndex 的返回里抽出 mblog 列表与下一页 since_id。
    async fn timeline_from_value(&self, value: &Value) -> Result<TimelinePage> {
        let cards = value
            .pointer("/data/cards")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut posts = Vec::new();
        for card in &cards {
            if let Some(mblog) = card.get("mblog") {
                let mut post = parse_post(mblog);
                // 时间线里正文可能被截断（isLongText），有需要再补拉长文
                if mblog.get("isLongText").and_then(|v| v.as_i64()).unwrap_or(0) == 1
                    && !post.bid.is_empty()
                {
                    if let Ok(full) = self.long_text(&post.bid).await {
                        if !full.is_empty() {
                            post.text = full;
                        }
                    }
                }
                posts.push(post);
            }
        }
        // since_id 可能是字符串也可能是数字（登录态实测返回 int），两种都要认——
        // 只按字符串读会在登录态下读空，把还没拉完的来源误判成"已到底"
        let since_id = value
            .pointer("/data/cardlistInfo/since_id")
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Number(n) => n.to_string(),
                _ => String::new(),
            })
            .unwrap_or_default();
        Ok(TimelinePage {
            posts,
            since_id,
            total: 0,
        })
    }
}

/// mblog / statuses-show 的 data → 干净结构。字段缺失一律按"没有"处理。
pub fn parse_post(data: &Value) -> PostData {
    let mut pics = Vec::new();
    // statuses/show 用 pics[].large.url；时间线 mblog 用 pics[].large.url（同构）
    if let Some(arr) = data.get("pics").and_then(|v| v.as_array()) {
        for pic in arr {
            let url = pic
                .pointer("/large/url")
                .or_else(|| pic.pointer("/bmiddle/url"))
                .or_else(|| pic.pointer("/url"))
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if !url.is_empty() {
                pics.push(https(url));
            }
        }
    }
    // 新版 pic_infos：{name: {large: {url}, thumbnail: {url}}}
    if pics.is_empty() {
        if let Some(map) = data.get("pic_infos").and_then(|v| v.as_object()) {
            for pic in map.values() {
                let url = pic
                    .pointer("/largest/url")
                    .or_else(|| pic.pointer("/large/url"))
                    .or_else(|| pic.pointer("/thumbnail/url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if !url.is_empty() {
                    pics.push(https(url));
                }
            }
        }
    }

    let mut videos = Vec::new();
    let mut audios = Vec::new();
    let is_audio = data
        .pointer("/page_info/type")
        .and_then(|v| v.as_str())
        .map(|t| t == "audio")
        .unwrap_or(false);
    let mut duration = 0;
    if let Some(media) = data.pointer("/page_info/media_info") {
        // 音频帖子：stream_url_hd（高品质）/ stream_url（标准）
        if is_audio {
            for (key, label) in [("stream_url_hd", "高品质"), ("stream_url", "标准")] {
                let url = media.get(key).and_then(|v| v.as_str()).unwrap_or_default();
                if !url.is_empty() {
                    audios.push((label.to_string(), https(url)));
                }
            }
        }
        // 新版：playback_list（清晰度数组，play_info.url 直接是 mp4）
        if let Some(list) = media.get("playback_list").and_then(|v| v.as_array()) {
            for item in list {
                let info = item.get("play_info").cloned().unwrap_or_default();
                let url = info
                    .get("url")
                    .or_else(|| info.get("mp4_url"))
                    .or_else(|| info.get("mp4_hd_url"))
                    .or_else(|| info.get("mp4_sd_url"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                let label = info
                    .get("quality_label")
                    .and_then(|v| v.as_str())
                    .unwrap_or("视频")
                    .to_string();
                if !url.is_empty() {
                    videos.push((label, https(url)));
                }
            }
        }
        // 旧版：直接给 stream_url / stream_url_hd
        if videos.is_empty() {
            for (key, label) in [("stream_url_hd", "高清"), ("stream_url", "标清")] {
                let url = media.get(key).and_then(|v| v.as_str()).unwrap_or_default();
                if !url.is_empty() {
                    videos.push((label.to_string(), https(url)));
                }
            }
        }
        duration = media
            .get("duration")
            .and_then(|v| v.as_f64())
            .map(|d| d as i64)
            .unwrap_or(0);
    }
    // 视频时长有时在 page_info 上
    if duration == 0 {
        duration = data
            .pointer("/page_info/media_duration")
            .and_then(|v| v.as_f64())
            .map(|d| d as i64)
            .unwrap_or(0);
    }

    let created_at = parse_weibo_time(&str_at(data, &["created_at"]).unwrap_or_default());

    PostData {
        // 移动端叫 bid，PC 域叫 mblogid（同一标识的两种命名）
        bid: str_at(data, &["bid"])
            .or_else(|| str_at(data, &["mblogid"]))
            .unwrap_or_default(),
        mid: data
            .get("id")
            .and_then(|v| v.as_u64().map(|n| n.to_string()))
            .or_else(|| data.get("id").and_then(|v| v.as_str().map(str::to_string)))
            .or_else(|| data.get("mid").and_then(|v| v.as_str().map(str::to_string)))
            .unwrap_or_default(),
        text: str_at(data, &["text_raw"])
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| strip_html(&str_at(data, &["text"]).unwrap_or_default())),
        created_at,
        user_id: data.pointer("/user/id").and_then(|v| v.as_u64()).unwrap_or(0),
        screen_name: str_at(data, &["user", "screen_name"]).unwrap_or_default(),
        pics,
        videos,
        audios,
        is_audio,
        is_retweet: data.get("retweeted_status").is_some(),
        duration,
    }
}

fn str_at(value: &Value, path: &[&str]) -> Option<String> {
    let mut current = value;
    for key in path {
        current = current.get(key)?;
    }
    current.as_str().map(str::to_string)
}

fn https(url: &str) -> String {
    if let Some(rest) = url.strip_prefix("http://") {
        format!("https://{rest}")
    } else if let Some(rest) = url.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        url.to_string()
    }
}

/// 去掉微博正文里的 HTML 标签与常见实体。
/// 解 RSC/JSON 字符串里的 Unicode 转义（\u0026 → & 等，覆盖常用几类）。
pub fn unescape_unicode(s: &str) -> String {
    if !s.contains("\\u") {
        return s.to_string();
    }
    let re = regex::Regex::new(r"\\u([0-9a-fA-F]{4})").unwrap();
    re.replace_all(s, |caps: &regex::Captures| {
        u32::from_str_radix(&caps[1], 16)
            .ok()
            .and_then(char::from_u32)
            .map(|c| c.to_string())
            .unwrap_or_default()
    })
    .replace("\\/", "/")
}

pub fn strip_html(html: &str) -> String {
    let re = regex::Regex::new(r"<[^>]+>").unwrap();
    let text = re.replace_all(html, "");
    text.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

/// weibo.cn wap 收藏页 HTML → TimelinePage。
///
/// 页面结构：每条收藏一个 `<div class="c" id="Mxxx">` 块，块内有
/// `weibo.cn/comment/<bid>?...uid=<作者>` 链接与正文；"已不可见"的条目跳过
/// （收藏还在但原文取不到，下载必失败）；页尾有 `page=N+1` 链接即还有下一页。
pub fn parse_favourites_html(html: &str, page: u32) -> TimelinePage {
    let link_re = regex::Regex::new(r#"weibo\.cn/comment/([0-9A-Za-z]+)\?[^"]*uid=(\d+)"#).unwrap();
    let tag_re = regex::Regex::new(r"<[^>]+>").unwrap();

    let mut posts = Vec::new();
    for chunk in html.split(r#"<div class="c""#).skip(1) {
        // 先丢掉开标签的剩余部分（id="M_xxx">），否则属性文本会混进正文（实测踩过）
        let chunk = chunk.split_once('>').map(|(_, rest)| rest).unwrap_or(chunk);
        let Some(caps) = link_re.captures(chunk) else { continue };
        let bid = caps[1].to_string();
        let author_uid: u64 = caps[2].parse().unwrap_or(0);
        // 正文：剥标签 + 解常见实体；截到"赞["统计尾巴之前，取前 30 字做摘要
        let text_end = chunk.find("赞[").unwrap_or(chunk.len());
        let text = {
            let raw = &chunk[..text_end];
            tag_re
                .replace_all(raw, "")
                .replace("&nbsp;", " ")
                .replace("&amp;", "&")
                .replace("&quot;", "\"")
                .replace("&#39;", "'")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        };
        if text.contains("不可见") {
            continue;
        }
        posts.push(PostData {
            bid: bid.clone(),
            mid: String::new(),
            text,
            created_at: 0,
            user_id: author_uid,
            screen_name: String::new(),
            pics: Vec::new(),
            videos: Vec::new(),
            audios: Vec::new(),
            is_audio: false,
            is_retweet: false,
            duration: 0,
        });
    }

    let has_next = html.contains(&format!("page={}", page + 1));
    TimelinePage {
        // wap 收藏按页翻：since_id 字段复用为"下一页页号"，无下一页链接即到底
        since_id: if has_next && !posts.is_empty() {
            (page + 1).to_string()
        } else {
            String::new()
        },
        total: 0,
        posts,
    }
}

/// 微博时间文本（`Wed Oct 01 12:34:56 +0800 2025`）→ Unix 秒。
/// 手写民用历转纪元日，不引 chrono。
pub fn parse_weibo_time(text: &str) -> i64 {
    let parts: Vec<&str> = text.split_whitespace().collect();
    if parts.len() < 6 {
        return 0;
    }
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let Some(mon) = months.iter().position(|m| *m == parts[1]) else {
        return 0;
    };
    let Ok(day) = parts[2].parse::<i64>() else { return 0 };
    let time_parts: Vec<&str> = parts[3].split(':').collect();
    if time_parts.len() < 3 {
        return 0;
    }
    let (Ok(h), Ok(mi), Ok(s)) = (
        time_parts[0].parse::<i64>(),
        time_parts[1].parse::<i64>(),
        time_parts[2].parse::<i64>(),
    ) else {
        return 0;
    };
    let Ok(year) = parts[5].parse::<i64>() else { return 0 };

    // 民用历 → 纪元日（days_from_civil，Howard Hinnant 算法）
    let y = if mon >= 2 { year } else { year - 1 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mon as i64 + 10) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    // 微博时间带 +0800 时区，直接按东八区折成 Unix 秒
    days * 86400 + h * 3600 + mi * 60 + s - 8 * 3600
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_weibo_time() {
        // 2025-10-01 12:00:00 +0800 = 04:00 UTC = 1759291200
        assert_eq!(parse_weibo_time("Wed Oct 01 12:00:00 +0800 2025"), 1759291200);
        assert_eq!(parse_weibo_time("garbage"), 0);
    }

    #[test]
    fn strips_html_and_entities() {
        assert_eq!(strip_html("<a  href='ht'>#话题#</a> 正文 &amp; 更多"), "#话题# 正文 & 更多");
    }

    #[test]
    fn parses_favourites_wap_html() {
        let html = r#"<div class="c" id="M1"><a>写书哥</a> 转发了 的微博:某条好内容 <a href="https://weibo.cn/comment/IrJeZw48z?uid=7235308961&amp;rl=0#cmtfrm">原文</a> 赞[3]&nbsp;原文转发[1]</div><div class="c" id="M2">某条已不可见的微博 <a href="https://weibo.cn/comment/XxXxXxX?uid=1">原文</a> 此微博已不可见。</div><div class="c" id="M3">另一条收藏 <a href="https://weibo.cn/comment/I43hOiPmz?uid=6626867192#cmtfrm">原文</a> 赞[0]</div><div id="pager"><a href="/fav/1?page=2">下页</a></div>"#;
        let page = parse_favourites_html(html, 1);
        // 不可见条目被过滤
        assert_eq!(page.posts.len(), 2, "不可见条目应被过滤");
        assert_eq!(page.posts[0].bid, "IrJeZw48z");
        assert_eq!(page.posts[0].user_id, 7235308961);
        assert!(page.posts[0].text.contains("某条好内容"), "{}", page.posts[0].text);
        assert!(!page.posts[0].text.contains("id="), "开标签属性不应混进正文: {}", page.posts[0].text);
        assert!(!page.posts[0].text.contains("M1"), "{}", page.posts[0].text);
        assert_eq!(page.posts[1].bid, "I43hOiPmz");
        // 有下页链接 → since_id = 2
        assert_eq!(page.since_id, "2");
    }
}
