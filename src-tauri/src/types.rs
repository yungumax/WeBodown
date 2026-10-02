//! 与前端交换的数据结构。字段名一律 camelCase，和 api.js 里的键逐一对齐。

use serde::{Deserialize, Serialize};

/// 宽容字段反序列化：null/数字一律收成字符串（前端对空值会兜底成 `0`，
/// 而若干标识字段是 String——严格类型会让整条请求被 Tauri 拒绝，实测踩过）。
pub mod lenient {
    use serde::{Deserialize, Deserializer};
    use serde_json::Value;

    /// null / 数字 / 字符串 → 字符串
    pub fn deserialize<'de, D>(deserializer: D) -> Result<String, D::Error>
    where
        D: Deserializer<'de>,
    {
        let v = Value::deserialize(deserializer)?;
        Ok(match v {
            Value::String(s) => s,
            Value::Number(n) => n.to_string(),
            Value::Null => String::new(),
            other => other.to_string(),
        })
    }

}


#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LoginInfo {
    pub logged_in: bool,
    pub uname: String,
    pub face: String,
    pub mid: u64,
    pub vip: bool,
    pub vip_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct QualityOption {
    pub value: String,
    pub label: String,
    pub available: bool,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PostEntry {
    pub bid: String,
    pub mid: String,
    pub title: String,
    pub author: String,
    pub created_at: i64,
    pub pics: u32,
    pub has_video: bool,
    pub is_audio: bool,
    /// tv/show 播客等：解析时已抓到的直链（下载任务直接用，不再查详情）
    pub media_url: String,
    pub duration: i64,
    pub is_retweet: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Probe {
    pub kind: String, // post | user | favorite
    pub bid: String,
    pub mid: String,
    pub title: String,
    pub author: String,
    pub uid: u64,
    pub cover: String,
    pub note: String,
    pub total: usize,
    pub loaded: usize,
    pub exhausted: bool,
    pub qualities: Vec<QualityOption>,
    pub items: Vec<PostEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProbeMore {
    pub items: Vec<PostEntry>,
    pub loaded: usize,
    pub total: usize,
    pub exhausted: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct NamingVars {
    pub author: String,
    pub uid: u64,
    #[serde(deserialize_with = "lenient::deserialize")]
    pub bid: String,
    #[serde(deserialize_with = "lenient::deserialize")]
    pub mid: String,
    pub publish_date: String,
    pub date: String,
    pub year: String,
    pub month: String,
    pub source_kind: String,
    pub index: u32,
    pub index_pad: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DownloadReq {
    pub bid: String,
    pub mid: String,
    pub source: String,
    pub title: String,
    pub author: String,
    pub uid: u64,
    pub quality: String,
    pub image: String,
    /// tv/show 播客等：解析时抓到的媒体直链（非空 = 直接下载，跳过详情）
    pub media_url: String,
    /// 播放页地址（文案里引用）
    pub page_url: String,
    /// 音质：best（最佳）/ standard（标准）
    pub audio_quality: String,
    pub is_audio: bool,
    pub image_count: u32,
    pub video_count: u32,
    pub quality_label: String,
    pub naming: NamingVars,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TaskUpdate {
    pub id: String,
    pub title: String,
    pub quality_label: String,
    pub status: String, // queued | downloading | saving | done | failed | canceled
    pub image_pct: f64,
    pub video_pct: f64,
    pub image_count: u32,
    pub video_count: u32,
    /// 音频帖：进度段与阶段名显示「音频」而非「视频」
    pub is_audio: bool,
    pub downloaded: u64,
    pub total: u64,
    pub speed_bps: f64,
    pub output_path: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct AppStatus {
    pub version: String,
    pub login: LoginInfo,
    pub output_dir: String,
    pub cookies_path: String,
}

#[derive(Debug, Clone, Serialize)]

pub struct SettingsEnv {
    pub settings: crate::state::Settings,
    pub cookies_path: String,
    pub cookies_saved: bool,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct NamingVar {
    pub token: String,
    pub label: String,
    pub section: String,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct FavItem {
    pub id: u64,
    pub bid: String,
    pub title: String,
    pub created_at: i64,
    pub pics: u32,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct FollowItem {
    pub id: u64,
    pub title: String,
    pub media_count: u32,
    pub owner: String,
    pub owner_mid: u64,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LibraryFolders {
    pub mid: u64,
    pub created: Vec<FavItem>,
    pub subscribed: Vec<FollowItem>,
    /// 平台限制提示：微博接口只开放最近的收藏/关注时给出说明（无限制则空串）
    pub fav_note: String,
    pub follow_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QrCode {
    pub url: String,
    /// 注意保持 snake_case：前端 api.js 读 `qr.qrcode_key`
    pub qrcode_key: String,
}

/// 更新检测结果（前端设置页「应用更新」用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateCheck {
    pub current: String,
    pub latest: String,
    pub up_to_date: bool,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct PollResult {
    pub state: String, // pending | scanned | confirmed | expired
    pub login: LoginInfo,
}
