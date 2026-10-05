"""生成 src-tauri/tests/fixtures/classify_bench.jsonl 的 seeded 部分。

为什么用脚本而不是手写：长上下文样本需要精确控制字符数（approx_tokens 对
Content::Text 是 chars().count()，阈值 8000），手写数不清；JSONL 也容易手抖
写成非法行。脚本同时保证 id 唯一、prompt 不重复。

owner 部分由本人补写，脚本不生成 —— 卡片明确写了不许用模型标注顶替人工标注。
"""

import json
import os

OUT = r"D:\Java\GitHub\llm-auto\llm-gateway\src-tauri\tests\fixtures\classify_bench.jsonl"

rows = []


def add(sid, prompt, label, note, has_tools=False, has_image=False, history=None):
    rows.append({
        "id": sid,
        "prompt": prompt,
        "label": label,
        "source": "seeded",
        "note": note,
        "has_tools": has_tools,
        "has_image": has_image,
        "history": history or [],
    })


# ---------- A. 极短指令 → simple ----------
# 判据：一句话能答完，没有约束、没有背景、不需要权衡。
add("b001", "把变量 x 改名成 count",
    "simple", "纯改名，零推理。启发式：12 字 + 无代码痕迹 = 5 分，落 Simple。")
add("b002", "你好",
    "simple", "寒暄，2 字。任何把它判成 reasoning 的分类器都是错的。")
add("b003", "格式化这段 JSON",
    "simple", "单一机械操作，不需要理解内容语义。")
add("b004", "翻译成英文：今天天气不错",
    "simple", "短句翻译，有唯一正确答案。")
add("b005", "1+1 等于几",
    "simple", "常识算术。注意它与 b011「证明勾股定理」的区别：算得出 vs 要推导。")
add("b006", "删掉多余的空格",
    "simple", "纯文本规整，无歧义。")

# ---------- B. 有客观答案的技术题（simple / reasoning 的边界）----------
# 卡片点名这一类「本身就是边界争议」，所以每条 note 都要写清为什么这么标。
add("b007", "写一个快排",
    "simple",
    "边界样本，标 simple 的理由：算法有唯一公认写法，不需要在多个方案间权衡，"
    "也不需要读上下文。标 reasoning 也能自圆其说（写正确实现要小心边界）。"
    "本基准集取「不需要用户现有代码/环境的上下文」为判据 → simple。")
add("b008", "解析一下这段代码的时间复杂度",
    "reasoning",
    "边界样本，标 reasoning 的理由：与 b007 相反，这里没有标准答案可查 —— "
    "复杂度取决于那段代码的具体结构，必须先读懂再推导。"
    "两条一起构成「同样有客观答案，但一条要读输入、一条不要」的对照。")
add("b009", "Python 里怎么把 list 转成 dict",
    "simple", "API 用法查询，文档里有唯一答案。")
add("b010", "解释一下 TCP 三次握手",
    "simple",
    "教科书知识，面试有标准答案。注意它含「解释一下」这个启发式关键词 ——"
    "标 simple 是为了检验关键词表会不会把这类查询误判成推理。")
add("b011", "证明勾股定理",
    "reasoning", "含「证明」，且真的需要一步步推导，不是查表能给答案的。")
add("b012", "把这段 SQL 改成用 JOIN 写",
    "simple", "改写目标明确、写法唯一，属于机械变换。")

# ---------- C. 含定位/根因/排查字样的线上问题 → reasoning ----------
# 卡片点名：Jev 实测会把这类高置信度判成 simple（0.3.0验证记录:132-166）。
# 这一组是本基准集里最有价值的部分 —— 它们专门用来暴露那个已知失误。
add("b013", "线上报 502，帮我定位一下是网关还是上游的问题",
    "reasoning",
    "Jev 已知失误样本类型：含「定位」但句子短，决策模型倾向判 simple。"
    "人工判 reasoning —— 判断 502 来自网关还是上游必须结合拓扑、日志与"
    "当时配置，是排查不是查询。")
add("b014", "这个接口偶尔超时，排查一下根因",
    "reasoning", "「偶尔」意味着要区分稳态与抖动，需要假设-验证循环。")
add("b015", "日志里出现这个错误，定位是哪一行抛的",
    "reasoning", "要顺着调用栈回溯，属于多步推理。")
add("b016", "数据库连接池被打满，排查原因",
    "reasoning",
    "Jev 已知失误样本类型：9 个字，极短，但排查连接池泄漏要看连接生命周期、"
    "慢查询、事务边界三处，不是查询。")
add("b017", "为什么这个查询没走索引",
    "reasoning", "含「为什么」+ 需要读执行计划，两个推理信号。")
add("b018", "服务重启后内存一直涨，帮我看看",
    "reasoning",
    "「一直涨」是时间序列现象，要区分泄漏与正常缓存增长，需要推理。")

# ---------- D. 含图片标记 → vision ----------
# 硬规则：media.image → Vision，不看文本内容。这一组验证硬规则优先级
# 高于 Jev 与启发式（哪怕文案写着「帮我改个名字」这种 simple 活）。
add("b019", "这里为什么报错", "vision",
    "带截图问报错。文本只有 6 字，若只看文本会被判 simple —— "
    "这正是硬规则必须排在分类最前面的理由。", has_image=True)
add("b020", "把这个设计图还原成 HTML", "vision",
    "带图 + 含「设计」关键词。硬规则判 vision 优先于关键词判 reasoning。", has_image=True)
add("b021", "看一下这个报错截图", "vision", "纯看图。", has_image=True)
add("b022", "这张表里的数据对吗", "vision", "要先读图才能回答。", has_image=True)
add("b023", "把这张图里的文字提取出来", "vision", "OCR 类任务，仍然属于视觉输入。", has_image=True)
add("b024", "图里的按钮颜色改一下", "vision", "带图，硬规则优先。", has_image=True)
add("b025", "这个流程图有没有逻辑问题", "vision",
    "带图 + 含「问题」。验证硬规则仍判 vision（不是 reasoning）。", has_image=True)

# ---------- E. 长上下文 + 工具调用 → reasoning ----------
# 硬规则：has_tools && (approx_tokens > 8000 || messages.len() > 6)
#
# 注意 approx_tokens 的口径是 `chars / 3 + 4`（domain/model.rs），不是
# chars 本身。第一版日志只有 220 行 ≈ 17631 字符 = 5881 token，
# **够不到 8000 的门槛**，于是「长上下文」那两条实际走的是启发式，
# 而这组用例看起来像在测硬规则。400 行 ≈ 32000 字符 ≈ 10671 token 才够。
LONG_LOG = "\n".join(
    f"2026-10-05T10:{i // 60:02d}:{i % 60:02d}Z INFO  worker[{i % 8}] "
    f"processed batch={i} rows={i * 37 % 500} elapsed_ms={i * 13 % 900}"
    for i in range(400)
)

add("b026", "根据上面的构建日志，这次失败的原因是什么", "reasoning",
    "长上下文（日志 220 行，远超 8000 字符）+ 带工具。",
    has_tools=True, history=[LONG_LOG])
add("b027", "把上面所有报错的调用链整理出来", "reasoning",
    "长上下文 + 工具，且要求跨多行聚合。", has_tools=True, history=[LONG_LOG])
add("b028", "继续", "reasoning",
    "多轮（history 7 条）+ 工具。「继续」两个字单独看是 simple，"
    "但真实的智能体请求里它承接了前面 7 轮的上下文，"
    "判 simple 会把长链路任务派给不思考的模型。",
    has_tools=True,
    history=["读一下 src/a.rs", "改掉里面的 unwrap", "跑测试", "测试红了",
             "看下报错", "修一下", "再跑一次"])
add("b029", "下一步做什么", "reasoning",
    "同上：多轮 + 工具的续跑请求，语义重量在上下文里而不在这 5 个字里。",
    has_tools=True,
    history=["列出所有 TODO", "按优先级排序", "先做第一个", "那个改完了",
             "现在看第二个", "第二个卡住了", "换个思路"])
add("b030", "重构这个模块，把职责拆开", "reasoning",
    "多轮 + 工具 + 含「重构」。", has_tools=True,
    history=["看下 src/router", "它多大", "有哪些函数", "哪个最复杂",
             "为什么复杂", "有测试吗", "测试覆盖多少"])
add("b031", "所有测试都过了，帮我确认一下没问题", "reasoning",
    "多轮 + 工具。「确认一下」看似简单，实际要做的是复核整条改动链路。",
    has_tools=True,
    history=["审计一遍这个 crate", "找出所有 unwrap", "改成问号",
             "跑了测试吗", "跑了", "全绿吗", "全绿"])
add("b032", "继续上一个任务", "reasoning",
    "多轮 + 工具 + 含「任务」。", has_tools=True,
    history=["分析这个 CR", "有几个问题", "列出来", "先修第一个",
             "修好了", "再看第二个", "第二个要不要改"])

# ---------- 落盘 ----------
# 自检：id 唯一、prompt 唯一、标签合法、seeded 条数达标。
assert len({r["id"] for r in rows}) == len(rows), "id 有重复"
assert len({r["prompt"] for r in rows}) == len(rows), "prompt 有重复"
assert all(r["label"] in ("simple", "vision", "reasoning") for r in rows), "标签非法"

labels = {}
for r in rows:
    labels[r["label"]] = labels.get(r["label"], 0) + 1
assert min(labels.values()) >= 5, f"有类别不足 5 条: {labels}"

os.makedirs(os.path.dirname(OUT), exist_ok=True)
with open(OUT, "w", encoding="utf-8", newline="\n") as fh:
    for r in rows:
        fh.write(json.dumps(r, ensure_ascii=False) + "\n")

print(f"wrote {len(rows)} seeded samples -> {OUT}")
print(f"label distribution: {labels}")
print(f"with image: {sum(1 for r in rows if r['has_image'])}")
print(f"with tools: {sum(1 for r in rows if r['has_tools'])}")
long_hist = [r["id"] for r in rows if any(len(h) > 8000 for h in r["history"])]
many_turns = [r["id"] for r in rows if len(r["history"]) > 6]
print(f"history > 8000 chars: {long_hist}")
print(f"history turns > 6:    {many_turns}")
# 复核硬规则门槛：approx_tokens = chars / 3 + 4，必须真的越过 8000。
for r in rows:
    if any(len(h) > 8000 for h in r["history"]):
        chars = sum(len(h) for h in r["history"]) + len(r["prompt"])
        approx = -(-chars // 3) + 4
        assert approx > 8000, f"{r['id']} 只有约 {approx} token，够不到硬规则的 8000"
        print(f"  {r['id']}: {chars} chars -> about {approx} tokens")
