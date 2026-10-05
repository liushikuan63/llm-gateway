//! 媒体承载能力：判断某方言能否真正把请求里的图片/音频/视频送到上游。
//!
//! 「模型声明支持视觉」和「这条链路能承载这张图」是两件事。最典型的例子是
//! Gemini 原生接口：它只接受 base64 内联数据或 Google 系文件 URI，不接受任意
//! http(s) 图片地址。若不在这里拦住，转换层只能把图片丢掉——那等于静默篡改
//! 用户请求。因此承载判断参与候选过滤：没有任何候选能承载时直接给出可操作的
//! 错误，而不是发出一条被偷偷裁剪过的请求。

use crate::domain::{ChatRequest, Content, Dialect, Part};

/// 请求中出现的媒体形态。`remote_*` 表示使用了 http(s) 远程地址
/// （而非 data: 内联数据），因为部分方言只支持后者。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Media {
    pub image: bool,
    pub audio: bool,
    pub video: bool,
    pub remote_image: bool,
    pub remote_audio: bool,
    pub remote_video: bool,
    /// 存在 Gemini 也不接受的远程视频来源（非 YouTube / Google 存储）。该字段与
    /// 方言无关，只是记录「这个视频地址无法内联成 file_data」这一客观事实。
    pub exotic_remote_video: bool,
}

impl Media {
    pub fn any(&self) -> bool {
        self.image || self.audio || self.video
    }

    /// 从（重建后的）完整上下文里提取媒体需求。
    pub fn of(req: &ChatRequest) -> Self {
        let mut media = Media::default();
        for message in &req.messages {
            let Content::Parts(parts) = &message.content else {
                continue;
            };
            for part in parts {
                match part {
                    Part::Text { .. } => {}
                    Part::ImageUrl { image_url } => {
                        media.image = true;
                        media.remote_image |= is_remote(&image_url.url);
                    }
                    Part::InputAudio { input_audio } => {
                        media.audio = true;
                        media.remote_audio |= audio_is_remote(input_audio);
                    }
                    Part::VideoUrl { video_url } => {
                        media.video = true;
                        let remote = is_remote(&video_url.url);
                        media.remote_video |= remote;
                        media.exotic_remote_video |=
                            remote && gemini_video_source(&video_url.url).is_none();
                    }
                }
            }
        }
        media
    }
}

fn is_remote(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    url.starts_with("http://") || url.starts_with("https://")
}

/// OpenAI 的 `input_audio` 既可能是 `{data, format}`（base64），也可能是 `{url}`。
fn audio_is_remote(input_audio: &serde_json::Value) -> bool {
    input_audio
        .get("url")
        .and_then(serde_json::Value::as_str)
        .map(is_remote)
        .unwrap_or(false)
}

/// 某方言能否承载当前媒体。返回原因文本便于直接展示给用户。
pub fn carry(dialect: Dialect, media: &Media) -> Result<(), String> {
    if !media.any() {
        return Ok(());
    }
    match dialect {
        // OpenAI 兼容链路三种媒体都按原样透传，由上游自行取用远程地址。
        // Responses 是 OpenAI 系，媒体处理与 Chat Completions 同规则。
        Dialect::OpenAI | Dialect::Responses => Ok(()),
        // Ollama 原生只接受 base64 图片；音频与视频不在其 /api/chat 协议里。
        Dialect::Ollama => {
            if media.audio {
                return Err("Ollama 原生接口不接受音频输入".into());
            }
            if media.video {
                return Err("Ollama 原生接口不接受视频输入".into());
            }
            if media.remote_image {
                return Err("Ollama 原生接口只接受 base64（data:）图片".into());
            }
            Ok(())
        }
        // Anthropic Messages 支持图片（含远程 URL），不支持音频与视频。
        Dialect::Anthropic => {
            if media.audio {
                return Err("Anthropic 接口不接受音频输入".into());
            }
            if media.video {
                return Err("Anthropic 接口不接受视频输入".into());
            }
            Ok(())
        }
        // Gemini 原生只接受内联 base64、YouTube/GCS 视频；任意 http 图片或音频
        // 必须由调用方先转成 base64，网关不代为抓取远程内容。
        Dialect::Gemini => {
            if media.remote_image {
                return Err(
                    "Gemini 原生接口只接受 base64（data:）图片，需由客户端先内联图片".into(),
                );
            }
            if media.remote_audio {
                return Err(
                    "Gemini 原生接口只接受 base64（data:）音频，需由客户端先内联音频".into(),
                );
            }
            if media.exotic_remote_video {
                return Err("Gemini 原生接口只接受 YouTube / Google 存储视频或 base64 视频".into());
            }
            Ok(())
        }
    }
}

/// 视频地址是否属于 Gemini 可接受的来源（YouTube 或 Google 系 URI）。
pub fn gemini_video_source(url: &str) -> Option<(&'static str, String)> {
    let trimmed = url.trim();
    if trimmed.starts_with("gs://") {
        return Some(("gs", trimmed.to_owned()));
    }
    if let Some(rest) = trimmed.strip_prefix("data:") {
        return Some(("inline", rest.to_owned()));
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("youtube.com/") || lower.contains("youtu.be/") {
        return Some(("youtube", trimmed.to_owned()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ImageUrl, Message};
    use serde_json::json;

    fn request_with(parts: Vec<Part>) -> ChatRequest {
        ChatRequest {
            model: "auto".into(),
            messages: vec![Message {
                role: crate::domain::Role::User,
                content: Content::Parts(parts),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            }],
            temperature: None,
            top_p: None,
            max_tokens: None,
            stop: None,
            stream: false,
            tools: None,
            tool_choice: None,
            thinking: None,
            extra: Default::default(),
        }
    }

    #[test]
    fn media_detection_separates_remote_and_inline_forms() {
        let inline = request_with(vec![Part::ImageUrl {
            image_url: ImageUrl {
                url: "data:image/png;base64,AAAA".into(),
                detail: None,
            },
        }]);
        let media = Media::of(&inline);
        assert!(media.image && !media.remote_image, "data: 不是远程地址");
        assert!(carry(Dialect::Gemini, &media).is_ok());

        let remote = request_with(vec![Part::ImageUrl {
            image_url: ImageUrl {
                url: "https://example.test/a.png".into(),
                detail: None,
            },
        }]);
        let media = Media::of(&remote);
        assert!(media.remote_image);
        // Gemini 必须拒绝远程图片，否则转换层只能丢掉它。
        assert!(carry(Dialect::Gemini, &media).is_err());
        // OpenAI / Anthropic 可以承载远程图片。
        assert!(carry(Dialect::OpenAI, &media).is_ok());
        assert!(carry(Dialect::Anthropic, &media).is_ok());
    }

    #[test]
    fn audio_and_video_are_limited_to_openai_style_dialects() {
        let audio = request_with(vec![Part::InputAudio {
            input_audio: json!({ "data": "AAAA", "format": "wav" }),
        }]);
        let media = Media::of(&audio);
        assert!(media.audio && !media.remote_audio);
        assert!(carry(Dialect::OpenAI, &media).is_ok());
        assert!(carry(Dialect::Anthropic, &media).is_err());
        assert!(carry(Dialect::Ollama, &media).is_err());

        let video = request_with(vec![Part::VideoUrl {
            video_url: ImageUrl {
                url: "https://www.youtube.com/watch?v=abc".into(),
                detail: None,
            },
        }]);
        let media = Media::of(&video);
        assert!(carry(Dialect::OpenAI, &media).is_ok());
        assert!(
            carry(Dialect::Gemini, &media).is_ok(),
            "YouTube 视频可以走 Gemini"
        );
        assert!(carry(Dialect::Anthropic, &media).is_err());
    }

    #[test]
    fn gemini_video_sources_are_classified_explicitly() {
        assert_eq!(
            gemini_video_source("https://youtu.be/xyz").map(|(kind, _)| kind),
            Some("youtube")
        );
        assert_eq!(
            gemini_video_source("gs://bucket/clip.mp4").map(|(kind, _)| kind),
            Some("gs")
        );
        assert_eq!(
            gemini_video_source("data:video/mp4;base64,AAAA").map(|(kind, _)| kind),
            Some("inline")
        );
        assert_eq!(gemini_video_source("https://cdn.test/clip.mp4"), None);
    }
}
