#!/usr/bin/env node
// C3 协议契约夹具生成器。
//
// 【为什么要生成器而不是手敲 5 个目录】**：夹具必须与它声称的形状同源。
// 手敲的 JSON 会慢慢和代码里的转换器漂移，而漂移是静默的 ——
// 测试全绿，契约已经不对了。生成器让「样本从哪来」这件事可复核。
//
// 【本生成器产出的样本是什么，以及**不是什么**】
// 产出的是**官方 API 文档记载的形状**（`kind: "documented-example"`），
// 不是真实抓包（`kind: "vendor-capture"`）。
//
// 这个区别是卡片的核心关切：原文说手写夹具「能证明解析器逻辑对，
// **不能证明上游没改版**」。所以每个目录的 `provenance.json` 里
// **必须**如实写明 kind，并有专门用例断言「kind 不是 vendor-capture 时，
// note 里必须写明它证明不了什么」—— 不让后来者把它误读成真实抓包。
//
// 用法：node scripts/gen-protocol-contracts.mjs
// 生成物进版本库；重跑应当得到逐字节相同的结果（用例会比对）。

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = join(fileURLToPath(new URL(".", import.meta.url)), "..");
const BASE = join(ROOT, "src-tauri", "tests", "fixtures", "protocol_contracts");

const RETRIEVED_AT = "2026-10-06";

/**
 * 每个协议的样本。`samples` 里每条的 `payload` 都是**该协议真实形态**的
 * 完整对象（不是片段）：片段能过解析器，但证明不了「整条报文里多余的字段
 * 会不会把它打挂」。
 *
 * 脱敏约定（`check-fixture-redaction.mjs` 会扫）：
 * - 凭据一律写 `<REDACTED_KEY>`
 * - 身份信息一律写 `<REDACTED_ID>` / `<REDACTED_EMAIL>`
 * - 提示词与回复一律写固定的占位句子，不放任何真实内容
 */
const CONTRACTS = {
  openai: {
    source: "https://platform.openai.com/docs/api-reference/chat",
    note:
      "按官方 Chat Completions 文档记载的字段名构造。" +
      "证明不了 OpenAI 是否已改版；真实抓包需带 key 打一次线上接口。",
    samples: [
      {
        kind: "request",
        payload: {
          model: "gpt-4o",
          messages: [
            { role: "system", content: "You are a helpful assistant." },
            { role: "user", content: "Hello" },
          ],
          temperature: 0.7,
          max_tokens: 64,
          stream: false,
        },
      },
      {
        kind: "response",
        payload: {
          id: "chatcmpl-<REDACTED_ID>",
          object: "chat.completion",
          created: 1759700000,
          model: "gpt-4o",
          choices: [
            {
              index: 0,
              message: { role: "assistant", content: "Hi there" },
              finish_reason: "stop",
              logprobs: null,
            },
          ],
          usage: {
            prompt_tokens: 9,
            completion_tokens: 2,
            total_tokens: 11,
          },
        },
      },
      {
        kind: "stream_chunk",
        payload: {
          id: "chatcmpl-<REDACTED_ID>",
          object: "chat.completion.chunk",
          created: 1759700000,
          model: "gpt-4o",
          choices: [{ index: 0, delta: { role: "assistant" }, finish_reason: null }],
        },
      },
      {
        kind: "stream_chunk",
        payload: {
          id: "chatcmpl-<REDACTED_ID>",
          object: "chat.completion.chunk",
          created: 1759700000,
          model: "gpt-4o",
          choices: [{ index: 0, delta: { content: "Hi there" }, finish_reason: null }],
        },
      },
      {
        kind: "stream_chunk",
        payload: {
          id: "chatcmpl-<REDACTED_ID>",
          object: "chat.completion.chunk",
          created: 1759700000,
          model: "gpt-4o",
          choices: [{ index: 0, delta: {}, finish_reason: "stop" }],
          usage: { prompt_tokens: 9, completion_tokens: 2, total_tokens: 11 },
        },
      },
    ],
  },

  anthropic: {
    source: "https://docs.anthropic.com/en/api/messages",
    note:
      "按官方 Messages API 文档记载的字段名构造。" +
      "证明不了 Anthropic 是否已改版；真实抓包需带 key 打一次线上接口。",
    samples: [
      {
        kind: "request",
        payload: {
          model: "claude-3-5-sonnet-latest",
          max_tokens: 64,
          // `system` 与 `temperature` 都是 Messages API 的真实字段。
          // 第一版漏了，被反向判据抓到（同 responses 那一处）。
          system: "You are a helpful assistant.",
          temperature: 0.7,
          messages: [{ role: "user", content: "Hello" }],
        },
      },
      {
        kind: "response",
        payload: {
          id: "msg_<REDACTED_ID>",
          type: "message",
          role: "assistant",
          model: "claude-3-5-sonnet-latest",
          content: [{ type: "text", text: "Hi there" }],
          stop_reason: "end_turn",
          stop_sequence: null,
          usage: { input_tokens: 9, output_tokens: 2 },
        },
      },
      {
        kind: "stream_event",
        event: "message_start",
        payload: {
          type: "message_start",
          message: {
            id: "msg_<REDACTED_ID>",
            type: "message",
            role: "assistant",
            model: "claude-3-5-sonnet-latest",
            content: [],
            stop_reason: null,
            usage: { input_tokens: 9, output_tokens: 0 },
          },
        },
      },
      {
        kind: "stream_event",
        event: "content_block_delta",
        payload: {
          type: "content_block_delta",
          index: 0,
          delta: { type: "text_delta", text: "Hi there" },
        },
      },
      {
        kind: "stream_event",
        event: "message_delta",
        payload: {
          type: "message_delta",
          delta: { stop_reason: "end_turn", stop_sequence: null },
          usage: { output_tokens: 2 },
        },
      },
      {
        kind: "stream_event",
        event: "message_stop",
        payload: { type: "message_stop" },
      },
    ],
  },

  gemini: {
    source:
      "https://ai.google.dev/api/generate-content 与 " +
      "https://google-gemini.github.io/gemini-cli/docs/get-started/configuration.html",
    note:
      "按官方 generateContent 文档记载的字段名构造。" +
      "证明不了 Google 是否已改版；真实抓包需带 key 打一次线上接口。",
    samples: [
      {
        kind: "request",
        payload: {
          // `systemInstruction` 是 generateContent 的真实字段（同 responses /
          // anthropic 那两处的漏项，都是被反向判据抓到的）。
          systemInstruction: { parts: [{ text: "You are a helpful assistant." }] },
          contents: [{ role: "user", parts: [{ text: "Hello" }] }],
          generationConfig: { temperature: 0.7, maxOutputTokens: 64 },
        },
      },
      {
        kind: "response",
        payload: {
          candidates: [
            {
              content: { role: "model", parts: [{ text: "Hi there" }] },
              finishReason: "STOP",
              index: 0,
            },
          ],
          usageMetadata: {
            promptTokenCount: 9,
            candidatesTokenCount: 2,
            totalTokenCount: 11,
          },
          modelVersion: "gemini-2.5-pro",
        },
      },
      {
        kind: "stream_chunk",
        payload: {
          candidates: [
            { content: { role: "model", parts: [{ text: "Hi there" }] }, index: 0 },
          ],
          modelVersion: "gemini-2.5-pro",
        },
      },
      {
        kind: "stream_chunk",
        payload: {
          candidates: [
            {
              content: { role: "model", parts: [] },
              finishReason: "STOP",
              index: 0,
            },
          ],
          usageMetadata: {
            promptTokenCount: 9,
            candidatesTokenCount: 2,
            totalTokenCount: 11,
          },
          modelVersion: "gemini-2.5-pro",
        },
      },
    ],
  },

  ollama: {
    source: "https://github.com/ollama/ollama/blob/main/docs/api.md",
    note:
      "按官方 /api/chat 文档记载的字段名构造。" +
      "证明不了 Ollama 是否已改版；真实抓包需本机跑一次 ollama serve。",
    samples: [
      {
        kind: "request",
        payload: {
          model: "qwen2.5:7b",
          messages: [{ role: "user", content: "Hello" }],
          stream: false,
          options: { num_ctx: 4096, temperature: 0.7 },
        },
      },
      {
        kind: "response",
        payload: {
          model: "qwen2.5:7b",
          created_at: "2026-10-06T00:00:00Z",
          message: { role: "assistant", content: "Hi there" },
          done: true,
          done_reason: "stop",
          total_duration: 1234567,
          load_duration: 12345,
          prompt_eval_count: 9,
          prompt_eval_duration: 234567,
          eval_count: 2,
          eval_duration: 345678,
        },
      },
      {
        kind: "stream_chunk",
        payload: {
          model: "qwen2.5:7b",
          created_at: "2026-10-06T00:00:00Z",
          message: { role: "assistant", content: "Hi there" },
          done: false,
        },
      },
      {
        kind: "stream_chunk",
        payload: {
          model: "qwen2.5:7b",
          created_at: "2026-10-06T00:00:00Z",
          message: { role: "assistant", content: "" },
          done: true,
          done_reason: "stop",
          prompt_eval_count: 9,
          eval_count: 2,
        },
      },
    ],
  },

  responses: {
    source: "https://platform.openai.com/docs/api-reference/responses",
    note:
      "按官方 Responses API 文档记载的字段名构造。" +
      "证明不了 OpenAI 是否已改版；真实抓包需带 key 打一次线上接口。",
    samples: [
      {
        kind: "request",
        payload: {
          model: "gpt-4o",
          // `instructions` / `max_output_tokens` / `temperature` 都是
          // Responses API 的真实字段（不是我们造的）。
          // 第一版样本里漏了它们，被 `请求样本的字段名与出站转换一致`
          // 的反向判据抓到「出站体多出了样本里没有的键」——
          // 这正是那条反向断言存在的意义：只写正向的话，
          // 样本漏字段时测试照样绿，而契约已经对不上了。
          instructions: "You are a helpful assistant.",
          input: [
            {
              role: "user",
              content: [{ type: "input_text", text: "Hello" }],
            },
          ],
          max_output_tokens: 64,
          temperature: 0.7,
          stream: false,
        },
      },
      {
        kind: "response",
        payload: {
          id: "resp_<REDACTED_ID>",
          object: "response",
          created_at: 1759700000,
          status: "completed",
          model: "gpt-4o",
          output: [
            {
              id: "msg_<REDACTED_ID>",
              type: "message",
              role: "assistant",
              status: "completed",
              content: [{ type: "output_text", text: "Hi there", annotations: [] }],
            },
          ],
          usage: {
            input_tokens: 9,
            output_tokens: 2,
            total_tokens: 11,
            input_tokens_details: { cached_tokens: 0 },
            output_tokens_details: { reasoning_tokens: 0 },
          },
        },
      },
      {
        kind: "stream_event",
        event: "response.output_text.delta",
        payload: { type: "response.output_text.delta", delta: "Hi there" },
      },
      {
        kind: "stream_event",
        event: "response.completed",
        payload: {
          type: "response.completed",
          response: { id: "resp_<REDACTED_ID>", status: "completed", output: [] },
        },
      },
    ],
  },
};

/** 写一个目录下的全部文件。排序固定，保证重跑得到逐字节相同的结果。 */
function emit(name, contract) {
  const dir = join(BASE, name);
  mkdirSync(dir, { recursive: true });

  writeFileSync(
    join(dir, "provenance.json"),
    JSON.stringify(
      {
        // kind 是**机器判据**，不是注释：用例会读它。
        // 只允许这三个取值，且非 vendor-capture 时 note 必须写明局限。
        kind: "documented-example",
        source: contract.source,
        retrieved_at: RETRIEVED_AT,
        note: contract.note,
      },
      null,
      2,
    ) + "\n",
    "utf8",
  );

  writeFileSync(
    join(dir, "samples.jsonl"),
    contract.samples.map((s) => JSON.stringify(s)).join("\n") + "\n",
    "utf8",
  );
}

mkdirSync(BASE, { recursive: true });
for (const [name, contract] of Object.entries(CONTRACTS)) {
  emit(name, contract);
}
console.log(`已生成 ${Object.keys(CONTRACTS).length} 个协议契约目录 → ${BASE}`);
