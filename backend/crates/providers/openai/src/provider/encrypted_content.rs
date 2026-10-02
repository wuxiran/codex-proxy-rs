//! 明确拒绝后的密文恢复；正常路径保持 reasoning 不透明且不修改历史。

use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gateway_core::account::ProviderAccount;
use gateway_core::error::ProviderError;
use gateway_core::policy::ClientApiKeyId;
use gateway_core::upstream::UpstreamSendState;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::time::Instant;

use crate::transport::protocol::responses::CodexResponsesRequest;

type DigestBytes = [u8; 32];
type CacheKey = (DigestBytes, DigestBytes);
const INVALID_CONTENT_TTL: Duration = Duration::from_secs(10 * 60);
const MAX_INVALID_CONTENT_ENTRIES: usize = 1024;

#[derive(Clone, Default)]
pub(super) struct InvalidEncryptedContentCache {
    entries: Arc<Mutex<VecDeque<(CacheKey, Instant)>>>,
}

impl InvalidEncryptedContentCache {
    pub(super) fn known(&self, scope: Option<DigestBytes>) -> HashSet<DigestBytes> {
        let Some(scope) = scope else {
            return HashSet::new();
        };
        let Ok(mut entries) = self.entries.lock() else {
            return HashSet::new();
        };
        entries.retain(|(_, recorded)| recorded.elapsed() < INVALID_CONTENT_TTL);
        entries
            .iter()
            .filter_map(|((owner, digest), _)| (*owner == scope).then_some(*digest))
            .collect()
    }

    pub(super) fn remember(&self, scope: Option<DigestBytes>, request: &CodexResponsesRequest) {
        let Some(scope) = scope else { return };
        let Ok(mut entries) = self.entries.lock() else {
            return;
        };
        entries.retain(|(_, recorded)| recorded.elapsed() < INVALID_CONTENT_TTL);
        for digest in request
            .input()
            .iter()
            .filter_map(encrypted_reasoning_digest)
        {
            let key = (scope, digest);
            // 命中不续期，避免重复坏请求让失效知识永久存活。
            if entries.iter().any(|(existing, _)| *existing == key) {
                continue;
            }
            if entries.len() == MAX_INVALID_CONTENT_ENTRIES {
                entries.pop_front();
            }
            entries.push_back((key, Instant::now()));
        }
    }
}

pub(super) fn recovery_scope(
    request: &CodexResponsesRequest,
    account: &ProviderAccount,
    client_key: &ClientApiKeyId,
) -> Option<DigestBytes> {
    // 不用自动生成的会话 ID 或 prompt cache key 充当客户端会话证明。
    let session = request
        .client_session_id
        .as_deref()
        .or(request.client_conversation_id.as_deref())
        .or(request.client_thread_id.as_deref())
        .filter(|value| !value.is_empty())?;
    let mut hash = Sha256::new();
    for field in [
        account.id().as_str(),
        client_key.as_str(),
        request.model(),
        session,
        request.client_thread_id.as_deref().unwrap_or_default(),
    ] {
        hash.update(field.len().to_le_bytes());
        hash.update(field.as_bytes());
    }
    hash.update(account.revision().get().to_le_bytes());
    Some(hash.finalize().into())
}

pub(super) fn is_encrypted_content_rejection(error: &ProviderError) -> bool {
    error.send_state() != UpstreamSendState::Ambiguous
        && matches!(error.upstream_status(), None | Some(400))
        // 持久化 code 会归一大小写；恢复必须匹配原始结构化 code，不能匹配 message。
        && error.client_visible_upstream_error().and_then(|error| error.code())
            == Some("invalid_encrypted_content")
}

fn encrypted_reasoning_digest(item: &Value) -> Option<DigestBytes> {
    if item.get("type").and_then(Value::as_str) != Some("reasoning") {
        return None;
    }
    let encrypted = item
        .get("encrypted_content")?
        .as_str()
        .filter(|value| !value.is_empty())?;
    Some(Sha256::digest(encrypted.as_bytes()).into())
}

pub(super) fn can_recover(request: &CodexResponsesRequest) -> bool {
    // 网关不拥有完整历史，不能通过删除引用或压缩项来假装恢复上下文。
    if request
        .body()
        .get("previous_response_id")
        .is_some_and(|value| !value.is_null())
        || request
            .body()
            .get("conversation")
            .is_some_and(|value| !value.is_null())
        || !request.generate()
    {
        return false;
    }
    let mut calls = HashSet::new();
    let mut has_message = false;
    let mut has_encrypted_reasoning = false;
    for item in request.input() {
        match item.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                has_encrypted_reasoning |= encrypted_reasoning_digest(item).is_some()
            }
            Some("message") | None
                if item.get("role").and_then(Value::as_str).is_some()
                    && item
                        .get("content")
                        .is_some_and(|content| !content.is_null()) =>
            {
                has_message = true
            }
            Some("function_call" | "custom_tool_call") => {
                let Some(call_id) = item
                    .get("call_id")
                    .and_then(Value::as_str)
                    .filter(|id| !id.is_empty())
                else {
                    return false;
                };
                if !calls.insert(call_id) {
                    return false;
                }
            }
            Some("function_call_output" | "custom_tool_call_output") => {
                let Some(call_id) = item.get("call_id").and_then(Value::as_str) else {
                    return false;
                };
                if !calls.remove(call_id) {
                    return false;
                }
            }
            // 未知项可能携带服务端状态，保留原请求交由客户端恢复。
            _ => return false,
        }
    }
    has_message && has_encrypted_reasoning && calls.is_empty()
}

pub(super) fn strip_encrypted_reasoning(
    request: &mut CodexResponsesRequest,
    known: Option<&HashSet<DigestBytes>>,
) -> usize {
    if !can_recover(request) {
        return 0;
    }
    let Some(Value::Array(input)) = request.body_mut().get_mut("input") else {
        return 0;
    };
    let before = input.len();
    input.retain(|item| {
        !encrypted_reasoning_digest(item)
            .is_some_and(|digest| known.is_none_or(|known| known.contains(&digest)))
    });
    before - input.len()
}
