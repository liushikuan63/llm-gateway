//! B3 审计检索与导出：**过滤条件 → SQL**、**行 → JSONL/CSV**、**提示词脱敏**。
//!
//! 本模块不碰数据库、不碰文件系统，全是纯函数。理由与 `budget` / `cache` 一致：
//! 这三件事判错的后果都很隐蔽 ——
//!
//! - 过滤条件拼错 → 返回的行少了，用户以为「没发生过」；
//! - 导出塑形错 → 文件能生成、能打开，只是内容是错的；
//! - 脱敏漏了一条 → 用户内容（可能含密钥）落到磁盘上，而且没人会发现。
//!
//! 三种都必须能单测打穿。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 单页最多返回多少条。
///
/// 卡片刻意写死 500 并要求用 LIMIT/OFFSET：这是桌面应用，
/// 用户可能已经跑了几个月，「把整表读进内存再分页」会随数据量线性变卡。
pub const MAX_PAGE_SIZE: u32 = 500;
/// 不指定时每页多少条。
pub const DEFAULT_PAGE_SIZE: u32 = 100;

/// 单次导出的硬上限。
///
/// 导出确实需要全量，但一次几十万行会让进程内存与用户等待时间都失控。
/// 超限时**如实报错**（见 [`check_export_size`]）而不是静默截断 ——
/// 截断的导出看起来是成功的，用户拿去对账才发现少了。
pub const EXPORT_MAX_ROWS: u64 = 100_000;

/// 导出前的容量判断。超限时返回可操作的提示。
///
/// 抽成纯函数而不是写在 `repo` 里：给 10 万行数据造一遍要跑很久，
/// 而这条逻辑恰恰是「静默截断」的唯一防线 —— 它必须被测到。
/// 现在可以直接喂一个大数字进来。
pub fn check_export_size(total: u64) -> Result<(), String> {
    if total > EXPORT_MAX_ROWS {
        return Err(format!(
            "命中 {total} 条，超过单次导出上限 {EXPORT_MAX_ROWS} 条。\
             请缩小时间区间或加过滤条件后重试。"
        ));
    }
    Ok(())
}

/// 审计存储配置。
///
/// 用 derive 而不是手写 `impl Default`：`store_refined_prompt: false`
/// 正好是 `bool` 的默认值，derive 与手写完全等价。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditConfig {
    /// 是否把**改写后的最终提示词**存进 `requests.refined_prompt`。
    ///
    /// **默认关闭是硬要求。** 存提示词就等于存用户内容 ——
    /// 那是隐私边界的移动，必须由用户显式打开，不能因为「反正已经存了
    /// token 数」就顺手加上。开启后写入前一定过 [`redact_prompt`]。
    #[serde(default)]
    pub store_refined_prompt: bool,
}

/// 决定要不要把改写后的提示词落库，并顺手脱敏。
///
/// 单独抽成函数而不是在写入点写 `if cfg.audit.store_refined_prompt`：
/// **「开关判断」与「脱敏」必须同进同退**。分开放的话，很容易在某个新加的
/// 写入点上只抄了开关、忘了脱敏 —— 那种缺陷不会有任何报错，
/// 只是密钥静静躺进了用户的数据库。
///
/// 返回 `None` 表示不存。
pub fn refined_prompt_to_store(cfg: &AuditConfig, refined: Option<&str>) -> Option<String> {
    if !cfg.store_refined_prompt {
        return None;
    }
    refined.map(redact_prompt)
}

/// 审计检索的过滤条件。**所有字段都是可选的**，缺省即不加该条件。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RequestFilter {
    /// 起始时间（含），Unix 秒。
    #[serde(default)]
    pub from_ts: Option<i64>,
    /// 结束时间（含），Unix 秒。
    #[serde(default)]
    pub to_ts: Option<i64>,
    /// 按 `routed_provider` 精确匹配。
    #[serde(default)]
    pub provider: Option<String>,
    /// 按 `routed_model` 精确匹配。
    #[serde(default)]
    pub model: Option<String>,
    /// 状态过滤。**两种写法**：具体状态码（`"429"`）或状态类（`"2xx"` / `"4xx"` / `"5xx"`）。
    #[serde(default)]
    pub status: Option<String>,
    /// 按 `currency` 精确匹配。
    #[serde(default)]
    pub currency: Option<String>,
    /// 成本下限（含）。
    #[serde(default)]
    pub min_cost: Option<f64>,
    /// 成本上限（含）。
    #[serde(default)]
    pub max_cost: Option<f64>,
    /// 只看发生过错误的行。
    #[serde(default)]
    pub only_errors: bool,
    /// 只看发生过降级的行（`fallback_attempts > 0`）。
    #[serde(default)]
    pub only_fallbacks: bool,
    /// 每页条数。`None` 用 [`DEFAULT_PAGE_SIZE`]，超过 [`MAX_PAGE_SIZE`] 会被夹到上限。
    #[serde(default)]
    pub limit: Option<u32>,
    /// 偏移。用 LIMIT/OFFSET 而不是游标：审计表有自增 `id` 但按 `ts DESC` 排序，
    /// 而 `ts` 会重复（同一秒内多条），游标要处理并列，复杂度不划算。
    /// 代价是翻页期间新写入会挪动分页边界 —— 这一点写在注释里，不假装没有。
    #[serde(default)]
    pub offset: Option<u32>,
}

impl RequestFilter {
    /// 夹到合法范围的每页条数。
    ///
    /// `limit = 0` 抬到 1 而不是 0：0 在 SQL 里是「返回空集」，
    /// 而用户传 0 的意图几乎必然是「没填」。
    pub fn page_size(&self) -> u32 {
        match self.limit {
            None => DEFAULT_PAGE_SIZE,
            Some(0) => 1,
            Some(n) => n.min(MAX_PAGE_SIZE),
        }
    }

    pub fn page_offset(&self) -> u32 {
        self.offset.unwrap_or(0)
    }
}

/// 一个绑定值。为什么不直接用 sqlx 的参数类型：那会把本模块绑死到 sqlx 上，
/// 而「拼了哪些条件、按什么顺序绑」这件事本身要能单测。
#[derive(Debug, Clone, PartialEq)]
pub enum Bind {
    I64(i64),
    F64(f64),
    Text(String),
}

/// 状态过滤的解析结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusMatcher {
    /// 精确状态码
    Exact(i64),
    /// 状态类，例如 200..=299
    Class(i64, i64),
}

impl StatusMatcher {
    /// 解析 `"429"` / `"2xx"` / `"4xx"` / `"5xx"`。
    ///
    /// 解析不出来时返回 `None`，调用方**不加这个条件**而不是报错 ——
    /// 与 `budget::parse_allowed_models` 同一取向：过滤条件失效（结果变多）
    /// 比过滤条件写错（结果变少、还看不出为什么）更容易被发现。
    pub fn parse(raw: &str) -> Option<Self> {
        let t = raw.trim().to_ascii_lowercase();
        if t.is_empty() {
            return None;
        }
        // 状态类：第一位数字 + "xx"
        if let Some(digit) = t.strip_suffix("xx") {
            let d: i64 = digit.parse().ok()?;
            if (1..=5).contains(&d) {
                return Some(StatusMatcher::Class(d * 100, d * 100 + 99));
            }
            return None;
        }
        t.parse::<i64>().ok().map(StatusMatcher::Exact)
    }
}

/// 拼 WHERE 子句。
///
/// 返回 `(子句, 绑定值)`。子句形如 ` WHERE a = ? AND b > ?`（含前导空格），
/// 无条件时是空串 —— 这样调用方可以直接 `format!("...{where} ORDER BY ...")`。
///
/// **每个条件只在自己有值时出现**，顺序固定，绑定值与 `?` 一一对应。
pub fn build_where(f: &RequestFilter) -> (String, Vec<Bind>) {
    let mut clauses: Vec<&str> = Vec::new();
    let mut binds: Vec<Bind> = Vec::new();

    if let Some(from) = f.from_ts {
        clauses.push("ts >= ?");
        binds.push(Bind::I64(from));
    }
    if let Some(to) = f.to_ts {
        clauses.push("ts <= ?");
        binds.push(Bind::I64(to));
    }
    if let Some(p) = f
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push("routed_provider = ?");
        binds.push(Bind::Text(p.to_string()));
    }
    if let Some(m) = f.model.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        clauses.push("routed_model = ?");
        binds.push(Bind::Text(m.to_string()));
    }
    if let Some(c) = f
        .currency
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        clauses.push("currency = ?");
        binds.push(Bind::Text(c.to_string()));
    }
    if let Some(s) = f.status.as_deref().and_then(StatusMatcher::parse) {
        match s {
            StatusMatcher::Exact(code) => {
                clauses.push("status = ?");
                binds.push(Bind::I64(code));
            }
            StatusMatcher::Class(lo, hi) => {
                clauses.push("status >= ? AND status <= ?");
                binds.push(Bind::I64(lo));
                binds.push(Bind::I64(hi));
            }
        }
    }
    if let Some(min) = f.min_cost {
        clauses.push("cost >= ?");
        binds.push(Bind::F64(min));
    }
    if let Some(max) = f.max_cost {
        clauses.push("cost <= ?");
        binds.push(Bind::F64(max));
    }
    if f.only_errors {
        // `error IS NOT NULL` 而不是 `!= ''`：没有错误时这一列就是 NULL。
        clauses.push("error IS NOT NULL");
    }
    if f.only_fallbacks {
        clauses.push("fallback_attempts > 0");
    }

    if clauses.is_empty() {
        return (String::new(), binds);
    }
    (format!(" WHERE {}", clauses.join(" AND ")), binds)
}

/// 一行审计记录。由 `repo` 从 sqlx Row 填出来，本模块负责塑形。
///
/// 刻意不用 `serde_json::Value` 当中转：那样塑形逻辑就只能靠「跑一遍看输出」
/// 来验证，而下面的 JSONL / CSV 两条路径都要能对着结构断言。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRow {
    pub id: i64,
    pub ts: i64,
    pub session_id: Option<String>,
    pub client: Option<String>,
    pub requested_model: String,
    pub routed_provider: Option<String>,
    pub routed_model: Option<String>,
    pub status: Option<i64>,
    pub latency_ms: Option<i64>,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub fallback_attempts: i64,
    pub error: Option<String>,
    pub cost: Option<f64>,
    pub currency: Option<String>,
    pub rate_label: Option<String>,
    pub estimated_prompt_tokens: Option<i64>,
    /// 逐跳降级明细。**已经解析成数组**，不是原始字符串 ——
    /// 导出的目的是给人看和给脚本读，塞成字符串就失去了导出的意义。
    pub attempts: Vec<Value>,
    pub route_intent: Option<String>,
    pub route_classifier: Option<String>,
    pub route_search: Option<String>,
    pub route_search_hits: Option<i64>,
    pub route_refined: Option<bool>,
    pub route_refine_note: Option<String>,
    /// 归属的远程 Key。本机统一 Key 的请求是 `None`（B2 加的列）。
    pub access_key_id: Option<String>,
    /// 改写后的最终提示词。**默认不写**（`audit.store_refined_prompt` 关着时恒为 `None`）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub refined_prompt: Option<String>,
}

impl AuditRow {
    /// 转成 JSONL 里的一行。`attempts` 是**数组**。
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "ts": self.ts,
            "session_id": self.session_id,
            "client": self.client,
            "requested_model": self.requested_model,
            "routed_provider": self.routed_provider,
            "routed_model": self.routed_model,
            "status": self.status,
            "latency_ms": self.latency_ms,
            "prompt_tokens": self.prompt_tokens,
            "completion_tokens": self.completion_tokens,
            "fallback_attempts": self.fallback_attempts,
            "error": self.error,
            "cost": self.cost,
            "currency": self.currency,
            "rate_label": self.rate_label,
            "estimated_prompt_tokens": self.estimated_prompt_tokens,
            // 数组，不是字符串
            "attempts": self.attempts,
            "route_intent": self.route_intent,
            "route_classifier": self.route_classifier,
            "route_search": self.route_search,
            "route_search_hits": self.route_search_hits,
            "route_refined": self.route_refined,
            "route_refine_note": self.route_refine_note,
            "access_key_id": self.access_key_id,
            "refined_prompt": self.refined_prompt,
        })
    }
}

/// JSONL 导出：一行一个 JSON 对象。
pub fn to_jsonl(rows: &[AuditRow]) -> String {
    let mut out = String::new();
    for row in rows {
        // `to_string` 不会失败（Value 一定能序列化）。
        out.push_str(&serde_json::to_string(&row.to_json()).unwrap_or_default());
        out.push('\n');
    }
    out
}

/// 尝试记录拍平后的列名。CSV 没有嵌套结构，逐跳明细只能摊成多列。
///
/// 为什么带 `attempt_N_` 前缀而不是直接用字段名：一次请求可能有多次尝试，
/// 不加前缀会让第二跳覆盖第一跳的列。
pub const ATTEMPT_FIELDS: &[&str] = &[
    "provider",
    "model",
    "status",
    "latency_ms",
    "error",
    "error_kind",
];

/// 基础列（不含 attempts 展开列）。
pub const BASE_COLUMNS: &[&str] = &[
    "id",
    "ts",
    "session_id",
    "client",
    "requested_model",
    "routed_provider",
    "routed_model",
    "status",
    "latency_ms",
    "prompt_tokens",
    "completion_tokens",
    "fallback_attempts",
    "error",
    "cost",
    "currency",
    "rate_label",
    "estimated_prompt_tokens",
    "route_intent",
    "route_classifier",
    "route_search",
    "route_search_hits",
    "route_refined",
    "route_refine_note",
    "access_key_id",
    "refined_prompt",
];

/// 整份 CSV 的表头：基础列 + 按**最大尝试次数**展开的列。
///
/// 展开次数取本批数据里的最大值，而不是写死一个上限：
/// 写死会让超出部分静默丢失（列不存在，值就被丢了），
/// 而那正是「导出看起来成功、内容不全」的经典症状。
pub fn csv_header(max_attempts: usize) -> Vec<String> {
    let mut cols: Vec<String> = BASE_COLUMNS.iter().map(|s| (*s).to_string()).collect();
    for i in 1..=max_attempts {
        for field in ATTEMPT_FIELDS {
            cols.push(format!("attempt_{i}_{field}"));
        }
    }
    cols
}

fn csv_escape(value: &str) -> String {
    // CSV 规矩：含逗号、引号、换行时用双引号包起来，内部引号翻倍。
    if value.contains(',') || value.contains('"') || value.contains('\n') || value.contains('\r') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn opt_str(v: &Option<String>) -> String {
    v.clone().unwrap_or_default()
}

/// 一行数据的 CSV 单元格，顺序与 [`csv_header`] 一致。
pub fn csv_cells(row: &AuditRow, max_attempts: usize) -> Vec<String> {
    let mut cells: Vec<String> = vec![
        row.id.to_string(),
        row.ts.to_string(),
        opt_str(&row.session_id),
        opt_str(&row.client),
        row.requested_model.clone(),
        opt_str(&row.routed_provider),
        opt_str(&row.routed_model),
        row.status.map(|v| v.to_string()).unwrap_or_default(),
        row.latency_ms.map(|v| v.to_string()).unwrap_or_default(),
        row.prompt_tokens.to_string(),
        row.completion_tokens.to_string(),
        row.fallback_attempts.to_string(),
        opt_str(&row.error),
        row.cost.map(|v| v.to_string()).unwrap_or_default(),
        opt_str(&row.currency),
        opt_str(&row.rate_label),
        row.estimated_prompt_tokens
            .map(|v| v.to_string())
            .unwrap_or_default(),
        opt_str(&row.route_intent),
        opt_str(&row.route_classifier),
        opt_str(&row.route_search),
        row.route_search_hits
            .map(|v| v.to_string())
            .unwrap_or_default(),
        row.route_refined.map(|v| v.to_string()).unwrap_or_default(),
        opt_str(&row.route_refine_note),
        opt_str(&row.access_key_id),
        // refined_prompt 在 CSV 里也要出现（列固定），空就是空。
        // 换行会被 csv_escape 引起来，不会破坏行结构。
        opt_str(&row.refined_prompt),
    ];
    for i in 0..max_attempts {
        let attempt = row.attempts.get(i);
        for field in ATTEMPT_FIELDS {
            let v = attempt
                .and_then(|a| a.get(*field))
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            cells.push(v);
        }
    }
    cells
}

/// 生成 CSV 全文（含表头）。
pub fn to_csv(rows: &[AuditRow]) -> String {
    let max_attempts = rows.iter().map(|r| r.attempts.len()).max().unwrap_or(0);
    let mut out = String::new();
    // UTF-8 BOM：Excel 打开无 BOM 的 UTF-8 CSV 会把中文显示成乱码，
    // 而这份导出的主要消费者就是人（拿 Excel/表格软件看）。
    out.push('\u{feff}');
    out.push_str(
        &csv_header(max_attempts)
            .iter()
            .map(|c| csv_escape(c))
            .collect::<Vec<_>>()
            .join(","),
    );
    out.push('\n');
    for row in rows {
        out.push_str(
            &csv_cells(row, max_attempts)
                .iter()
                .map(|c| csv_escape(c))
                .collect::<Vec<_>>()
                .join(","),
        );
        out.push('\n');
    }
    out
}

/// 导出时附带的列说明文件内容。
///
/// 为什么带这个：CSV 没有注释标准，列名一旦离开本仓库就没人知道
/// `attempt_2_error_kind` 是什么、`route_*` 是谁写的。
/// 把来源写成伴随文件（而不是塞进 CSV 注释行 —— 那会被解析器当成数据）。
pub fn columns_doc(max_attempts: usize) -> String {
    let mut out = String::new();
    out.push_str("# 导出列说明\n\n");
    out.push_str(
        "本文件由 llm-gateway 的「导出审计」自动生成，与同名 `.csv` 一起使用。\n\
         `ts` 是 Unix 秒（UTC）。金额 `cost` 的单位由同行 `currency` 决定，\n\
         不同币种的行不能相加。\n\n",
    );
    out.push_str("## 基础列\n\n| 列名 | 含义 |\n| --- | --- |\n");
    for (name, meaning) in BASE_COLUMN_DOCS {
        out.push_str(&format!("| `{name}` | {meaning} |\n"));
    }
    out.push_str("\n## 逐跳尝试列\n\n");
    out.push_str(
        "一次请求可能换过多家上游。`attempt_N_*` 是第 N 跳的明细，\n\
         N 从 1 开始；某一跳不存在时该组列为空。\n\n",
    );
    out.push_str(&format!(
        "本批导出里最大尝试次数为 **{max_attempts}**，因此展开了 {max_attempts} 组列。\n\
         换一批数据（最大尝试次数不同）时列数会变，脚本请按列名取值而不是按列序号。\n\n"
    ));
    out.push_str("| 列名前缀 | 含义 |\n| --- | --- |\n");
    for (field, meaning) in ATTEMPT_FIELD_DOCS {
        out.push_str(&format!("| `attempt_N_{field}` | {meaning} |\n"));
    }
    out
}

/// 基础列的中文说明。列名顺序与 [`BASE_COLUMNS`] 一致。
pub const BASE_COLUMN_DOCS: &[(&str, &str)] = &[
    ("id", "自增主键，同一批导出里唯一"),
    ("ts", "请求发生时间，Unix 秒（UTC）"),
    ("session_id", "会话标识（远程模式下是隔离后的内部键）"),
    (
        "client",
        "客户端标识。`remote-key:<id>` 表示由某个远程 Key 发出",
    ),
    (
        "requested_model",
        "客户端请求的模型名（可能是 `auto` 等虚拟名）",
    ),
    ("routed_provider", "实际命中的供应商 id"),
    ("routed_model", "实际命中的上游模型名"),
    ("status", "HTTP 状态码。NULL 表示请求没走到上游"),
    ("latency_ms", "端到端耗时（毫秒）"),
    ("prompt_tokens", "上游返回的输入 token"),
    ("completion_tokens", "上游返回的输出 token"),
    ("fallback_attempts", "降级次数。> 0 表示换过上游"),
    ("error", "错误原文。NULL 表示成功"),
    ("cost", "估算花费。NULL 表示该模型没配价格"),
    ("currency", "花费币种。不同币种不能相加"),
    ("rate_label", "生效的计价档位（时段价 / 输入长度分档）"),
    (
        "estimated_prompt_tokens",
        "网关本地估算的输入 token，用于与上游实际值对照",
    ),
    (
        "route_intent",
        "智能模式判定的任务类别：simple / vision / reasoning",
    ),
    ("route_classifier", "类别由谁判出：rule / jev / heuristic"),
    (
        "route_search",
        "联网搜索状态：后端名或 failed。空表示没触发",
    ),
    ("route_search_hits", "检索命中条数"),
    ("route_refined", "提示词是否被改写"),
    ("route_refine_note", "改写前后字数，形如 `原文→新文`"),
    (
        "access_key_id",
        "归属的远程访问 Key。空表示本机统一 Key 发出",
    ),
    (
        "refined_prompt",
        "改写后的最终提示词。仅在开启审计存储时有值，且已脱敏",
    ),
];

/// 逐跳列的中文说明。
pub const ATTEMPT_FIELD_DOCS: &[(&str, &str)] = &[
    ("provider", "这一跳的供应商 id"),
    ("model", "这一跳的模型名"),
    ("status", "这一跳的上游状态码（若有响应）"),
    ("latency_ms", "这一跳的耗时"),
    ("error", "这一跳的错误原文。空表示这一跳成功"),
    ("error_kind", "错误归类，用于统计同一类失败"),
];

/// 脱敏：把用户内容里不该落盘的东西抹掉。
///
/// **只在 `audit.store_refined_prompt` 开启时才会被调用**，但一旦调用就必须完整 ——
/// 存提示词就等于存用户内容，隐私边界要显式。
///
/// 抹掉四类（每一类都在下面有独立用例）：
/// 1. `Authorization: Bearer xxx` 整行 → `Authorization: [已脱敏]`
/// 2. `sk-` 开头的长串（OpenAI 风格密钥）
/// 3. `api_key=...` / `api-key: ...` / `apikey"...` 这类键值
/// 4. 长 base64 串（连续 40+ 个 base64 字符）—— 可能是编码过的密钥或图片数据
///
/// **宁多抹不少抹**：多抹一段提示词只是损失一点可读性，
/// 漏抹一个密钥是把它写进了用户的磁盘。
pub fn redact_prompt(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for (idx, line) in input.lines().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&redact_line(line));
    }
    // `lines()` 会吃掉结尾换行，这里不补：多一个换行没有意义，
    // 少一个也不会改变内容。保持一致比「还原原样」更重要。
    out
}

fn redact_line(line: &str) -> String {
    // 1) 整行的 Authorization 头
    let lower = line.to_ascii_lowercase();
    if lower.contains("authorization:") || lower.contains("authorization=") {
        // 保留前缀（谁被脱敏了要看得到），抹掉值
        if let Some(pos) = lower.find("authorization") {
            let head = &line[..pos];
            return format!("{head}Authorization: [已脱敏]");
        }
    }

    let mut result = String::with_capacity(line.len());
    let bytes: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        // 2) sk- 开头的密钥串：`sk-` 后面跟至少 8 个 [A-Za-z0-9_-]
        if bytes[i] == 's'
            && i + 1 < bytes.len()
            && bytes[i + 1] == 'k'
            && i + 2 < bytes.len()
            && bytes[i + 2] == '-'
        {
            let mut j = i + 3;
            while j < bytes.len()
                && (bytes[j].is_ascii_alphanumeric() || bytes[j] == '_' || bytes[j] == '-')
            {
                j += 1;
            }
            if j - (i + 3) >= 8 {
                result.push_str("[已脱敏:密钥]");
                i = j;
                continue;
            }
        }

        // 3) api_key / api-key / apikey 后面跟分隔符与值
        if let Some(len) = match_key_assignment(&bytes, i) {
            result.push_str("[已脱敏:密钥]");
            i += len;
            continue;
        }

        // 4) 长 base64 串：连续 40+ 个 base64 字符
        if is_base64_char(bytes[i]) {
            let mut j = i;
            while j < bytes.len() && is_base64_char(bytes[j]) {
                j += 1;
            }
            if j - i >= 40 {
                result.push_str("[已脱敏:长串]");
                i = j;
                continue;
            }
        }

        result.push(bytes[i]);
        i += 1;
    }
    result
}

fn is_base64_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='
}

/// 匹配 `api_key` / `api-key` / `apikey`（不分大小写）后面跟分隔符与值。
/// 命中时返回**从 i 开始要吃掉多少个字符**。
fn match_key_assignment(chars: &[char], i: usize) -> Option<usize> {
    const KEYS: [&str; 3] = ["api_key", "api-key", "apikey"];
    for key in KEYS {
        let klen = key.len();
        if i + klen > chars.len() {
            continue;
        }
        let candidate: String = chars[i..i + klen]
            .iter()
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if candidate != key {
            continue;
        }
        // 允许 `key = value` / `key: value` / `key="value"` / `key": "value"`
        let mut j = i + klen;
        // 跳过引号（JSON 里的 `"api_key"`）
        if j < chars.len() && (chars[j] == '"' || chars[j] == '\'') {
            j += 1;
        }
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if j >= chars.len() || (chars[j] != '=' && chars[j] != ':') {
            continue;
        }
        j += 1; // 吃掉分隔符
        while j < chars.len() && (chars[j].is_whitespace() || chars[j] == '"' || chars[j] == '\'') {
            j += 1;
        }
        // 值：直到空白 / 逗号 / 引号 / 结尾
        while j < chars.len()
            && !chars[j].is_whitespace()
            && chars[j] != ','
            && chars[j] != '"'
            && chars[j] != '\''
            && chars[j] != '}'
        {
            j += 1;
        }
        return Some(j - i);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, attempts: Vec<Value>) -> AuditRow {
        AuditRow {
            id,
            ts: 1_791_286_496,
            session_id: Some("s1".into()),
            client: Some("local-unified-key".into()),
            requested_model: "auto".into(),
            routed_provider: Some("mock".into()),
            routed_model: Some("alpha".into()),
            status: Some(200),
            latency_ms: Some(120),
            prompt_tokens: 10,
            completion_tokens: 5,
            fallback_attempts: attempts.len() as i64,
            error: None,
            cost: Some(0.5),
            currency: Some("USD".into()),
            rate_label: None,
            estimated_prompt_tokens: Some(9),
            attempts,
            route_intent: Some("simple".into()),
            route_classifier: Some("heuristic".into()),
            route_search: None,
            route_search_hits: None,
            route_refined: Some(false),
            route_refine_note: None,
            access_key_id: None,
            refined_prompt: None,
        }
    }

    // ---------- 过滤条件 ----------

    #[test]
    fn 无条件时_where_是空串() {
        let (sql, binds) = build_where(&RequestFilter::default());
        assert_eq!(sql, "");
        assert!(binds.is_empty(), "没有条件就不该有绑定值");
    }

    #[test]
    fn 时间区间拼成两个条件且顺序固定() {
        let f = RequestFilter {
            from_ts: Some(100),
            to_ts: Some(200),
            ..Default::default()
        };
        let (sql, binds) = build_where(&f);
        assert_eq!(sql, " WHERE ts >= ? AND ts <= ?");
        assert_eq!(binds, vec![Bind::I64(100), Bind::I64(200)]);
    }

    #[test]
    fn provider_与_model_精确匹配() {
        let f = RequestFilter {
            provider: Some("mock".into()),
            model: Some("alpha".into()),
            ..Default::default()
        };
        let (sql, binds) = build_where(&f);
        assert_eq!(sql, " WHERE routed_provider = ? AND routed_model = ?");
        assert_eq!(
            binds,
            vec![Bind::Text("mock".into()), Bind::Text("alpha".into())]
        );
    }

    #[test]
    fn 空白字符串当成没填() {
        // 前端把输入框清空后传 `""`，这时若拼上 `provider = ''`
        // 会返回空集 —— 用户看到「没有记录」而数据其实都在。
        let f = RequestFilter {
            provider: Some("   ".into()),
            model: Some(String::new()),
            currency: Some("".into()),
            ..Default::default()
        };
        let (sql, binds) = build_where(&f);
        assert_eq!(sql, "", "空白字符串不该拼成条件");
        assert!(binds.is_empty());
    }

    #[test]
    fn 状态类的三种写法() {
        assert_eq!(StatusMatcher::parse("429"), Some(StatusMatcher::Exact(429)));
        assert_eq!(
            StatusMatcher::parse("2xx"),
            Some(StatusMatcher::Class(200, 299))
        );
        assert_eq!(
            StatusMatcher::parse("5XX"),
            Some(StatusMatcher::Class(500, 599))
        );
        // 解析不出来就不加条件，而不是报错
        assert_eq!(StatusMatcher::parse("abc"), None);
        assert_eq!(StatusMatcher::parse(""), None);
        assert_eq!(StatusMatcher::parse("9xx"), None, "9xx 不是合法状态类");
    }

    #[test]
    fn 状态类拼成区间而具体码拼成等值() {
        let (sql, binds) = build_where(&RequestFilter {
            status: Some("4xx".into()),
            ..Default::default()
        });
        assert_eq!(sql, " WHERE status >= ? AND status <= ?");
        assert_eq!(binds, vec![Bind::I64(400), Bind::I64(499)]);

        let (sql2, binds2) = build_where(&RequestFilter {
            status: Some("429".into()),
            ..Default::default()
        });
        assert_eq!(sql2, " WHERE status = ?");
        assert_eq!(binds2, vec![Bind::I64(429)]);
    }

    #[test]
    fn 成本区间与降级与错误三个开关() {
        let (sql, binds) = build_where(&RequestFilter {
            min_cost: Some(0.1),
            max_cost: Some(2.0),
            only_errors: true,
            only_fallbacks: true,
            ..Default::default()
        });
        assert_eq!(
            sql,
            " WHERE cost >= ? AND cost <= ? AND error IS NOT NULL AND fallback_attempts > 0"
        );
        assert_eq!(binds, vec![Bind::F64(0.1), Bind::F64(2.0)]);
    }

    #[test]
    fn 全部条件一起拼时绑定值与问号一一对应() {
        let f = RequestFilter {
            from_ts: Some(1),
            to_ts: Some(2),
            provider: Some("p".into()),
            model: Some("m".into()),
            currency: Some("USD".into()),
            status: Some("5xx".into()),
            min_cost: Some(0.0),
            max_cost: Some(1.0),
            only_errors: true,
            only_fallbacks: true,
            ..Default::default()
        };
        let (sql, binds) = build_where(&f);
        let placeholders = sql.matches('?').count();
        assert_eq!(
            placeholders,
            binds.len(),
            "问号数必须等于绑定值数，否则 sqlx 会报参数不匹配：{sql}"
        );
        // 9 = 时间 2 + provider 1 + model 1 + currency 1 + 状态类 2 + 成本 2。
        // 第一版这里写的 8，是我自己数漏了状态类会占两个占位符。
        assert_eq!(placeholders, 9, "实际 SQL：{sql}");
    }

    #[test]
    fn 每页条数被夹在合法范围() {
        assert_eq!(RequestFilter::default().page_size(), DEFAULT_PAGE_SIZE);
        assert_eq!(
            RequestFilter {
                limit: Some(0),
                ..Default::default()
            }
            .page_size(),
            1,
            "0 的意图几乎必然是「没填」，抬到 1 而不是返回空集"
        );
        assert_eq!(
            RequestFilter {
                limit: Some(10_000),
                ..Default::default()
            }
            .page_size(),
            MAX_PAGE_SIZE
        );
        assert_eq!(
            RequestFilter {
                limit: Some(50),
                ..Default::default()
            }
            .page_size(),
            50
        );
        assert_eq!(RequestFilter::default().page_offset(), 0);
    }

    // ---------- 导出塑形 ----------

    #[test]
    fn jsonl_的_attempts_是数组不是字符串() {
        let rows = vec![row(
            1,
            vec![
                json!({"provider": "p1", "status": 500}),
                json!({"provider": "p2"}),
            ],
        )];
        let text = to_jsonl(&rows);
        let line: Value = serde_json::from_str(text.trim()).unwrap();
        assert!(
            line["attempts"].is_array(),
            "attempts 必须是数组，塞成字符串就失去了导出的意义：{}",
            line["attempts"]
        );
        assert_eq!(line["attempts"].as_array().unwrap().len(), 2);
        assert_eq!(line["attempts"][0]["provider"], "p1");
        // 一行一个对象
        assert_eq!(text.lines().count(), 1);
    }

    #[test]
    fn jsonl_行数等于记录数() {
        let rows: Vec<AuditRow> = (0..7).map(|i| row(i, vec![])).collect();
        assert_eq!(to_jsonl(&rows).lines().count(), 7);
    }

    #[test]
    fn jsonl_的空数组导出为空文本() {
        assert_eq!(to_jsonl(&[]), "");
    }

    #[test]
    fn csv_行数等于命中条数加表头() {
        let rows: Vec<AuditRow> = (0..5).map(|i| row(i, vec![])).collect();
        let csv = to_csv(&rows);
        // 5 行数据 + 1 行表头
        assert_eq!(csv.lines().count(), 6);
        // 行尾有换行，所以 split 后最后一个元素是空串
        assert!(csv.ends_with('\n'));
    }

    #[test]
    fn csv_带_bom_以便_excel_正确显示中文() {
        let csv = to_csv(&[row(1, vec![])]);
        assert!(
            csv.starts_with('\u{feff}'),
            "无 BOM 的 UTF-8 CSV 在 Excel 里中文是乱码"
        );
    }

    #[test]
    fn csv_列数随最大尝试次数展开() {
        let rows = vec![
            row(1, vec![json!({"provider": "a"})]),
            row(2, vec![json!({"provider": "b"}), json!({"provider": "c"})]),
        ];
        let csv = to_csv(&rows);
        let lines: Vec<&str> = csv.lines().collect();
        let header_cols = lines[0].split(',').count();
        assert_eq!(header_cols, BASE_COLUMNS.len() + 2 * ATTEMPT_FIELDS.len());
        // 每一行的列数都要一致，否则表格软件会把整张表读歪
        for line in &lines {
            assert_eq!(line.split(',').count(), header_cols, "行列数不一致：{line}");
        }
        // 表头（lines[0]）要真的展开出第二组列
        assert!(
            lines[0].contains("attempt_2_provider"),
            "表头应展开到第二跳：{}",
            lines[0]
        );
        // 数据行（lines[2] 是 id=2 那条，它有两跳）里第二跳的 provider 应是 c
        assert!(
            lines[2].contains(",c,"),
            "第二跳的 provider 应落在 attempt_2_provider 上：{}",
            lines[2]
        );
    }

    #[test]
    fn csv_对逗号引号换行做转义() {
        let mut r = row(1, vec![]);
        r.error = Some("boom, with \"quotes\"".into());
        let csv = to_csv(&[r]);
        assert!(csv.contains("\"boom, with \"\"quotes\"\"\""), "实际：{csv}");

        let mut r2 = row(2, vec![]);
        r2.error = Some("line1\nline2".into());
        let csv2 = to_csv(&[r2]);
        assert!(csv2.contains("\"line1\nline2\""));
    }

    #[test]
    fn 列说明文件带上来源与实际展开组数() {
        let doc = columns_doc(3);
        assert!(doc.contains("llm-gateway"));
        assert!(doc.contains("最大尝试次数为 **3**"));
        // 每一列都要有说明，缺一个用户就得去读源码
        for col in BASE_COLUMNS {
            assert!(doc.contains(&format!("`{col}`")), "列说明缺 {col}");
        }
        for field in ATTEMPT_FIELDS {
            assert!(doc.contains(&format!("`attempt_N_{field}`")), "缺 {field}");
        }
    }

    // ---------- 脱敏 ----------

    #[test]
    fn 脱敏抹掉_sk_开头的密钥() {
        let out = redact_prompt("我的 key 是 sk-abcdefghijklmnopqrstuvwxyz 请记住");
        assert!(
            !out.contains("sk-abcdefghijklmnopqrstuvwxyz"),
            "实际：{out}"
        );
        assert!(out.contains("[已脱敏:密钥]"));
        assert!(out.contains("请记住"), "脱敏不该把整句话都吃掉");
    }

    #[test]
    fn 脱敏抹掉_authorization_整行() {
        let out = redact_prompt("第一行\nAuthorization: Bearer abc123def456\n第三行");
        assert!(!out.contains("abc123def456"), "实际：{out}");
        assert!(out.contains("[已脱敏]"));
        assert!(out.contains("第一行") && out.contains("第三行"));
    }

    #[test]
    fn 脱敏抹掉_api_key_赋值() {
        for raw in [
            "api_key=secret-value-here",
            "api-key: secret-value-here",
            "APIKEY = secret-value-here",
            r#"{"api_key": "secret-value-here"}"#,
        ] {
            let out = redact_prompt(raw);
            assert!(!out.contains("secret-value-here"), "没抹掉：{raw} -> {out}");
        }
    }

    #[test]
    fn 脱敏抹掉长_base64_串() {
        let b64 = "A".repeat(60);
        let out = redact_prompt(&format!("图片数据 {b64} 结束"));
        assert!(!out.contains(&b64), "实际：{out}");
        assert!(out.contains("[已脱敏:长串]"));
    }

    #[test]
    fn 短串不该被误抹() {
        // 40 位以下不抹：普通单词、短标识会被误伤，
        // 而「宁多抹不少抹」的边界要落在真正的长串上。
        let out = redact_prompt("the quick brown fox jumps over the lazy dog");
        assert_eq!(out, "the quick brown fox jumps over the lazy dog");
        // `sk-` 后面不足 8 位也不抹（避免把 `sk-1` 这种普通词吃掉）
        let short = redact_prompt("变量名叫 sk-x");
        assert_eq!(short, "变量名叫 sk-x");
    }

    #[test]
    fn 脱敏保留正常文本与行结构() {
        let input = "第一行\n第二行\n第三行";
        assert_eq!(redact_prompt(input), input);
    }

    // ---------- 存储开关 ----------

    #[test]
    fn 默认配置下不存提示词() {
        let cfg = AuditConfig::default();
        assert!(!cfg.store_refined_prompt, "默认必须是关闭的");
        assert_eq!(
            refined_prompt_to_store(&cfg, Some("用户的原话")),
            None,
            "关着的时候一个字节都不该落库"
        );
    }

    #[test]
    fn 开启后存入的是脱敏结果() {
        let cfg = AuditConfig {
            store_refined_prompt: true,
        };
        let got = refined_prompt_to_store(&cfg, Some("用 sk-abcdefghijklmnop 调一下"))
            .expect("开启后应当返回内容");
        assert!(
            !got.contains("sk-abcdefghijklmnop"),
            "落库前必须脱敏：{got}"
        );
        assert!(got.contains("[已脱敏:密钥]"));
        // 没东西可存时也不该造出一个空串
        assert_eq!(refined_prompt_to_store(&cfg, None), None);
    }
}
