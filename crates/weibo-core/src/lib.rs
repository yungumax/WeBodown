//! WeBodown 核心库：m.weibo.cn 会话与接口、扫码登录、来源解析、下载引擎。

pub mod api;
pub mod client;
pub mod download;
pub mod error;
pub mod login;
pub mod parser;

pub use client::WeiboClient;
pub use error::{Result, WeiboError};
