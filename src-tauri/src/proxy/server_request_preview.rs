//! Offline preview with opt-in, SELECT-only common-Chat context reconstruction.
//! No online classifier/search, admissions/reservations or free turn selection.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestPreviewInput {
    #[serde(deserialize_with = "decode_request")]
    pub request: ChatRequest,
    pub access_key_id: Option<String>,
    pub strategy: Option<RoutingStrategy>,
    #[serde(default)]
    pub include_session: bool,
    pub session_id: Option<String>,
}
fn decode_request<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<ChatRequest, D::Error> {
    let mut value = serde_json::Value::deserialize(deserializer)?;
    if let Some(object) = value.as_object_mut() {
        object
            .entry("stream")
            .or_insert(serde_json::Value::Bool(false));
    }
    serde_json::from_value(value).map_err(serde::de::Error::custom)
}
#[derive(Debug, Serialize)]
pub struct PreviewIntent {
    pub class: String,
    pub classifier: String,
    pub complexity: u8,
}
#[derive(Debug, Serialize)]
pub struct PreviewRow {
    pub provider_id: String,
    pub provider_name: String,
    pub model: String,
    pub configured_upstream: String,
    pub eligible: bool,
    pub exclusion_reason: Option<&'static str>,
    pub ordinal: Option<usize>,
    pub score: Option<f32>,
    pub breakdown: Option<crate::router::score::ScoreBreakdown>,
}
#[derive(Debug, Serialize)]
pub struct RequestPreview {
    pub schema_version: u8,
    pub mode: &'static str,
    pub estimated_prompt_tokens: u32,
    pub fixed_reserve_tokens: u32,
    pub trimmed_messages: usize,
    pub configured_strategy: RoutingStrategy,
    pub cost_policy: &'static str,
    pub order_semantics: &'static str,
    pub intent: Option<PreviewIntent>,
    pub gate_issue: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<PreviewContextInfo>,
    pub rows: Vec<PreviewRow>,
    pub limitations: Vec<&'static str>,
}
#[derive(Debug, Serialize)]
pub struct PreviewContextInfo {
    pub status: &'static str,
    pub scope: &'static str,
    pub history_messages: Option<usize>,
    pub replay_messages: Option<usize>,
    pub history_capped: Option<bool>,
    pub summary_present: Option<bool>,
    pub sticky_status: &'static str,
    pub sticky_provider_id: Option<String>,
    pub sticky_model: Option<String>,
}
impl GatewayState {
    pub async fn preview_routing_request(
        &self,
        input: RequestPreviewInput,
    ) -> Result<RequestPreview> {
        let mut req = input.request;
        if req.model.trim().is_empty()
            || req.messages.is_empty()
            || req.messages.len() > 128
            || serde_json::to_vec(&req).map_or(true, |v| v.len() > 262_144)
        {
            return Err(GatewayError::Protocol(
                "预演请求须包含模型和1–128条消息，总大小不超过256 KiB".into(),
            ));
        }
        let mut cfg = self.cfg_snapshot();
        if let Some(strategy) = input.strategy {
            cfg.routing_strategy = strategy;
        }
        let policy = self
            .cost_gate
            .effective(RequestCostPolicy::resolve(&cfg, &req.model));
        let key_blocked = if let Some(id) = input.access_key_id.as_deref() {
            let key = self
                .remote_access_keys
                .read()
                .iter()
                .find(|k| k.id == id && k.enabled)
                .cloned()
                .ok_or_else(|| GatewayError::Protocol("预演的访问Key ID不存在或已停用".into()))?;
            cfg.budget.enabled && !crate::budget::model_allowed(&key.allowed_models, &req.model)
        } else {
            false
        };
        // A selected ID simulates that existing key's namespace/policy. It is
        // not authentication or principal proof, and cannot bypass its guard.
        let mut context = input.include_session.then_some(PreviewContextInfo {
            status: if key_blocked {
                "key_policy_blocked"
            } else {
                "missing"
            },
            scope: if input.access_key_id.is_some() {
                "selected_access_key"
            } else {
                "local_unified"
            },
            history_messages: None,
            replay_messages: None,
            history_capped: None,
            summary_present: None,
            sticky_status: "none",
            sticky_provider_id: None,
            sticky_model: None,
        });
        let mut sticky = None;
        if let Some(info) = context.as_mut().filter(|_| !key_blocked) {
            let logical = crate::context::derive_session_id(&req, input.session_id.as_deref());
            let client = input
                .access_key_id
                .as_ref()
                .map(|id| format!("remote-key:{id}"));
            let scoped = crate::context::scoped_session_id(&logical, client.as_deref());
            if let Some(snapshot) = self
                .ctx
                .preview_context(&scoped, &req.messages, CONTEXT_HISTORY_LIMIT)
                .await?
            {
                info.status = "found";
                info.history_messages = Some(snapshot.history_messages);
                info.replay_messages = Some(snapshot.replay_messages);
                info.history_capped = Some(snapshot.capped);
                info.summary_present = Some(
                    snapshot
                        .session
                        .summary
                        .as_ref()
                        .is_some_and(|s| !s.trim().is_empty()),
                );
                req.messages = snapshot.prepared.messages;
                if let (Some(p), Some(m), Some(exp)) = (
                    snapshot.session.sticky_provider_id,
                    snapshot.session.sticky_model,
                    snapshot.session.sticky_expires_at,
                ) {
                    if exp > now_secs() {
                        info.sticky_provider_id = Some(p.clone());
                        info.sticky_model = Some(m.clone());
                        sticky = Some((p, m));
                    } else {
                        info.sticky_status = "inactive";
                    }
                }
            }
        }
        // Dispatch classifies reconstructed input before local window trimming.
        // Domain detection below still uses the post-trim shape, as dispatch does.
        let intent = if smart_mode_active(&cfg, &req.model) {
            Some(
                crate::intellect::classify(
                    &crate::intellect::ClassifyInput {
                        messages: &req.messages,
                        media: crate::media::Media::of(&req),
                        has_tools: req.tools.is_some(),
                        requested_model: &req.model,
                    },
                    &cfg.smart_routing,
                    None,
                )
                .await,
            )
        } else {
            None
        };
        let all = match self.router.resolve_typed_with(
            &req.model,
            &self.legacy_providers(),
            ModelType::Chat,
            smart_mode_enabled(&cfg),
        ) {
            Ok(v) => v,
            Err(GatewayError::ModelNotFound(_)) if policy.is_free() => Vec::new(),
            Err(e) => return Err(e),
        };
        let mut reasons = std::collections::HashMap::new();
        let key =
            |c: &crate::router::score::Candidate| (c.provider.id.clone(), c.model.alias.clone());
        let mut candidates = all.clone();
        let mut gate_issue = None;
        if key_blocked {
            gate_issue = Some("access_key_model_not_allowed");
            for c in candidates.drain(..) {
                reasons.insert(key(&c), "access_key_model_not_allowed");
            }
        } else if policy.is_free()
            && crate::cost_policy::wire_shape_issue(
                &serde_json::to_value(&req).map_err(|e| GatewayError::Protocol(e.to_string()))?,
                &req.model,
            )
            .is_some()
        {
            gate_issue = Some("free_wire_shape_unverified");
            for c in candidates.drain(..) {
                reasons.insert(key(&c), "free_wire_shape_unverified");
            }
        } else if let Err(e) = self.cost_gate.filter(&mut candidates, policy).await {
            if matches!(e, GatewayError::FreePolicyRejected { .. }) {
                gate_issue = Some("free_policy_rejected");
            } else {
                return Err(e);
            }
        }
        for c in &all {
            if !candidates.iter().any(|v| key(v) == key(c)) {
                reasons.entry(key(c)).or_insert("cost_policy_not_retained");
            }
        }
        let mut supported = Vec::new();
        for c in candidates {
            // Same local runtime configuration resolver as actual dispatch.
            // It does not prepare auth, launch a CLI or prove account readiness.
            let reason = if req.stream && !c.model.supports_stream {
                Some("stream_transport_unsupported")
            } else if let Some(id) = c.provider.runtime_id.as_deref() {
                match crate::agent_upstream::runtime::resolve_chat_runtime(
                    self.db.pool(),
                    &self.adapters,
                    id,
                )
                .await
                {
                    Err(_) => Some("runtime_unavailable"),
                    Ok(runtime)
                        if req.stream
                            && !matches!(
                                runtime,
                                crate::agent_upstream::runtime::ChatRuntime::WorkBuddy { .. }
                                    | crate::agent_upstream::runtime::ChatRuntime::Qoder { .. }
                            ) =>
                    {
                        Some("stream_transport_unsupported")
                    }
                    Ok(_) => None,
                }
            } else {
                None
            };
            if let Some(reason) = reason {
                reasons.insert(key(&c), reason);
            } else {
                supported.push(c);
            }
        }
        candidates = supported;
        let calibration = self.calibration_snapshot().await;
        let fixed_reserve = fixed_context_reserve(&req, &candidates);
        let before = req.messages.len();
        if let Some(max) = max_message_budget(&candidates, fixed_reserve) {
            let ratio = conservative_ratio(&candidates, &calibration);
            if scaled(estimate_message_tokens(&req.messages), ratio) > max {
                req.messages = trim_to_budget(req.messages, (max as f64 / ratio).floor() as u32);
            }
        }
        let message_tokens = estimate_message_tokens(&req.messages);
        let media = crate::media::Media::of(&req);
        let required = required_capabilities(&req);
        candidates.retain(|c| {
            let reason = if crate::media::carry(c.provider.dialect, &media).is_err() {
                Some("protocol_media_unsupported")
            } else if !crate::router::score::satisfies_hard_constraints(c, &required) {
                Some("model_capability_missing")
            } else if !candidate_supports_context(c, message_tokens, fixed_reserve, &calibration) {
                Some("context_window_insufficient")
            } else {
                None
            };
            if let Some(reason) = reason {
                reasons.insert(key(c), reason);
                false
            } else {
                true
            }
        });
        let classify = crate::intellect::ClassifyInput {
            messages: &req.messages,
            media,
            has_tools: req.tools.is_some(),
            requested_model: &req.model,
        };
        let domain = crate::intellect::detect_domain(&classify);
        let mut decision = self.router.rank_with_snapshot(
            candidates,
            &cfg,
            required,
            sticky.as_ref().map(|(p, m)| (p.as_str(), m.as_str())),
            intent.as_ref().map(|i| i.class),
            message_tokens,
            domain,
        );
        let ranked = &mut decision.candidates;
        if let Some(active) = self.active.read().clone() {
            if let Some(at) = ranked.iter().position(|c| c.provider.id == active) {
                let c = ranked.remove(at);
                ranked.insert(0, c);
            }
        }
        if let (Some(info), Some((p, m))) = (context.as_mut(), sticky.as_ref()) {
            info.sticky_status = if ranked
                .iter()
                .any(|c| c.provider.id == *p && c.model.upstream == *m)
            {
                "eligible"
            } else {
                "excluded"
            };
        }
        // Do not call FreeSelector::select: it advances weighted credits/turns.
        let explained = self.router.explain_ranked_request(&decision);
        let ranked = &decision.candidates;
        let mut rows = Vec::new();
        for c in &all {
            let at = ranked.iter().position(|v| key(v) == key(c));
            let explanation = at.and_then(|i| explained.get(i));
            let score = if at.is_some() && policy.is_free() {
                Some(
                    crate::cost_policy::FreeSelector::weight(
                        &self.health.get(&c.provider.id, &c.model.upstream),
                        self.router.quota_headroom(&c.provider, &c.model),
                    )
                    .value as f32,
                )
            } else {
                explanation.map(|e| e.score)
            };
            rows.push(PreviewRow {
                provider_id: c.provider.id.clone(),
                provider_name: c.provider.name.clone(),
                model: c.model.alias.clone(),
                configured_upstream: c.model.upstream.clone(),
                eligible: at.is_some(),
                exclusion_reason: if at.is_none() {
                    Some(
                        *reasons
                            .get(&key(c))
                            .unwrap_or(&"health_quota_or_custom_rule"),
                    )
                } else {
                    None
                },
                ordinal: if policy.is_free() {
                    None
                } else {
                    at.map(|i| i + 1)
                },
                score,
                breakdown: if policy.is_free() {
                    None
                } else {
                    explanation.map(|e| e.breakdown.clone())
                },
            });
        }
        rows.sort_by(|a, b| {
            b.eligible
                .cmp(&a.eligible)
                .then(a.ordinal.cmp(&b.ordinal))
                .then(a.provider_id.cmp(&b.provider_id))
                .then(a.model.cmp(&b.model))
        });
        Ok(RequestPreview {
            schema_version: 1,
            mode: if input.include_session {
                "offline_session_context"
            } else {
                "offline_supplied_messages"
            },
            estimated_prompt_tokens: message_tokens,
            fixed_reserve_tokens: fixed_reserve,
            trimmed_messages: before.saturating_sub(req.messages.len()),
            configured_strategy: cfg.routing_strategy,
            cost_policy: if policy.is_free() {
                "free_only"
            } else {
                "unrestricted"
            },
            order_semantics: if policy.is_free() {
                "free_weight_only_no_turn_prediction"
            } else {
                if input.include_session {
                    "current_rank_with_session_stickiness"
                } else {
                    "current_rank_no_session_stickiness"
                }
            },
            intent: intent.map(|i| PreviewIntent {
                class: i.class.code().into(),
                classifier: i.classifier.code().into(),
                complexity: i.complexity,
            }),
            gate_issue,
            context,
            rows,
            limitations: vec![
                if input.include_session {
                    "显式读取所选本机会话的同一事务快照；按普通Chat历史上限重建，缺失不创建。只返回计数／标志，不返回历史正文。"
                } else {
                    "仅本次传入消息；不重建服务端会话或模拟会话粘性。"
                },
                "未运行Jev、搜索、提示词改写或远程压缩；仅模拟本地裁剪。",
                "不预占预算、执行许可或免费轮次；实际发送会重新核验。",
                "分数为当前预演，不是历史决策；协议特有控制与账号最终物理参数仍需发送链核验。",
            ],
        })
    }
}
