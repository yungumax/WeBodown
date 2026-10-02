//! 下载引擎：探测长度 → 流式落盘，带进度回调与重试。
//!
//! 微博的媒体（图片/视频）都是单文件直链，量小图多，
//! 用"每文件一条流 + 定期汇报"就够，不需要 B 站那种大文件分片矩阵。

use crate::error::{Result, WeiboError};
use futures::StreamExt;
use reqwest::Client;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub struct Progress {
    pub downloaded: u64,
    pub total: u64,
    /// 平均速率（字节/秒）
    pub speed_bps: f64,
}

pub type ProgressFn = Arc<dyn Fn(Progress) + Send + Sync>;

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub retries: usize,
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self { retries: 3 }
    }
}

/// 下载一个文件到 `dest`，进度回调带累计字节数。
pub async fn download(
    http: &Client,
    url: &str,
    dest: &Path,
    opts: &DownloadOptions,
    progress: ProgressFn,
) -> Result<u64> {
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            tokio::fs::create_dir_all(parent).await?;
        }
    }

    let mut delay = Duration::from_millis(500);
    let mut last_err = WeiboError::Unavailable("未知错误".into());

    for attempt in 0..=opts.retries {
        if attempt > 0 {
            tokio::time::sleep(delay).await;
            delay *= 2;
        }
        match download_once(http, url, dest, &progress).await {
            Ok(size) => {
                // 终值：让调用方拿到 100%
                progress(Progress { downloaded: size, total: size, speed_bps: 0.0 });
                return Ok(size);
            }
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

async fn download_once(
    http: &Client,
    url: &str,
    dest: &Path,
    progress: &ProgressFn,
) -> Result<u64> {
    let resp = http.get(url).send().await?;
    let status = resp.status();
    if !status.is_success() {
        return Err(WeiboError::Unavailable(format!("下载失败 HTTP {status}")));
    }
    let total = resp.content_length().unwrap_or(0);

    let downloaded = Arc::new(AtomicU64::new(0));
    let reporter = spawn_reporter(downloaded.clone(), total, progress.clone());

    let mut file = tokio::fs::File::create(dest).await?;
    let mut stream = resp.bytes_stream();
    let mut last_flush = Instant::now();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk).await?;
        downloaded.fetch_add(chunk.len() as u64, Ordering::Relaxed);
        // 图片很小，没必要每块都 flush
        if last_flush.elapsed() > Duration::from_millis(300) {
            tokio::io::AsyncWriteExt::flush(&mut file).await?;
            last_flush = Instant::now();
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file).await?;

    reporter.abort();
    let size = downloaded.load(Ordering::Relaxed);
    if total > 0 && size != total {
        return Err(WeiboError::IncompleteDownload { expected: total, actual: size });
    }
    Ok(size)
}

fn spawn_reporter(
    downloaded: Arc<AtomicU64>,
    total: u64,
    progress: ProgressFn,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let start = Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let now = downloaded.load(Ordering::Relaxed);
            let elapsed = start.elapsed().as_secs_f64();
            progress(Progress {
                downloaded: now,
                total,
                speed_bps: if elapsed > 0.0 { now as f64 / elapsed } else { 0.0 },
            });
        }
    })
}

/// 按图片规格调整 sinaimg 地址：`/large/` 是大图，`/thumb360/` 是压缩图。
pub fn image_variant(url: &str, spec: &str) -> String {
    if spec == "thumbnail" {
        url.replace("/large/", "/thumb360/").replace("/orj1080/", "/thumb360/")
    } else {
        // 原图：large 已是最大规格（gif 的 large 即原图）
        url.replace("/thumb360/", "/large/").replace("/orj1080/", "/large/")
    }
}
