//! DuckDuckGo `lite` 版 HTML 解析。
//!
//! `Cargo.toml` 没有 HTML 解析依赖，项目铁律也不允许新增，所以这里是手写扫描。
//! 选 `lite.duckduckgo.com` 而不是主站，正是因为它的 HTML 规整到只需要两个锚点：
//!
//! ```html
//! <a rel="nofollow" href="https://example.com/a" class='result-link'>标题</a>
//! <td class='result-snippet'>摘要文本</td>
//! ```
//!
//! ## 解析失败是正常路径
//!
//! 上游随时可能改版。解析不出来时返回**空列表**而不是报错——搜索是锦上添花，
//! 不该让一次正常请求因为检索页面变形而失败。调用方通过 `X-Route-Search-Hits: 0`
//! 区分「没搜到」和「后端挂了」。

use crate::search::backend::SearchResult;

/// 覆盖到的 HTML 实体。
///
/// `&ensp;` / `&emsp;` 是必应摘要里的（实测必应会在日期后放
/// `&ensp;&#0183;` 当分隔符），不加的话摘要里会直接露出这两串字符。
const ENTITIES: &[(&str, char)] = &[
    ("&amp;", '&'),
    ("&lt;", '<'),
    ("&gt;", '>'),
    ("&quot;", '"'),
    ("&#39;", '\''),
    ("&#x27;", '\''),
    ("&apos;", '\''),
    ("&nbsp;", ' '),
    ("&ensp;", ' '),
    ("&emsp;", ' '),
    ("&thinsp;", ' '),
    ("&hellip;", '…'),
    ("&middot;", '·'),
    ("&middot", '·'),
    ("&times;", '×'),
    ("&laquo;", '«'),
    ("&raquo;", '»'),
    ("&rarr;", '→'),
];

/// 只解 HTML 实体，**不解百分号**。用于 URL。
///
/// 这条区分很要紧：`%20` 解出来是裸空格，`%26` 解出来是 `&`——把两者都解掉的
/// 结果是一个语义已经改变、而且点不开的 URL。href 本来就是已经百分号编码好的，
/// 保持原样才是可用的链接。`decode()` 用于标题与摘要这类给人看的文本。
pub fn decode_entities(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'&' {
            if let Some((needle, ch)) = ENTITIES.iter().find(|(n, _)| input[i..].starts_with(*n)) {
                out.push(*ch);
                i += needle.len();
                continue;
            }
            // 数字实体 `&#183;` / `&#0183;` / `&#xB7;`。
            // 必应摘要在日期后固定放一个 `&ensp;&#0183;` 当分隔符，
            // 不解的话这串字符会原样进注入上下文、进而进模型的眼睛。
            if let Some((ch, len)) = numeric_entity(&input[i..]) {
                out.push(ch);
                i += len;
                continue;
            }
        }
        let width = utf8_width(bytes[i]);
        let end = (i + width).min(bytes.len());
        out.push_str(&input[i..end]);
        i = end;
    }
    out
}

/// 解析 `&#183;` / `&#0183;` / `&#xB7;` 形式的数字实体，返回 (字符, 消耗字节数)。
///
/// 上限守在 Unicode 标量范围，且只接受**合法字符**——`&#0;` 这类控制字符
/// 放进来会污染日志与界面。解不出返回 `None`，调用方原样保留。
fn numeric_entity(input: &str) -> Option<(char, usize)> {
    let rest = input.strip_prefix("&#")?;
    // `prefix_len` = `&#` 之后的额外前缀长度（`x` 占 1 字节）。
    // 少算这一字节时 `&#xB7;` 会解出 `·` 却把分号留在原地（实测症状：`A·;B`）。
    let (digits, radix, semi_at, prefix_len) = if let Some(hex) = rest.strip_prefix(['x', 'X']) {
        (hex, 16, hex.find(';')?, 1usize)
    } else {
        (rest, 10, rest.find(';')?, 0usize)
    };
    // 位数上限要按**分号位置**算，不是 `digits.len()`——后者是整段剩余串的长度。
    // 写成 `digits.len() > 8` 时，`&#0183;&ensp;` 的剩余串有 12 字节就整条被丢掉，
    // 于是夹在别处的数字实体全都解不掉（实测必应摘要
    // `A&ensp;&#0183;&ensp;B` 里的 `&#0183;` 永远留着）。
    if semi_at == 0 || semi_at > 8 || digits.len() < semi_at {
        return None;
    }
    let code = u32::from_str_radix(&digits[..semi_at], radix).ok()?;
    // 跳过代理区与控制字符：它们在界面上会渲染成方块或空白。
    let ch = char::from_u32(code)?;
    if ch.is_control() {
        return None;
    }
    Some((ch, 2 + prefix_len + semi_at + 1))
}

/// 解码 HTML 实体 + 百分号转义，用于**展示文本**。
///
/// 按字节累积、末尾一次性 UTF-8 解码。**不能**逐个 `%XX` 立刻转成 char——
/// 中文 URL 常是 `%E4%B8%AD` 这样的三字节序列，逐字节解码会得到三个乱码字符。
pub fn decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        // 百分号转义：把解码出的原始字节塞进 out，留给末尾统一解码。
        if bytes[i] == b'%' && i + 2 < bytes.len() && is_hex(bytes[i + 1]) && is_hex(bytes[i + 2]) {
            if let Ok(byte) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        // HTML 实体
        if bytes[i] == b'&' {
            if let Some((needle, ch)) = ENTITIES.iter().find(|(n, _)| input[i..].starts_with(*n)) {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                i += needle.len();
                continue;
            }
            // 数字实体也必须在这里解：**展示文本走的是 `decode()` 而不是
            // `decode_entities()`**，两处各写一份分支时极易漏掉一处。
            // 症状是必应摘要在模型眼里留下 `&#0183;` 这串字符。
            if let Some((ch, len)) = numeric_entity(&input[i..]) {
                let mut buf = [0u8; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                i += len;
                continue;
            }
        }
        // 原样复制一个 UTF-8 字符的全部字节。
        let width = utf8_width(bytes[i]);
        let end = (i + width).min(bytes.len());
        out.extend_from_slice(&bytes[i..end]);
        i = end;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_hex(byte: u8) -> bool {
    byte.is_ascii_hexdigit()
}

/// UTF-8 首字节决定的后续字节数；续字节一律按 1 走。
fn utf8_width(first: u8) -> usize {
    match first {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF7 => 4,
        _ => 1,
    }
}

/// 按类名抽出 `<tag … class=…>inner</tag>` 的 inner，返回 `(inner 起点, 解码后文本, inner 终点)`。
///
/// 用**类名**而不是 `class='result-link'` 当锚点：引号风格上游随时可能改，
/// 而锚点写死成 `class='…'` 的话改一次引号就让整页解析静默返回空列表——
/// 界面只显示「没搜到」，永远不会有人发现解析器已经坏了。
fn capture(html: &str, class_name: &str, close_tag: &str) -> Vec<(usize, String, usize)> {
    let mut out = Vec::new();
    let mut cursor = 0;
    while let Some(rel) = html[cursor..].find(class_name) {
        let marker_at = cursor + rel;
        // 类名后面的 `>` 就是开标签的结尾。
        let Some(gt_rel) = html[marker_at..].find('>') else {
            break;
        };
        let open_end = marker_at + gt_rel;
        let body_start = open_end + 1;
        let Some(end_rel) = html[body_start..].find(close_tag) else {
            break;
        };
        let body_end = body_start + end_rel;
        out.push((
            body_start,
            // 标题与摘要是给人看的文本：实体和百分号都要解。
            decode(&strip_tags(&html[body_start..body_end])),
            body_end,
        ));
        cursor = body_end + close_tag.len();
    }
    out
}

fn strip_tags(fragment: &str) -> String {
    let mut out = String::with_capacity(fragment.len());
    let mut inside = false;
    for ch in fragment.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(ch),
            _ => {}
        }
    }
    out
}

/// 从一段 `<a ...>` 开标签里取 `href` 的值。
///
/// 只解 HTML 实体、不解百分号：href 本来就是已经百分号编码好的地址，
/// 解开 `%20` 会得到带裸空格的不可用链接，解开 `%26` 会把查询参数的分隔符也改掉。
fn href_of(tag: &str) -> String {
    let bytes = tag.as_bytes();
    let mut cursor = 0;
    while let Some(rel) = tag[cursor..].find("href") {
        let at = cursor + rel;
        // `data-href` / `ng-href` / `xlink:href` 这类属性名里也含 "href"，
        // 而且后面**同样紧跟 `=`**，所以只判断「后面是不是 =」是不够的——
        // 那样会从 `data-href="x"` 里取出 `x` 当成 URL。
        // 真正的判据是前面那个字符：属于属性名的一部分就不是我们要的。
        if at > 0 && is_name_byte(bytes[at - 1]) {
            cursor = at + 4;
            continue;
        }
        let rest = tag[at + 4..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            cursor = at + 4;
            continue;
        };
        let rest = rest.trim_start();
        return match rest.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let rest = &rest[1..];
                let end = rest.find(q).unwrap_or(rest.len());
                decode_entities(&rest[..end])
            }
            // 无引号形式读到空白为止。
            _ => {
                let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
                decode_entities(rest[..end].trim())
            }
        };
    }
    String::new()
}

/// 能出现在 HTML 属性名里的字符。用来判断 `href` 前面的那个字节
/// 是不是另一个属性名（如 `data-href`）的一部分。
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
}

/// DuckDuckGo 有时把结果链接包成 `/l/?uddg=<百分号编码的目标>`。
/// 不解包的话，注入上下文里会全是点不开的跳转地址。
fn unwrap_redirect(url: &str) -> Option<String> {
    if !url.starts_with("//") && !url.contains("duckduckgo.com/l/?") {
        return Some(url.to_owned());
    }
    let query = url.split_once("uddg=")?.1;
    let value = query.split('&').next()?;
    let target = decode(value);
    let absolute = if let Some(rest) = target.strip_prefix("//") {
        format!("https://{rest}")
    } else {
        target
    };
    (absolute.starts_with("http://") || absolute.starts_with("https://")).then_some(absolute)
}

/// 解析 DuckDuckGo lite 页。
///
/// `limit` 为 0 时返回空列表；`limit` 直接限制输出条数。
pub fn duckduckgo_lite(html: &str, limit: usize) -> Vec<SearchResult> {
    if limit == 0 {
        return Vec::new();
    }
    // 链接锚点按出现顺序即结果顺序。
    let links = capture(html, "result-link", "</a>");
    let snippets = capture(html, "result-snippet", "</td>");

    let mut out: Vec<SearchResult> = Vec::new();
    for (start, title, _end) in links.iter() {
        if out.len() >= limit {
            break;
        }
        let title = title.trim();
        if title.is_empty() {
            continue;
        }
        // 往前找最近的 `>`，那一段就是这个 <a> 的开标签。
        let open_end = html[..*start].rfind('>').unwrap_or(0);
        let open_start = html[..open_end].rfind('<').unwrap_or(0);
        let href = href_of(&html[open_start..=open_end]);
        if href.trim().is_empty() {
            // 取不到地址就没有结果。宁可少一条，也不要在注入上下文里
            // 塞一个空链接让模型去猜。
            continue;
        }
        let Some(url) = unwrap_redirect(&href) else {
            continue;
        };
        // 摘要不一定每条都有，按锚点顺序就近配对。
        let snippet = snippets
            .iter()
            .find(|(at, _, _)| *at > *start)
            .map(|(_, text, _)| text.trim().to_owned())
            .unwrap_or_default();
        out.push(SearchResult {
            title: title.to_owned(),
            url,
            snippet,
            score: 0.0,
        });
    }
    out
}

/// 解析必应（Bing）中国站的 SERP。
///
/// **为什么加这个后端**：原有的免 Key 兜底是 DuckDuckGo，但本机实测
/// `lite.duckduckgo.com` / `html.duckduckgo.com` **全部超时**，
/// 「免 Key 兜底」在本机等于没有。而必应中国站在本机实测
/// 248 ms 返回、解析出 10 条真实结果，且**不需要任何 Key**。
///
/// 结果结构（实测 2026-10-05 `cn.bing.com`）：
/// 每条结果是 `<li class="b_algo">`，里面 `<h2><a href="…">标题</a></h2>`，
/// 摘要在 `<p class="b_lineclamp…">` 或紧随 h2 的段落里。
///
/// 摘要**允许为空**：必应经常不返回摘要，缺了就缺了，硬凑反而是编的。
pub fn bing_cn(html: &str, limit: usize) -> Vec<SearchResult> {
    if limit == 0 {
        return Vec::new();
    }
    let mut out: Vec<SearchResult> = Vec::new();
    // 以结果块为界，逐块解析。比全页正则稳：不会把导航栏里的 <h2> 也当成结果。
    let mut cursor = 0usize;
    while out.len() < limit {
        let Some(rel) = html[cursor..].find("b_algo") else {
            break;
        };
        let start = cursor + rel;
        // 该块到下一个块为止
        let end = html[start + 7..]
            .find("b_algo")
            .map(|n| start + 7 + n)
            .unwrap_or(html.len());
        let block = &html[start..end];
        cursor = end.max(start + 7);

        let Some(h2_at) = block.find("<h2") else {
            continue;
        };
        // 同理：`find` 是相对偏移，必须加回基准。
        let Some(h2_rel) = block[h2_at..].find("</h2>") else {
            continue;
        };
        let h2_end = h2_at + h2_rel;
        let h2 = &block[h2_at..h2_end];
        let Some(a_at) = h2.find("<a") else { continue };
        // 注意：`find` 返回的是**相对偏移**，必须加回基准下标才是绝对位置。
        // 写成 `let a_end = h2[a_at..].find('>')` 会得到一个相对值，
        // 后面 `h2[a_at..=a_end]` 就在错误的字节位置切中文串——
        // 症状是标题被截成 `e/"第一个标题`、地址少掉尾巴（真机抓必应时实测到）。
        let Some(a_rel) = h2[a_at..].find('>') else {
            continue;
        };
        let a_end = a_at + a_rel;
        let open = &h2[a_at..=a_end];
        let href = href_of(open);
        if href.trim().is_empty() || href.starts_with('#') {
            continue;
        }
        // 标题 = <a …> 与 </a> 之间的正文，剥标签后解实体
        let inner_start = a_end + 1;
        let title_html = match h2[inner_start..].find("</a>") {
            Some(n) => &h2[inner_start..inner_start + n],
            None => continue,
        };
        let title = decode(&strip_tags(title_html));
        let title = title.trim();
        if title.is_empty() {
            continue;
        }
        // 摘要：块里第一个 <p …>…</p>；取不到就留空。
        let snippet = block
            .find("<p")
            .and_then(|p| block[p..].find("</p>").map(|n| &block[p..p + n]))
            .map(|p| decode(&strip_tags(p)))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        out.push(SearchResult {
            title: title.to_owned(),
            url: href,
            snippet,
            score: 0.0,
        });
    }
    out
}

/// 把检索结果拼成注入上下文的一段文本。
///
/// 刻意用紧凑编号列表而不是 Markdown 表格：它要塞进 token 预算，而且模型读
/// 「1. 标题 — URL\\n   摘要」比读表格更省 token 也更少走神。
pub fn render(query: &str, backend: &str, results: &[SearchResult]) -> String {
    let mut out = format!(
        "[联网搜索结果] 查询：{query}  来源：{backend}  命中：{}\n",
        results.len()
    );
    for (index, item) in results.iter().enumerate() {
        out.push_str(&format!("{}. {} — {}\n", index + 1, item.title, item.url));
        let snippet = item.snippet.trim();
        if !snippet.is_empty() {
            out.push_str("   ");
            out.push_str(snippet);
            out.push('\n');
        }
    }
    out.push_str("（以上为联网检索到的公开资料。请在回答中标注来源；若资料与你的内置知识冲突，以检索结果为准并说明。）");
    out
}
