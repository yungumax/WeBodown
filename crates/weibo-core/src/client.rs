//! HTTP 会话层：移动端 UA 伪装、Cookie 管理、通用 JSON 请求。
//!
//! m.weibo.cn 的接口要移动端 UA 与 `X-Requested-With`，否则会被风控页拦下；
//! 图片与视频 CDN（sinaimg.cn / miaopai 等）不校验 Referer，但带上也无妨。

use crate::error::{Result, WeiboError};
use reqwest::cookie::{CookieStore, Jar};
use reqwest::header::{HeaderMap, HeaderValue, ACCEPT_LANGUAGE, REFERER, USER_AGENT};
use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

/// 按域分桶的 Cookie：m.weibo.cn 与 weibo.com 的登录凭据是两套不同值。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DomainCookies {
    pub m: Vec<(String, String)>,
    pub pc: Vec<(String, String)>,
}

impl DomainCookies {
    pub fn is_empty(&self) -> bool {
        self.m.is_empty() && self.pc.is_empty()
    }

    /// 两桶合并的名字去重清单（存在性判断用）。
    pub fn merged(&self) -> Vec<(String, String)> {
        let mut out = self.m.clone();
        for (name, value) in &self.pc {
            if !out.iter().any(|(n, _): &(String, String)| n == name) {
                out.push((name.clone(), value.clone()));
            }
        }
        out
    }
}

/// 移动端 Safari UA：m.weibo.cn 的接口按移动端预期服务。
pub const UA: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";
/// 桌面 Chrome UA：weibo.com ajax 接口（视频 1080P 档位只在 PC 域给出）。
pub const PC_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";
pub const M_REFERER: &str = "https://m.weibo.cn/";
pub const PC_REFERER: &str = "https://weibo.com/";

/// 读取会话 Cookie 时探测的站点。

pub struct WeiboClient {
    pub http: Client,
    jar: Arc<Jar>,
}

impl WeiboClient {
    pub fn new() -> Result<Self> {
        Self::build(None, false)
    }

    /// 走指定代理（形如 `http://127.0.0.1:7890`）；传空则直连。
    /// 会话客户端：Cookie 罐只当存储，请求按 export_cookies 显式组头发送
    /// （与 curl 实测登录校验通过的行为一致；罐子的自动发送曾把访客 SUB
    /// 与登录 SUB 一起带给 m.weibo.cn 被判未登录）。
    pub fn with_proxy(proxy: Option<&str>) -> Result<Self> {
        Self::build(proxy.filter(|value| !value.trim().is_empty()), false)
    }

    /// 登录链客户端：带 cookie_provider，Set-Cookie 自动入罐、跨域跳转
    /// 自动带上前一步的 Cookie——扫码确认的 SSO 跳转链需要这个。
    /// 链走完后把罐里的 Cookie 导出，装到会话客户端上。
    pub fn with_cookie_jar(proxy: Option<&str>) -> Result<Self> {
        Self::build(proxy.filter(|value| !value.trim().is_empty()), true)
    }

    fn build(proxy: Option<&str>, auto_cookie: bool) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(USER_AGENT, HeaderValue::from_static(UA));
        headers.insert(REFERER, HeaderValue::from_static(M_REFERER));
        headers.insert("X-Requested-With", HeaderValue::from_static("XMLHttpRequest"));
        headers.insert(
            ACCEPT_LANGUAGE,
            HeaderValue::from_static("zh-CN,zh;q=0.9,en;q=0.8"),
        );

        let jar = Arc::new(Jar::default());
        let mut builder = Client::builder()
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(180));
        if auto_cookie {
            builder = builder.cookie_provider(jar.clone());
        }

        if let Some(addr) = proxy {
            let proxy = reqwest::Proxy::all(addr.trim())
                .map_err(|e| WeiboError::InvalidInput(format!("代理地址无效: {e}")))?;
            builder = builder.proxy(proxy);
        }

        Ok(Self {
            http: builder.build()?,
            jar,
        })
    }

    /// 向会话中写入一条 Cookie，后续请求自动携带。
    pub fn add_cookie(&self, cookie: &str, url: &Url) {
        self.jar.add_cookie_str(cookie, url);
    }

    /// 导出某个域的 Cookie（k=v 形式）。
    pub fn export_cookies_for(&self, origin: &str) -> Vec<(String, String)> {
        let Ok(url) = Url::parse(origin) else { return Vec::new() };
        let Some(header) = self.jar.cookies(&url) else { return Vec::new() };
        let Ok(text) = header.to_str() else { return Vec::new() };
        let mut out = Vec::new();
        for pair in text.split(';') {
            if let Some((name, value)) = pair.trim().split_once('=') {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    out.push((name, value.trim().to_string()));
                }
            }
        }
        out
    }

    /// 按域导出全部 Cookie：m.weibo.cn 与 weibo.com 两域的登录凭据是**两套不同值**，
    /// 必须分桶保存/恢复（按名合并会互相覆盖，装错域就是无效登录态——实测踩过）。
    pub fn export_domains(&self) -> DomainCookies {
        DomainCookies {
            m: self.export_cookies_for("https://m.weibo.cn/"),
            pc: self
                .export_cookies_for("https://weibo.com/")
                .into_iter()
                .chain(self.export_cookies_for("https://passport.weibo.com/"))
                .collect(),
        }
    }

    /// 兼容旧调用：两域合并的名字去重清单（仅用于"有没有 SUB"这类存在性判断）。
    pub fn export_cookies(&self) -> Vec<(String, String)> {
        let domains = self.export_domains();
        let mut out = domains.m;
        for (name, value) in domains.pc {
            if !out.iter().any(|(n, _): &(String, String)| n == &name) {
                out.push((name, value));
            }
        }
        out
    }

    /// 按域恢复 Cookie。
    pub fn install_cookies_for(&self, origin: &str, cookies: &[(String, String)]) {
        let Ok(url) = Url::parse(origin) else { return };
        for (name, value) in cookies {
            self.jar
                .add_cookie_str(&format!("{name}={value}"), &url);
        }
    }

    /// 把按域分桶的 Cookie 装回会话。
    /// m 桶同时装到 m.weibo.cn 与 weibo.cn（经典 wap 收藏页在 weibo.cn 上，
    /// 两个主机都要能读到，实测漏装 weibo.cn 会让 wap 收藏页当未登录）。
    pub fn install_domains(&self, cookies: &DomainCookies) {
        for origin in ["https://m.weibo.cn/", "https://weibo.cn/"] {
            self.install_cookies_for(origin, &cookies.m);
        }
        self.install_cookies_for("https://weibo.com/", &cookies.pc);
    }

    /// 把持久化的 Cookie 装回会话（旧接口：同一组装两个域，仅用于非登录态场景）。
    pub fn install_cookies(&self, cookies: &[(String, String)]) {
        self.install_cookies_for("https://weibo.com/", cookies);
    }

    /// 按请求 URL 的域取 Cookie 头（与浏览器一致：m.weibo.cn 请求带 m 域的 SUB，
    /// weibo.com 请求带 pc 域的 SUB）。罐里没有对应 Cookie 时返回 None。
    fn cookie_header_for(&self, url: &Url) -> Option<String> {
        let header = self.jar.cookies(url)?;
        let text = header.to_str().ok()?;
        (!text.is_empty()).then(|| text.to_string())
    }

    /// 访问 m.weibo.cn 首页拿到访客 Cookie，可显著降低接口被拦的概率。
    pub async fn warmup(&self) -> Result<()> {
        let resp = self.http.get(M_REFERER).send().await?;
        let _ = resp.bytes().await;
        Ok(())
    }

    /// 访客身份（免登录浏览公开微博）：
    /// genvisitor 领 tid → incarnate 换 sub/subp → 手动种到 .weibo.cn/.weibo.com。
    /// m.weibo.cn 的容器接口没有这组 Cookie 会直接 432。
    pub async fn ensure_visitor(&self) -> Result<()> {
        if self.export_cookies().iter().any(|(name, _)| name == "SUB") {
            return Ok(());
        }
        // fp 用固定的移动端指纹（与 curl 实测一致），避免引 URL 编码依赖
        let body = "cb=gen_callback&fp=%7B%22os%22%3A%222%22%2C%22browser%22%3A%22Safari17%2C5%22%2C%22fonts%22%3A%5B%22undefined%22%5D%2C%22screenInfo%22%3A%22390*844*30%22%2C%22plugins%22%3A%5B%5D%7D";
        let text = self
            .http
            .post("https://passport.weibo.com/visitor/genvisitor")
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await?
            .text()
            .await?;
        let Some(value) = crate::login::parse_jsonp(&text) else {
            return Err(WeiboError::Decode("genvisitor 返回的不是 JSONP".into()));
        };
        if value.get("retcode").and_then(|v| v.as_i64()) != Some(20000000) {
            return Err(WeiboError::Api("genvisitor 未发放访客身份".into()));
        }
        let tid = value
            .pointer("/data/tid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if tid.is_empty() {
            return Err(WeiboError::Api("genvisitor 没给 tid".into()));
        }

        let incarnate = format!(
            "https://passport.weibo.com/visitor/visitor?a=incarnate&t={tid}&w=2&c=100&gc=&cb=cross_domain&from=weibo"
        );
        let text = self.http.get(&incarnate).send().await?.text().await?;
        let Some(value) = crate::login::parse_jsonp(&text) else {
            return Err(WeiboError::Decode("incarnate 返回的不是 JSONP".into()));
        };
        if value.get("retcode").and_then(|v| v.as_i64()) != Some(20000000) {
            return Err(WeiboError::Api("incarnate 未签发访客 Cookie".into()));
        }
        let sub = value.pointer("/data/sub").and_then(|v| v.as_str()).unwrap_or_default();
        let subp = value.pointer("/data/subp").and_then(|v| v.as_str()).unwrap_or_default();
        if sub.is_empty() {
            return Err(WeiboError::Api("incarnate 没给 sub".into()));
        }

        // 手动种到两个域（incarnate 的 crossdomain 靠 JS，我们自己落地）
        for origin in ["https://weibo.com/", "https://m.weibo.cn/"] {
            let url: Url = origin.parse().unwrap();
            self.add_cookie(&format!("SUB={sub}"), &url);
            if !subp.is_empty() {
                self.add_cookie(&format!("SUBP={subp}"), &url);
            }
        }
        Ok(())
    }

    /// 跟随重定向拿到最终地址（用于 t.cn 短链与 weibo.com 桌面链接）。
    pub async fn resolve_redirect(&self, url: &str) -> Result<String> {
        let resp = self.http.get(url).send().await?;
        Ok(resp.url().to_string())
    }

    /// 请求 JSON 并检查 m.weibo.cn 的 `ok` 信封；ok != 1 时给出 msg。
    /// 撞上 432 风控时自动补一次访客身份再重试。
    pub async fn fetch_json(&self, url: &str) -> Result<Value> {
        match self.fetch_json_once(url).await {
            Err(WeiboError::Unavailable(e)) if e.contains("HTTP 432") => {
                self.ensure_visitor().await?;
                self.fetch_json_once(url).await
            }
            other => other,
        }
    }

    async fn fetch_json_once(&self, url: &str) -> Result<Value> {
        let mut req = self.http.get(url);
        if let Ok(parsed) = Url::parse(url) {
            if let Some(cookies) = self.cookie_header_for(&parsed) {
                req = req.header(reqwest::header::COOKIE, cookies);
            }
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(WeiboError::Unavailable(format!(
                "HTTP {status}: {}",
                crate::error::truncate(&text, 200)
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| WeiboError::Decode(format!("{e}；响应片段: {}", crate::error::truncate(&text, 300))))?;
        let ok = value.get("ok").and_then(|v| v.as_i64()).unwrap_or(-1);
        if ok == 1 {
            Ok(value)
        } else if ok == -100 {
            // 登录墙：微博只给一个 signin 跳转地址
            Err(WeiboError::NeedLogin(
                "此内容需要登录后查看（微博未开放匿名访问）".into(),
            ))
        } else {
            let msg = value
                .get("msg")
                .or_else(|| value.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("接口未给出原因");
            if text.contains("未登录") || msg.contains("登录") {
                return Err(WeiboError::NeedLogin(msg.to_string()));
            }
            Err(WeiboError::Api(format!(
                "{msg}；请求 {}",
                crate::error::truncate(url, 160)
            )))
        }
    }

    /// PC 域 ajax 请求（weibo.com/ajax/*）：桌面 UA + weibo.com Cookie。
    /// 视频的 1080P/原画档位只在 PC 接口给出（移动端 playback_list 常为空）。
    pub async fn fetch_json_pc(&self, url: &str) -> Result<Value> {
        let pc_url = Url::parse("https://weibo.com/").unwrap();
        let mut req = self
            .http
            .get(url)
            .header(USER_AGENT, HeaderValue::from_static(PC_UA))
            .header(REFERER, HeaderValue::from_static(PC_REFERER))
            .header("X-Requested-With", HeaderValue::from_static("XMLHttpRequest"));
        if let Some(cookies) = self.cookie_header_for(&pc_url) {
            req = req.header(reqwest::header::COOKIE, cookies);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(WeiboError::Unavailable(format!(
                "HTTP {status}: {}",
                crate::error::truncate(&text, 200)
            )));
        }
        let value: Value = serde_json::from_str(&text)
            .map_err(|e| WeiboError::Decode(format!("{e}；响应片段: {}", crate::error::truncate(&text, 300))))?;
        let ok = value.get("ok").and_then(|v| v.as_i64()).unwrap_or(-1);
        if ok == 1 {
            Ok(value)
        } else if ok == -100 {
            Err(WeiboError::NeedLogin("PC 接口要求登录".into()))
        } else {
            Err(WeiboError::Api(format!(
                "PC 接口未给出数据；请求 {}",
                crate::error::truncate(url, 160)
            )))
        }
    }

    /// 取一页文本（跨域登录的 HTML 里要抽链接）。
    pub async fn fetch_text(&self, url: &str, referer: &str) -> Result<String> {
        let mut req = self.http.get(url).header(REFERER, referer);
        if let Ok(parsed) = Url::parse(url) {
            if let Some(cookies) = self.cookie_header_for(&parsed) {
                req = req.header(reqwest::header::COOKIE, cookies);
            }
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(WeiboError::Unavailable(format!(
                "HTTP {status}: {}",
                crate::error::truncate(&text, 200)
            )));
        }
        Ok(text)
    }

    /// 取一页文本（PC 域：桌面 UA + weibo.com Cookie——tv/show 播客页需要）。
    pub async fn fetch_text_pc(&self, url: &str, referer: &str) -> Result<String> {
        let mut req = self
            .http
            .get(url)
            .header(USER_AGENT, HeaderValue::from_static(PC_UA))
            .header(REFERER, referer);
        if let Some(cookies) = self.cookie_header_for(&Url::parse("https://weibo.com/").unwrap()) {
            req = req.header(reqwest::header::COOKIE, cookies);
        }
        let resp = req.send().await?;
        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(WeiboError::Unavailable(format!(
                "HTTP {status}: {}",
                crate::error::truncate(&text, 200)
            )));
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 安装的 Cookie 必须能被 m.weibo.cn 与 weibo.com 两个域读到（登录态恢复的关键）。
    #[test]
    fn installed_cookies_reach_both_domains() {
        let client = WeiboClient::new().unwrap();
        client.install_domains(&crate::client::DomainCookies {
            m: vec![("SUB".to_string(), "m_value".to_string())],
            pc: vec![("SUB".to_string(), "pc_value".to_string())],
        });
        let m = Url::parse("https://m.weibo.cn/").unwrap();
        let m_header = client
            .jar
            .cookies(&m)
            .and_then(|value| value.to_str().ok().map(str::to_string))
            .unwrap_or_default();
        assert!(m_header.contains("SUB=m_value"), "m 域应收到 m 域 SUB: {m_header}");
        let pc = Url::parse("https://weibo.com/").unwrap();
        let pc_header = client
            .jar
            .cookies(&pc)
            .and_then(|value| value.to_str().ok().map(str::to_string))
            .unwrap_or_default();
        assert!(pc_header.contains("SUB=pc_value"), "pc 域应收到 pc 域 SUB: {pc_header}");
    }
}
