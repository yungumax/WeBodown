//! 输入解析：把用户粘贴的各种微博链接/UID 归一成来源目标。

use crate::error::{Result, WeiboError};

/// 一条输入归一后的来源目标。
#[derive(Debug, Clone, PartialEq)]
pub enum SourceTarget {
    /// 单条微博（bid）
    Post(String),
    /// 博主主页（uid）
    User(u64),
    /// tv/show 音频/播客播放页（原样 URL，解析时抓内嵌音频直链）
    TvShow(String),
}

/// 解析一行输入。支持：
/// - `https://weibo.com/{uid}/{bid}` / `https://www.weibo.com/{uid}/{bid}`
/// - `https://m.weibo.cn/status/{bid}` / `…/statuses/show?id={bid}`
/// - `https://weibo.com/u/{uid}` / `https://m.weibo.cn/u/{uid}` / `…/profile/{uid}`
/// - 裸数字 UID、裸 9 位 base62 BID
/// - t.cn 短链（需要网络解析，交给上层 resolve）
pub fn parse_input(input: &str) -> Result<SourceTarget> {
    let text = input.trim();
    if text.is_empty() {
        return Err(WeiboError::InvalidInput("链接是空的".into()));
    }

    // t.cn 短链：无法离线判断目标，返回 User(0) 之外的哨兵——这里直接报错，
    // 上层（commands）遇到 t.cn 先 resolve_redirect 再回来调本函数。
    if text.contains("t.cn/") {
        return Err(WeiboError::InvalidInput("T_CN_SHORTLINK".into()));
    }

    // 提取路径与查询串；带协议的 URL 先跳过主机段（weibo.com/1234567/bid 里的域名）
    let has_scheme = text.contains("://");
    let path = text
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(text);
    let (path, query) = path
        .split_once('?')
        .map(|(p, q)| (p, Some(q)))
        .unwrap_or((path, None));
    let mut segments: Vec<&str> = path
        .split('#').next().unwrap_or(path)
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if has_scheme && segments.len() > 1 && segments[0].contains('.') {
        segments.remove(0);
    }

    // weibo.com/tv/show/{type}:{id}：音频/播客播放页（无重定向，解析时抓内嵌直链）。
    // 必须在 mid 转换之前判定——它的数字段不是 mid，先到先得（实测踩过）
    if let Some(i) = segments.iter().position(|seg| *seg == "tv") {
        if segments.get(i + 1).map(|s| *s == "show").unwrap_or(false) {
            let normalized = if has_scheme {
                format!("https://{}", path)
            } else {
                format!("https://weibo.com/{}", text.trim_start_matches('/'))
            };
            return Ok(SourceTarget::TvShow(normalized));
        }
    }

    // 纯数字 mid（16 位上下）：分段 base62 转成 bid。
    // 注意候选里要跳过带冒号的段（tv/show 的 {type}:{id} 不是 mid）
    let tv_mid = segments
        .iter()
        .filter(|s| !s.contains(':'))
        .find_map(|s| (is_digits(s) && s.len() >= 15).then_some(&**s));
    let tv_mid = tv_mid.or_else(|| {
        (text.len() >= 15 && text.len() <= 17 && is_digits(text)).then_some(&*text)
    });
    if let Some(mid) = tv_mid {
        if let Some(bid) = mid2bid(mid) {
            return Ok(SourceTarget::Post(bid));
        }
    }

    // 微博昵称链接（weibo.com/n/xxx / m.weibo.cn/n/xxx）：昵称要先经 302 跳转
    // 解析成 UID（m.weibo.cn/n/Violetxpter → /u/6458148211），交由上层 resolve。
    if let Some(i) = segments.iter().position(|s| *s == "n") {
        if segments.get(i + 1).is_some() {
            return Err(WeiboError::InvalidInput(format!(
                "NEED_RESOLVE:{text}"
            )));
        }
    }

    // m.weibo.cn/status/{bid} | m.weibo.cn/statuses/show?id={bid}
    if let Some(i) = segments.iter().position(|s| *s == "status" || *s == "statuses") {
        if let Some(bid) = segments.get(i + 1) {
            if *bid != "show" {
                return Ok(SourceTarget::Post((*bid).to_string()));
            }
        }
        // /statuses/show?id=xxx
        if segments.get(i + 1).map(|s| *s == "show").unwrap_or(true) {
            if let Some(q) = query {
                for pair in q.split('&') {
                    if let Some(bid) = pair.strip_prefix("id=") {
                        if !bid.is_empty() {
                            return Ok(SourceTarget::Post(bid.to_string()));
                        }
                    }
                }
            }
        }
    }
    // weibo.com/{uid}/{bid}
    if segments.len() >= 2 && is_digits(segments[0]) && looks_like_bid(segments[1]) {
        return Ok(SourceTarget::Post(segments[1].to_string()));
    }
    // /u/{uid} 或 /profile/{uid}
    for marker in ["u", "profile"] {
        if let Some(i) = segments.iter().position(|s| *s == marker) {
            if let Some(uid) = segments.get(i + 1).filter(|s| is_digits(s)) {
                return Ok(SourceTarget::User(uid.parse().unwrap()));
            }
        }
    }
    // 数字 UID（链接尾部或裸输入）
    if let Some(last) = segments.last() {
        if is_digits(last) && last.len() >= 5 && last.len() <= 12 {
            return Ok(SourceTarget::User(last.parse().unwrap()));
        }
    }
    if is_digits(text) && text.len() >= 5 && text.len() <= 12 {
        return Ok(SourceTarget::User(text.parse().unwrap()));
    }
    // 裸 BID（9 位 base62）
    if looks_like_bid(text) {
        return Ok(SourceTarget::Post(text.to_string()));
    }

    Err(WeiboError::InvalidInput(format!(
        "认不出这个链接：{}（支持单条微博、博主主页或数字 UID）",
        crate::error::truncate(text, 60)
    )))
}

fn is_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

const B62: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

fn u64_to_b62(mut n: u64, pad: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(B62[(n % 62) as usize]);
        n /= 62;
        if n == 0 {
            break;
        }
    }
    while out.len() < pad {
        out.push(b'0');
    }
    out.reverse();
    String::from_utf8(out).expect("b62 字符集是 ASCII")
}

/// 微博 mid → bid（标准分段 base62 转换）：
/// mid 按位切成 [0:2] / [2:9] / [9:16]，第一段直接转 base62（不补零），
/// 后两段补足 7 位十进制后各转成 4 位 base62 拼接。
/// 例：5349595318321922 ↔ RkOcryUUy（真实对照）。
pub fn mid2bid(mid: &str) -> Option<String> {
    if mid.len() < 10 || !is_digits(mid) {
        return None;
    }
    let (c1, c2, c3) = (
        &mid[..2.min(mid.len())],
        mid.get(2..9)?,
        mid.get(9..16.min(mid.len())).unwrap_or(""),
    );
    let n1: u64 = c1.parse().ok()?;
    let n2: u64 = c2.parse().ok()?;
    let n3: u64 = if c3.is_empty() { 0 } else { c3.parse().ok()? };
    Some(format!(
        "{}{}{}",
        u64_to_b62(n1, 1),
        u64_to_b62(n2, 4),
        u64_to_b62(n3, 4)
    ))
}

/// 微博 BID：9 位左右的 base62，至少含一个字母（避免与 UID 混淆）。
fn looks_like_bid(s: &str) -> bool {
    (8..=12).contains(&s.len())
        && s.bytes().all(|b| b.is_ascii_alphanumeric())
        && s.bytes().any(|b| b.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_links() {
        assert_eq!(
            parse_input("https://weibo.com/1234567/NbXxKq1aB").unwrap(),
            SourceTarget::Post("NbXxKq1aB".into())
        );
        assert_eq!(
            parse_input("https://m.weibo.cn/status/NbXxKq1aB").unwrap(),
            SourceTarget::Post("NbXxKq1aB".into())
        );
        assert_eq!(
            parse_input("https://m.weibo.cn/statuses/show?id=NbXxKq1aB").unwrap(),
            SourceTarget::Post("NbXxKq1aB".into())
        );
        assert_eq!(
            parse_input("https://weibo.com/u/7380874257").unwrap(),
            SourceTarget::User(7380874257)
        );
        assert_eq!(
            parse_input("https://m.weibo.cn/profile/7380874257").unwrap(),
            SourceTarget::User(7380874257)
        );
        assert_eq!(parse_input("7380874257").unwrap(), SourceTarget::User(7380874257));
        assert_eq!(parse_input("NbXxKq1aB").unwrap(), SourceTarget::Post("NbXxKq1aB".into()));
    }

    #[test]
    fn nick_links_need_resolve() {
        // /n/ 昵称链接：返回带 NEED_RESOLVE 哨兵的错误，由上层先做 302 解析
        let err = parse_input("https://weibo.com/n/Violetxpter").unwrap_err();
        assert!(err.to_string().contains("NEED_RESOLVE:"), "{err}");
        let err2 = parse_input("https://m.weibo.cn/n/某博主").unwrap_err();
        assert!(err2.to_string().contains("NEED_RESOLVE:"), "{err2}");
    }

    #[test]
    fn mid2bid_real_pairs() {
        // 2026-10 从真实时间线抓取的四组对照
        assert_eq!(mid2bid("5349595318321922").as_deref(), Some("RkOcryUUy"));
        assert_eq!(mid2bid("5349584641720467").as_deref(), Some("RkNVe7dzt"));
        assert_eq!(mid2bid("5349584630972467").as_deref(), Some("RkNVd44YX"));
        assert_eq!(mid2bid("5349584400550773").as_deref(), Some("RkNUQ2jhr"));
        assert!(mid2bid("abc").is_none());
    }

    #[test]
    fn tv_show_links_become_tvshow_target() {
        // tv/show 是播客播放页（数字段不是 mid），应返回 TvShow 由上层抓内嵌直链
        match parse_input(
            "https://weibo.com/tv/show/2373717:5349605416828946?from=old_pc_videoshow",
        )
        .unwrap()
        {
            SourceTarget::TvShow(url) => {
                assert!(url.contains("/tv/show/2373717:5349605416828946"), "{url}");
            }
            other => panic!("应为 TvShow，实际 {other:?}"),
        }
        // 纯 16 位数字 mid 仍然走转换
        assert_eq!(
            parse_input("5349595318321922").unwrap(),
            SourceTarget::Post(mid2bid("5349595318321922").unwrap())
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_input("hello world").is_err());
        assert!(parse_input("").is_err());
    }
}
