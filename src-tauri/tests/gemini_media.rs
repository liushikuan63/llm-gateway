use llm_gateway_lib::domain::{ChatRequest, Content, ImageUrl, Message, Part, Role};
use llm_gateway_lib::protocol::gemini::to_gemini_body;

fn request_with(parts: Vec<Part>) -> ChatRequest {
    ChatRequest {
        model: "gemini-2.5-flash".into(),
        messages: vec![Message {
            role: Role::User,
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

fn first_user_parts(body: &serde_json::Value) -> Vec<serde_json::Value> {
    body["contents"][0]["parts"]
        .as_array()
        .expect("contents[0].parts 必须存在")
        .clone()
}

#[test]
fn base64_images_and_audio_become_inline_data() {
    let body = to_gemini_body(&request_with(vec![
        Part::Text {
            text: "描述这张图".into(),
        },
        Part::ImageUrl {
            image_url: ImageUrl {
                url: "data:image/png;base64,QUJD".into(),
                detail: None,
            },
        },
        Part::InputAudio {
            input_audio: serde_json::json!({ "data": "QUJD", "format": "wav" }),
        },
    ]));

    let parts = first_user_parts(&body);
    let text = parts
        .iter()
        .find(|part| part.get("text").is_some())
        .expect("文本部分必须保留");
    assert_eq!(text["text"], "描述这张图");

    let image = parts
        .iter()
        .find(|part| part.get("inline_data").is_some())
        .expect("base64 图片必须转成 inline_data");
    assert_eq!(image["inline_data"]["mime_type"], "image/png");
    assert_eq!(image["inline_data"]["data"], "QUJD");

    let audio = parts
        .iter()
        .filter(|part| part.get("inline_data").is_some())
        .nth(1)
        .expect("base64 音频也必须内联发送");
    assert_eq!(audio["inline_data"]["mime_type"], "audio/wav");
    assert_eq!(audio["inline_data"]["data"], "QUJD");
}

#[test]
fn youtube_and_google_storage_videos_become_file_data() {
    let body = to_gemini_body(&request_with(vec![
        Part::Text {
            text: "总结这个视频".into(),
        },
        Part::VideoUrl {
            video_url: ImageUrl {
                url: "https://www.youtube.com/watch?v=dQw4w9WgXcQ".into(),
                detail: None,
            },
        },
        Part::VideoUrl {
            video_url: ImageUrl {
                url: "gs://bucket/clip.mp4".into(),
                detail: None,
            },
        },
    ]));

    let parts = first_user_parts(&body);
    let uris: Vec<&str> = parts
        .iter()
        .filter_map(|part| part["file_data"]["file_uri"].as_str())
        .collect();
    assert_eq!(
        uris,
        vec![
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "gs://bucket/clip.mp4"
        ]
    );
}

#[test]
fn text_only_requests_keep_the_previous_shape() {
    let body = to_gemini_body(&request_with(vec![Part::Text {
        text: "只发文字".into(),
    }]));
    let parts = first_user_parts(&body);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts[0]["text"], "只发文字");
}
