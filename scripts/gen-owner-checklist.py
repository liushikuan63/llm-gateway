"""生成 A4 的 owner 待标注清单（只出题，不给答案）。

设计约束：
1. **不给答案、不给类别提示、不打乱分组** —— 分组会泄露我的预期，
   而你一旦被我的判断锚定，这份标注就不再是独立的参照系，本卡就白做了。
   所以清单是打乱后的平铺列表，类别覆盖情况只写在生成脚本里（你看不到）。
2. 用固定随机种子打乱，保证清单可复现、可复核。
3. 与 seeded 的 32 条**不重复** —— 重复样本会让两份计数打架。

产出两个文件：
  docs/A4-owner待标注清单.md          人读的，30 题 + 待填表格
  src-tauri/tests/fixtures/classify_bench_owner.jsonl   机读的，label 留空
"""

import json
import os
import random

ROOT = r"D:\Java\GitHub\llm-auto\llm-gateway"
MD = os.path.join(ROOT, "docs", "A4-owner待标注清单.md")
JSONL = os.path.join(ROOT, "src-tauri", "tests", "fixtures", "classify_bench_owner.jsonl")

# 每项 = (prompt, has_image, has_tools, 长上下文/多轮标记, 我出这道题想覆盖什么)
# 最后一项只给生成脚本看，不写进清单。
items = [
    # --- 极短指令 ---
    ("把 theme 变量提到顶层", False, False, "", "极短、纯机械操作"),
    ("这段删了", False, False, "", "极短、指代上下文"),
    ("改成 snake_case", False, False, "", "极短、纯改名"),
    ("加个空行", False, False, "", "极短、零推理"),
    ("好了没", False, False, "", "极短、状态询问"),
    # --- 有客观答案的技术题 ---
    ("Rust 里 String 和 &str 有什么区别", False, False, "", "语言概念，文档有标准答案"),
    ("写一个二分查找", False, False, "", "有唯一公认写法"),
    ("怎么把 Vec<u8> 转成 String", False, False, "", "API 查询"),
    ("git 怎么撤销最近一次 commit 但保留改动", False, False, "", "命令查询，答案唯一"),
    ("解释一下什么是闭包捕获", False, False, "", "概念解释，教科书答案"),
    ("用 SQL 查出每个用户的订单总数", False, False, "", "写法明确，机械变换"),
    ("这段正则匹配邮箱对不对", False, False, "", "判断题，要读给定输入"),
    # --- 含定位/根因/排查的线上问题 ---
    ("昨晚发布后错误率涨了 3 倍，帮我看看", False, False, "", "要结合发布内容与监控推断"),
    ("这个页面白屏了，是什么原因", False, False, "", "短句但根因排查"),
    ("压测到 500 并发就开始丢包，查一下瓶颈在哪", False, False, "", "要分层定位"),
    ("为什么定时任务有时候不执行", False, False, "", "偶发问题，需假设-验证"),
    ("缓存和数据库数据对不上，定位一下", False, False, "", "一致性排查"),
    ("CPU 突然打满，排查一下", False, False, "", "极短但需要排查链路"),
    ("用户反馈登录后又被踢出，看看怎么回事", False, False, "", "要串起认证与超时逻辑"),
    # --- 带图片 ---
    ("这个界面布局对吗", True, False, "", "带图，文本极短"),
    ("帮我把这张图里的表格转成 markdown", True, False, "", "带图 + 含「表格」"),
    ("报错截图如上", True, False, "", "带图，文本只有 4 字"),
    ("这个配色太丑了，改改", True, False, "", "带图 + 审美判断"),
    ("图里第三个字段是什么类型", True, False, "", "带图，问细节"),
    ("按这张原型图写页面", True, False, "", "带图 + 含「设计」类词"),
    # --- 长上下文 / 多轮 + 工具 ---
    ("继续", False, True, "many", "两字承接多轮，语义重量在历史里"),
    ("总结一下这次构建", False, True, "long", "长日志 + 工具"),
    ("刚才那个改完了吗", False, True, "many", "指代多轮上下文"),
    ("按这份规范检查代码", False, True, "long", "长文档 + 工具"),
    ("下一步", False, True, "many", "三字承接多轮"),
]

assert len(items) == 30, f"要 30 条，实际 {len(items)}"

# 长上下文样本：造一段足够越过硬规则门槛的日志。
# approx_tokens 的口径是 chars / 3 + 4（domain/model.rs），
# 8000 token 需要约 24000 字符。这里给 400 行 ≈ 32000 字符。
LONG_LOG = "\n".join(
    f"2026-10-06T09:{i // 60:02d}:{i % 60:02d}Z INFO  build[{i % 6}] "
    f"step={i} artifacts={i * 17 % 300} elapsed_ms={i * 11 % 700}"
    for i in range(400)
)
MANY_TURNS = [
    "看下这个模块", "有几个函数", "哪个最长", "为什么长", "有测试吗",
    "覆盖率多少", "先补测试", "测试写好了", "跑一遍", "红了", "看下报错",
]

rng = random.Random(20261006)
order = list(range(len(items)))
rng.shuffle(order)

rows = []
lines = []
lines.append("# A4 · owner 待标注清单（30 条）")
lines.append("")
lines.append("## 怎么用")
lines.append("")
lines.append("每条只缺一个 `label`，取值三选一：`simple` / `vision` / `reasoning`。")
lines.append("")
lines.append("- **`simple`** —— 一句话能答完，不需要读你现有的代码或环境上下文")
lines.append("- **`vision`** —— 请求里真的有图片/视频（**不看文本写了什么**）")
lines.append("- **`reasoning`** —— 要读输入再推导、排查、权衡，或在长上下文里维持计划")
lines.append("")
lines.append("**判断时请只看题目本身，别参考我的任何暗示** —— 这份清单刻意打乱了顺序、")
lines.append("没有分组、也没有标出每题想考什么。你被我的预期锚定的话，")
lines.append("它就退化成「自证」，A4 这卡的意义就没了。")
lines.append("")
lines.append("填法二选一：")
lines.append("")
lines.append("1. 直接在本文件的表格里填，然后把文件给我；")
lines.append("2. 更省事：在对话里回 `1=simple 2=reasoning 3=vision ...` 这样的列表，我来合并。")
lines.append("")
lines.append("`note` 字段留空即可，我会补上「为什么这么标」的说明（那是我的活，不是你的）。")
lines.append("")
lines.append("---")
lines.append("")
lines.append("## 待标注")
lines.append("")
lines.append("表中「带图片」「带工具」「含历史」是**请求的客观属性**，不是标签 —— ")
lines.append("它们描述这个请求实际带了什么，标签仍由你判断。")
lines.append("")
lines.append("| # | prompt | 客观属性 | label |")
lines.append("| --- | --- | --- | --- |")

for display_idx, src_idx in enumerate(order, start=1):
    prompt, has_image, has_tools, ctx, why = items[src_idx]
    attrs = []
    if has_image:
        attrs.append("带图片")
    if has_tools:
        attrs.append("带工具")
    if ctx == "long":
        attrs.append("含长上下文")
    elif ctx == "many":
        attrs.append("含多轮历史")
    attr_text = "、".join(attrs) if attrs else "纯文本"
    # 表格里的竖线要转义，避免把表格切断
    safe = prompt.replace("|", "\\|")
    lines.append(f"| {display_idx} | {safe} | {attr_text} |  |")

    rows.append({
        "id": f"o{display_idx:03d}",
        "prompt": prompt,
        # None -> JSON null。不能写空串：BenchSample.label 是 Option<TaskClass>，
        "label": None,
        "source": "owner",
        "note": "",
        "has_tools": has_tools,
        "has_image": has_image,
        "history": (
            [LONG_LOG] if ctx == "long"
            else (MANY_TURNS if ctx == "many" else [])
        ),
        "_why": why,
    })

lines.append("")
lines.append("---")
lines.append("")
lines.append("## 机读版（可选）")
lines.append("")
lines.append("如果你更愿意直接编辑 JSONL，改这一份就行：")
lines.append("`src-tauri/tests/fixtures/classify_bench_owner.jsonl`")
lines.append("（同一批 30 条，`label` 字段留空等着填）")
lines.append("")

os.makedirs(os.path.dirname(MD), exist_ok=True)
with open(MD, "w", encoding="utf-8", newline="\n") as fh:
    fh.write("\n".join(lines))

with open(JSONL, "w", encoding="utf-8", newline="\n") as fh:
    for r in rows:
        r.pop("_why")
        fh.write(json.dumps(r, ensure_ascii=False) + "\n")

# --- 覆盖度自检（只给脚本看）---
print(f"wrote {len(rows)} owner candidates")
print(f"  -> {MD}")
print(f"  -> {JSONL}")
from collections import Counter
print("覆盖度（不写进清单，防止泄露预期）:", dict(Counter(w for *_, w in items)))
print(f"带图片: {sum(1 for i in items if i[1])}")
print(f"带工具: {sum(1 for i in items if i[2])}")
print(f"长上下文: {sum(1 for i in items if i[3] == 'long')}")
print(f"多轮: {sum(1 for i in items if i[3] == 'many')}")
