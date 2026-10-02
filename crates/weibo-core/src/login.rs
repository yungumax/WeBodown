//! 扫码登录：passport.weibo.com SSO v2 二维码（申请 → 轮询 → 跨域落地 Cookie）。
//!
//! 流程（2026-10 实测，旧 login.sina.com.cn 接口已返回空 body）：
//! 1. GET `sso/v2/qrcode/image?entry=weibo&size=180` → qrid + 现成二维码图片地址；
//! 2. GET `sso/v2/qrcode/check?qrid=…` 轮询：50114001 未使用 / 50114015 已扫码 /
//!    50114002·50114017 过期 / 20000000 确认（data 带跨域登录地址）；
//! 3. 访问跨域地址把 Cookie 落到各域（Set-Cookie 自动入罐；JSON sub/subp 手动种）。

use crate::client::WeiboClient;
use crate::error::{Result, WeiboError};
use regex::Regex;

/// 一次二维码会话。
#[derive(Debug, Clone)]
pub struct QrSession {
    pub qrid: String,
    /// 二维码**图片**地址（https，直接 <img> 显示；内容已编码扫码地址）
    pub image_url: String,
}

/// 轮询结果。
#[derive(Debug, Clone, PartialEq)]
pub enum QrState {
    Pending,
    Scanned,
    Confirmed,
    Expired,
}

/// 登录后的账号信息。
#[derive(Debug, Clone, Default)]
pub struct LoginInfo {
    pub logged_in: bool,
    pub uid: u64,
    pub uname: String,
    pub face: String,
}

impl WeiboClient {
    /// 申请二维码（SSO v2）。
    pub async fn qr_create(&self) -> Result<QrSession> {
        let url = "https://passport.weibo.com/sso/v2/qrcode/image?entry=weibo&size=180";
        let text = self.fetch_text(url, "https://passport.weibo.com/").await?;
        let value = crate::login::parse_jsonp(&text)
            .ok_or_else(|| WeiboError::Decode("二维码接口返回的不是 JSON".into()))?;
        if value.get("retcode").and_then(|v| v.as_i64()) != Some(20000000) {
            return Err(WeiboError::Api(
                value
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("二维码申请失败")
                    .into(),
            ));
        }
        let qrid = value
            .pointer("/data/qrid")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let image = value
            .pointer("/data/image")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if qrid.is_empty() || image.is_empty() {
            return Err(WeiboError::Api("二维码接口没给 qrid/图片".into()));
        }
        // 图片地址是协议相对的（//v2.qr.weibo.cn/...），补 https
        let image_url = if image.starts_with("//") {
            format!("https:{image}")
        } else {
            image
        };
        Ok(QrSession { qrid, image_url })
    }

    /// 轮询二维码状态。返回 (状态, 跨域登录地址)。
    /// 跨域落地由调用方在**全新会话**上执行——登录前的访客 SUB 会与真登录 SUB
    /// 同域共存，两个 SUB 一起发会让 m.weibo.cn 判为未登录。
    /// 所有原始响应都追加写入 qrcode_debug.log（数据目录），风控码对不上时能精确定位。
    pub async fn qr_poll(&self, qrid: &str) -> Result<(QrState, Option<String>)> {
        let url = format!(
            "https://passport.weibo.com/sso/v2/qrcode/check?qrid={qrid}&entry=weibo"
        );
        let text = self
            .fetch_text(&url, "https://passport.weibo.com/")
            .await?;
        debug_log(&format!("check qrid={qrid} → {text}"));
        let Some(value) = crate::login::parse_jsonp(&text) else {
            return Ok((QrState::Pending, None));
        };
        let code = value.get("retcode").and_then(|v| v.as_i64()).unwrap_or(-1);
        match code {
            20000000 => {
                // 确认后的跨域地址：字段名在 v2 里可能是 url / crossDomainUrl / data
                let alt = ["url", "crossDomainUrl", "data", "redirect"]
                    .iter()
                    .find_map(|key| {
                        value.pointer(&format!("/data/{key}")).and_then(|v| v.as_str())
                    })
                    .unwrap_or_default()
                    .to_string();
                debug_log(&format!("confirmed，跨域地址={alt}"));
                Ok((
                    QrState::Confirmed,
                    (!alt.is_empty()).then_some(alt),
                ))
            }
            _ => {
                // v2 的状态码与旧接口不同义（50114002 是"已扫描"不是"过期"），
                // 以 msg 语义为准：扫码/确认 → Scanned，过期 → Expired，其余 → Pending
                let msg = value
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();
                if msg.contains("扫描") || msg.contains("确认") {
                    Ok((QrState::Scanned, None))
                } else if msg.contains("过期") {
                    Ok((QrState::Expired, None))
                } else {
                    Ok((QrState::Pending, None))
                }
            }
        }
    }

    /// 访问跨域地址把登录 Cookie 落到各域。
    ///
    /// 兼容三种返回形态：Set-Cookie（jar 自动收）、JSON data.sub/subp（手动种）、
    /// 旧式页面里一串 /sso/crossdomain 链接（逐个访问）。
    pub async fn crossdomain_login(&self, alt: &str) -> Result<()> {
        let page = self.fetch_text(alt, "https://passport.weibo.com/").await?;
        debug_log(&format!(
            "alt={alt}\n  响应前 600 字：{}",
            crate::error::truncate(&page, 600)
        ));

        // 形态一：JSON 带 sub/subp → 学访客流手动种
        if let Some(value) = crate::login::parse_jsonp(&page) {
            let sub = value.pointer("/data/sub").and_then(|v| v.as_str());
            let subp = value.pointer("/data/subp").and_then(|v| v.as_str());
            if let Some(sub) = sub.filter(|s| !s.is_empty()) {
                for origin in ["https://weibo.com/", "https://m.weibo.cn/"] {
                    let url: reqwest::Url = origin.parse().unwrap();
                    self.add_cookie(&format!("SUB={sub}"), &url);
                    if let Some(subp) = subp {
                        self.add_cookie(&format!("SUBP={subp}"), &url);
                    }
                }
                debug_log("已按 JSON sub/subp 手动种 Cookie");
                return Ok(());
            }
        }

        // 形态二：旧式页面，正则抓全部 crossdomain 链接逐个落地
        let re = Regex::new(r#"https?://[^"'\\\s]+/sso/crossdomain[^"'\\\s]*"#).unwrap();
        let mut hits = re
            .find_iter(&page)
            .map(|m| m.as_str().to_string())
            .collect::<Vec<_>>();
        hits.dedup();
        if !hits.is_empty() {
            debug_log(&format!("发现 {} 个 crossdomain 链接，逐个落地", hits.len()));
            for url in hits.iter().take(6) {
                let _ = self.fetch_text(url, "https://passport.weibo.com/").await;
            }
            return Ok(());
        }

        debug_log("alt 无 JSON sub、无 crossdomain 链接（Set-Cookie 已由 jar 收取）");
        Ok(())
    }
}

/// JSONP 文本 → JSON Value（剥掉 STK_xxx( … ) 外壳；纯 JSON 也认）。
pub(crate) fn parse_jsonp(text: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str(text) {
        return Some(value);
    }
    let start = text.find('(')?;
    let end = text.rfind(')')?;
    if end <= start {
        return None;
    }
    serde_json::from_str(&text[start + 1..end]).ok()
}

/// 扫码调试日志：%APPDATA%\webodown\qrcode_debug.log（无 APPDATA 则退到临时目录）。
pub fn debug_log(line: &str) {
    use std::io::Write;
    let path = std::env::var("APPDATA")
        .map(|a| std::path::PathBuf::from(a).join("webodown").join("qrcode_debug.log"))
        .unwrap_or_else(|_| std::env::temp_dir().join("webodown_qrcode_debug.log"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "[{ts}] {line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_jsonp() {
        let v = parse_jsonp(r#"STK_123({"retcode":20000000,"data":{"qrid":"abc"}})"#).unwrap();
        assert_eq!(v.pointer("/data/qrid").unwrap().as_str().unwrap(), "abc");
        // 纯 JSON 也要认
        let v2 = parse_jsonp(r#"{"retcode":20000000}"#).unwrap();
        assert_eq!(v2.get("retcode").unwrap().as_i64().unwrap(), 20000000);
    }
}
