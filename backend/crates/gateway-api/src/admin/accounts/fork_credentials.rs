//! fork 保留的凭据轮换入口 `/api/admin/accounts/rotate` 的请求体与校验。
//!
//! 上游已移除该入口；fork 仍用它做观澜复活、钉 turn-state 与自动续期，以及手工轮换 OAuth token。

use super::*;

/// 与上游 `credentials::parse_provider` 同口径；上游函数私有，这里保留一份。
fn parse_provider(value: &str) -> Result<ProviderKind, WireValidationError> {
    ProviderKind::new(value.trim().to_owned()).map_err(|_| WireValidationError::new("provider"))
}

/// 账号级 state 的自动续期设置；`enabled: false` 关闭并清除参数。
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TurnStateAutoHuntRequest {
    pub enabled: bool,
    #[serde(default)]
    pub model_id: String,
    #[serde(default)]
    pub attempts: u8,
    #[serde(default)]
    pub include_direct: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RotateAccountRequest {
    pub guanlan_revive: Option<bool>,
    pub pin_turn_state: Option<bool>,
    pub turn_state_auto_hunt: Option<TurnStateAutoHuntRequest>,
    pub provider: String,
    pub account_id: String,
    pub access_token: Option<String>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub transport: Option<String>,
    pub settings: Option<UpdateAccountRequest>,
}

impl RotateAccountRequest {
    pub fn validate(&self) -> Result<(), WireValidationError> {
        if parse_provider(&self.provider)?.as_str() != "openai" {
            return Err(WireValidationError::new("provider"));
        }
        require_account_id(&self.account_id, "accountId")?;
        if let Some(settings) = &self.settings {
            settings.validate()?;
            if settings.account_id != self.account_id {
                return Err(WireValidationError::new("settings.accountId"));
            }
            // 连接设置只走 /accounts/update；轮换附带的设置只接受调度与分组字段。
            if settings.connection.is_some() {
                return Err(WireValidationError::new("settings.connection"));
            }
        }
        if self.guanlan_revive.is_some() {
            if self.guanlan_revive != Some(true)
                || self.pin_turn_state.is_some()
                || self.turn_state_auto_hunt.is_some()
                || self.access_token.is_some()
                || self.refresh_token.is_some()
                || self.id_token.is_some()
                || self.base_url.is_some()
                || self.api_key.is_some()
                || self.transport.is_some()
                || self.settings.is_some()
            {
                return Err(WireValidationError::new("guanlanRevive"));
            }
            return Ok(());
        }
        if self.pin_turn_state.is_some() || self.turn_state_auto_hunt.is_some() {
            if self.access_token.is_some()
                || self.refresh_token.is_some()
                || self.id_token.is_some()
                || self.base_url.is_some()
                || self.api_key.is_some()
                || self.transport.is_some()
            {
                return Err(WireValidationError::new("pinTurnState"));
            }
            if let Some(auto) = &self.turn_state_auto_hunt
                && auto.enabled
                && (auto.model_id.trim().is_empty() || !(1..=20).contains(&auto.attempts))
            {
                return Err(WireValidationError::new("turnStateAutoHunt"));
            }
            return Ok(());
        }
        if let Some(base_url) = &self.base_url {
            if self.access_token.is_some()
                || self.refresh_token.is_some()
                || self.id_token.is_some()
                || base_url.is_empty()
                || base_url.len() > 2048
                || !matches!(self.transport.as_deref(), Some("http" | "prefer_websocket"))
                || self.api_key.as_ref().is_some_and(|key| {
                    key.is_empty()
                        || key.len() > 16 * 1024
                        || !key.bytes().all(|byte| byte.is_ascii_graphic())
                })
            {
                return Err(WireValidationError::new("credential"));
            }
            Ok(())
        } else {
            if self.api_key.is_some() || self.transport.is_some() {
                return Err(WireValidationError::new("credential"));
            }
            validate_oauth_material(
                self.access_token
                    .as_deref()
                    .ok_or_else(|| WireValidationError::new("accessToken"))?,
                self.refresh_token.as_deref(),
                self.id_token.as_deref(),
            )
        }
    }

    pub(super) fn into_command(
        self,
        context: gateway_admin::model::MutationContext,
    ) -> Result<RotateCredential, WireValidationError> {
        self.validate()?;
        let mut material = Map::new();
        if self.guanlan_revive == Some(true) {
            material.insert("guanlan_revive".to_owned(), Value::Bool(true));
        } else if self.pin_turn_state.is_some() || self.turn_state_auto_hunt.is_some() {
            if let Some(enabled) = self.pin_turn_state {
                material.insert("pin_turn_state".to_owned(), Value::Bool(enabled));
            }
            if let Some(auto) = self.turn_state_auto_hunt {
                material.insert(
                    "turn_state_auto_hunt".to_owned(),
                    serde_json::json!({
                        "enabled": auto.enabled,
                        "model": auto.model_id,
                        "attempts": auto.attempts,
                        "include_direct": auto.include_direct,
                    }),
                );
            }
        } else if let Some(base_url) = self.base_url {
            material.insert("base_url".to_owned(), Value::String(base_url));
            material.insert(
                "transport".to_owned(),
                self.transport.map_or(Value::Null, Value::String),
            );
            if let Some(key) = self.api_key {
                material.insert("api_key".to_owned(), Value::String(key));
            }
        } else {
            material.insert(
                "access_token".to_owned(),
                self.access_token.map_or(Value::Null, Value::String),
            );
            material.insert(
                "refresh_token".to_owned(),
                self.refresh_token.map_or(Value::Null, Value::String),
            );
            material.insert(
                "id_token".to_owned(),
                self.id_token.map_or(Value::Null, Value::String),
            );
        }
        Ok(RotateCredential {
            settings: self
                .settings
                .map(|settings| {
                    UpdateAccountRequest::into_command(settings).map(|(settings, _)| settings)
                })
                .transpose()?,
            mutation: CredentialMutation {
                context,
                account_id: ProviderAccountId::new(self.account_id)
                    .map_err(|_| WireValidationError::new("accountId"))?,
            },
            provider_material: ProviderDocument::new(OpaqueProviderData::new(material)),
        })
    }
}

// fork：手工轮换 OAuth token 仍走 /accounts/rotate，上游移除该入口时一并删掉了这些校验。
const MAX_ACCESS_TOKEN_BYTES: usize = 16 * 1024;
const MAX_REFRESH_TOKEN_BYTES: usize = 64 * 1024;
const MAX_ID_TOKEN_BYTES: usize = 16 * 1024;

fn validate_oauth_material(
    access_token: &str,
    refresh_token: Option<&str>,
    id_token: Option<&str>,
) -> Result<(), WireValidationError> {
    if access_token.len() > MAX_ACCESS_TOKEN_BYTES
        || !valid_visible_ascii(access_token)
        || !valid_compact_jwt_shape(access_token)
    {
        return Err(WireValidationError::new("accessToken"));
    }
    if refresh_token.is_some_and(|token| {
        token.len() > MAX_REFRESH_TOKEN_BYTES
            || !valid_visible_ascii(token)
            || token == access_token
    }) {
        return Err(WireValidationError::new("refreshToken"));
    }
    if id_token.is_some_and(|token| {
        token.len() > MAX_ID_TOKEN_BYTES
            || !valid_visible_ascii(token)
            || !valid_compact_jwt_shape(token)
    }) {
        return Err(WireValidationError::new("idToken"));
    }
    Ok(())
}

fn valid_compact_jwt_shape(value: &str) -> bool {
    let mut segments = value.split('.');
    matches!(
        (segments.next(), segments.next(), segments.next(), segments.next()),
        (Some(header), Some(payload), Some(signature), None)
            if !header.is_empty() && !payload.is_empty() && !signature.is_empty()
    )
}

fn valid_visible_ascii(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
}
