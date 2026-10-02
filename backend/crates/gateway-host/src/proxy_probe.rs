//! 使用与 Provider 请求一致的显式代理协议，执行有超时和响应大小限制的出口测试。
//!
//! 探测目标只来自编译期常量或组合根注入，从不接受请求传入的地址。

use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use gateway_admin::{
    model::proxies::{
        ProxyExitGeo, ProxyQualityItem, ProxyQualityItemStatus, ProxyQualityProbe, ProxyTestResult,
    },
    ports::proxy::ProxyProbe,
};
use gateway_core::account::OutboundProxy;
use serde::Deserialize;

const EXIT_IP_BODY_LIMIT: usize = 1024;
const GEO_BODY_LIMIT: usize = 2048;
const GEO_TIMEOUT: Duration = Duration::from_secs(5);
const QUALITY_BODY_LIMIT: usize = 8 * 1024;
const QUALITY_TIMEOUT: Duration = Duration::from_secs(15);
// 部分上游对非浏览器 UA 直接返回挑战页；质量检测关心的是浏览器形态请求能否到达。
const QUALITY_USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
     (KHTML, like Gecko) Chrome/136.0.0.0 Safari/537.36";

/// 出口 IP 探测策略：单端点（返回 v4 或 v6），或分别对 v4/v6 专用端点并发探测得到真实双栈出口。
enum ProbeStrategy {
    Single(String),
    Dual {
        ipv4_endpoint: String,
        ipv6_endpoint: String,
    },
}

/// 无凭据请求的预期状态码即代表「目标可达」；实测校准后固定，不随请求变化。
#[derive(Debug, Clone)]
pub struct ProxyQualityTarget {
    pub name: String,
    pub url: String,
    pub reachable_statuses: Vec<u16>,
}

impl ProxyQualityTarget {
    #[must_use]
    pub fn new(name: &str, url: &str, reachable_statuses: &[u16]) -> Self {
        Self {
            name: name.to_owned(),
            url: url.to_owned(),
            reachable_statuses: reachable_statuses.to_vec(),
        }
    }
}

pub struct HttpProxyProbe {
    // 出口 IP 走策略（单栈/双栈）；地区与质量目标是 fork 定制，可选开启。
    strategy: ProbeStrategy,
    geo_endpoint: Option<String>,
    quality_targets: Vec<ProxyQualityTarget>,
    build_client: Arc<ProxyClientBuilder>,
}

type ProxyClientBuilder =
    dyn Fn(reqwest::ClientBuilder) -> Result<reqwest::Client, &'static str> + Send + Sync;

impl Default for HttpProxyProbe {
    fn default() -> Self {
        // 分别向 IPv4/IPv6 专用端点并发探测，得到真实双栈出口；再叠加地区与质量检测。
        Self::new_dual(
            "https://api.ipify.org?format=json",
            "https://api6.ipify.org?format=json",
        )
        // 免费地区服务只提供明文 HTTP；请求经代理发出，既得到出口视角，也不消耗本机限额。
        .with_geo_endpoint(
            "http://ip-api.com/json/?fields=status,country,countryCode,regionName,city,timezone&lang=zh-CN",
        )
        // 只检测网关真实会访问的上游：Codex 后端、令牌刷新、官方 API 与 xAI。
        .with_quality_targets(vec![
            ProxyQualityTarget::new(
                "chatgpt",
                "https://chatgpt.com/backend-api/codex/models",
                &[401, 404, 405],
            ),
            ProxyQualityTarget::new(
                "openai_auth",
                "https://auth.openai.com/oauth/token",
                &[302, 400, 401, 405],
            ),
            ProxyQualityTarget::new("openai_api", "https://api.openai.com/v1/models", &[401]),
            ProxyQualityTarget::new("xai", "https://api.x.ai/v1/models", &[401]),
        ])
    }
}

impl HttpProxyProbe {
    /// 只做出口 IP 检测；地区与质量目标由调用方显式开启。
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            strategy: ProbeStrategy::Single(endpoint.into()),
            geo_endpoint: None,
            quality_targets: Vec::new(),
            build_client: Arc::new(|builder| builder.build().map_err(|_| "无法创建代理连接")),
        }
    }

    #[must_use]
    pub fn new_dual(ipv4_endpoint: impl Into<String>, ipv6_endpoint: impl Into<String>) -> Self {
        Self {
            strategy: ProbeStrategy::Dual {
                ipv4_endpoint: ipv4_endpoint.into(),
                ipv6_endpoint: ipv6_endpoint.into(),
            },
            geo_endpoint: None,
            quality_targets: Vec::new(),
            build_client: Arc::new(|builder| builder.build().map_err(|_| "无法创建代理连接")),
        }
    }

    #[must_use]
    pub fn with_geo_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.geo_endpoint = Some(endpoint.into());
        self
    }

    #[must_use]
    pub fn with_quality_targets(mut self, targets: Vec<ProxyQualityTarget>) -> Self {
        self.quality_targets = targets;
        self
    }

    /// 由组合根注入与 Provider 请求一致的证书信任策略。
    #[must_use]
    pub fn with_client_builder<E>(
        mut self,
        build: impl Fn(reqwest::ClientBuilder) -> Result<reqwest::Client, E> + Send + Sync + 'static,
    ) -> Self {
        self.build_client = Arc::new(move |builder| {
            build(builder).map_err(|_| "无法创建代理连接，请检查证书信任配置")
        });
        self
    }

    fn client(
        &self,
        proxy: &OutboundProxy,
        timeout: Duration,
    ) -> Result<reqwest::Client, &'static str> {
        let proxy = reqwest::Proxy::all(proxy.expose_url()).map_err(|_| "代理地址不合法")?;
        let builder = reqwest::Client::builder()
            // 不继承环境代理，也不跟随重定向：结论必须只反映指定代理到指定目标。
            .no_proxy()
            .proxy(proxy)
            .connect_timeout(Duration::from_secs(5))
            .timeout(timeout)
            .redirect(reqwest::redirect::Policy::none());
        (self.build_client)(builder)
    }

    /// 经指定代理请求指定出口检测端点；策略层用来分别探测 v4/v6。
    async fn exit_ip_at(
        &self,
        proxy: &OutboundProxy,
        endpoint: &str,
    ) -> Result<IpAddr, &'static str> {
        let client = self.client(proxy, Duration::from_secs(12))?;
        self.exit_ip_from(&client, endpoint).await
    }

    async fn exit_ip_from(
        &self,
        client: &reqwest::Client,
        endpoint: &str,
    ) -> Result<IpAddr, &'static str> {
        let response = client.get(endpoint).send().await.map_err(|error| {
            if error.is_timeout() {
                "代理连接超时"
            } else {
                "代理连接失败，请检查地址、认证和网络"
            }
        })?;
        if !response.status().is_success() {
            return Err(
                if response.status() == reqwest::StatusCode::PROXY_AUTHENTICATION_REQUIRED {
                    "代理认证失败"
                } else {
                    "出口检测服务返回错误状态"
                },
            );
        }
        let body = read_limited(response, EXIT_IP_BODY_LIMIT, false)
            .await
            .map_err(|error| match error {
                BodyError::TooLarge => "出口检测响应过大",
                BodyError::Read => "出口检测响应读取失败",
            })?;
        #[derive(Deserialize)]
        struct Response {
            ip: IpAddr,
        }
        serde_json::from_slice::<Response>(&body)
            .map(|response| response.ip)
            .map_err(|_| "出口检测响应不合法")
    }

    /// 地区只是展示信息：任何失败都返回空，不改变连通性结论，也不计入耗时。
    async fn exit_geo(&self, client: &reqwest::Client) -> Option<ProxyExitGeo> {
        let endpoint = self.geo_endpoint.as_deref()?;
        let request = async {
            let response = client.get(endpoint).send().await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let body = read_limited(response, GEO_BODY_LIMIT, false).await.ok()?;
            parse_exit_geo(&body)
        };
        tokio::time::timeout(GEO_TIMEOUT, request)
            .await
            .ok()
            .flatten()
    }

    /// 按策略探测出口 IP：单栈返回 v4 或 v6；双栈并发探测得到真实 v4+v6。
    /// 返回 (成功, exit_ip, exit_ipv4, exit_ipv6, 结论文案)，供 base() 叠加地区/耗时。
    async fn probe_exit(
        &self,
        proxy: &OutboundProxy,
    ) -> (
        bool,
        Option<IpAddr>,
        Option<Ipv4Addr>,
        Option<Ipv6Addr>,
        String,
    ) {
        let timeout_limit = Duration::from_secs(15);
        match &self.strategy {
            ProbeStrategy::Single(endpoint) => {
                let result = tokio::time::timeout(timeout_limit, self.exit_ip_at(proxy, endpoint))
                    .await
                    .unwrap_or(Err("代理连接超时"));
                match result {
                    Ok(ip) => {
                        let (v4, v6) = match ip {
                            IpAddr::V4(v4) => (Some(v4), None),
                            IpAddr::V6(v6) => (None, Some(v6)),
                        };
                        (true, Some(ip), v4, v6, "连接成功".to_owned())
                    }
                    Err(err) => (false, None, None, None, err.to_owned()),
                }
            }
            ProbeStrategy::Dual {
                ipv4_endpoint,
                ipv6_endpoint,
            } => {
                let probe_dual = async {
                    tokio::join!(
                        self.exit_ip_at(proxy, ipv4_endpoint),
                        self.exit_ip_at(proxy, ipv6_endpoint),
                    )
                };
                match tokio::time::timeout(timeout_limit, probe_dual).await {
                    Ok((res_v4, res_v6)) => {
                        let exit_ipv4: Option<Ipv4Addr> = match res_v4 {
                            Ok(IpAddr::V4(v4)) => Some(v4),
                            _ => None,
                        };
                        let exit_ipv6: Option<Ipv6Addr> = match res_v6 {
                            Ok(IpAddr::V6(v6)) => Some(v6),
                            _ => None,
                        };
                        if exit_ipv4.is_some() && exit_ipv6.is_some() {
                            (
                                true,
                                exit_ipv4.map(IpAddr::V4),
                                exit_ipv4,
                                exit_ipv6,
                                "连接成功（双栈可用）".to_owned(),
                            )
                        } else if let Some(v4) = exit_ipv4 {
                            (
                                true,
                                Some(IpAddr::V4(v4)),
                                Some(v4),
                                None,
                                "连接成功（仅 IPv4）".to_owned(),
                            )
                        } else if let Some(v6) = exit_ipv6 {
                            (
                                true,
                                Some(IpAddr::V6(v6)),
                                None,
                                Some(v6),
                                "连接成功（仅 IPv6）".to_owned(),
                            )
                        } else {
                            let message = res_v4
                                .err()
                                .or(res_v6.err())
                                .unwrap_or("代理连接失败")
                                .to_owned();
                            (false, None, None, None, message)
                        }
                    }
                    Err(_) => (false, None, None, None, "代理连接超时".to_owned()),
                }
            }
        }
    }

    async fn base(&self, proxy: &OutboundProxy) -> (ProxyTestResult, Option<reqwest::Client>) {
        let started = Instant::now();
        let (success, exit_ip, exit_ipv4, exit_ipv6, message) = self.probe_exit(proxy).await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        // 地区/质量复用一个新客户端（与出口探测端点无关，只需相同代理配置）；失败则不查地区。
        let client = success
            .then(|| self.client(proxy, Duration::from_secs(12)).ok())
            .flatten();
        let exit_geo = match &client {
            Some(client) => self.exit_geo(client).await,
            None => None,
        };
        (
            ProxyTestResult {
                success,
                latency_ms,
                exit_ip,
                exit_geo,
                exit_ipv4,
                exit_ipv6,
                message,
            },
            client,
        )
    }

    async fn quality_target(
        &self,
        client: &reqwest::Client,
        target: &ProxyQualityTarget,
    ) -> ProxyQualityItem {
        let started = Instant::now();
        let response = client
            .get(&target.url)
            .header(reqwest::header::ACCEPT, "application/json,text/html,*/*")
            .header(reqwest::header::USER_AGENT, QUALITY_USER_AGENT)
            .send()
            .await;
        let latency_ms = Some(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
        let item = |status, http_status, message: String, cf_ray| ProxyQualityItem {
            target: target.name.clone(),
            status,
            http_status,
            latency_ms,
            message,
            cf_ray,
        };
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                let message = if error.is_timeout() {
                    "请求超时"
                } else {
                    "请求失败，代理无法到达目标"
                };
                return item(ProxyQualityItemStatus::Fail, None, message.to_owned(), None);
            }
        };
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        let cf_mitigated = header("cf-mitigated");
        let content_type = header("content-type");
        let cf_ray = header("cf-ray").filter(|value| value.len() <= 64);
        // 只有可能是挑战页的状态才读取正文，其余状态码本身已足够下结论。
        let body = if matches!(status, 403 | 429) {
            read_limited(response, QUALITY_BODY_LIMIT, true)
                .await
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        if is_cloudflare_challenge(
            status,
            cf_mitigated.as_deref(),
            content_type.as_deref(),
            &String::from_utf8_lossy(&body),
        ) {
            return item(
                ProxyQualityItemStatus::Challenge,
                Some(status),
                "命中 Cloudflare challenge".to_owned(),
                cf_ray,
            );
        }
        if target.reachable_statuses.contains(&status) || (200..300).contains(&status) {
            let message = if (200..300).contains(&status) {
                format!("HTTP {status}")
            } else {
                format!("HTTP {status}（目标可达）")
            };
            return item(ProxyQualityItemStatus::Pass, Some(status), message, None);
        }
        if status == 429 {
            return item(
                ProxyQualityItemStatus::Warn,
                Some(status),
                "目标返回 429，可能存在频控".to_owned(),
                cf_ray,
            );
        }
        item(
            ProxyQualityItemStatus::Fail,
            Some(status),
            format!("非预期状态码: {status}"),
            cf_ray,
        )
    }
}

enum BodyError {
    Read,
    TooLarge,
}

/// `truncate` 为真时读满上限即停（只用于特征识别），否则超限视为异常响应。
async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
    truncate: bool,
) -> Result<Vec<u8>, BodyError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| BodyError::Read)? {
        if body.len() + chunk.len() > limit {
            if truncate {
                body.extend_from_slice(&chunk[..limit - body.len()]);
                return Ok(body);
            }
            return Err(BodyError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// 第三方返回的文本会入库并展示：国家码必须是两位大写字母，其余字段限长并去除控制字符。
#[must_use]
pub fn parse_exit_geo(body: &[u8]) -> Option<ProxyExitGeo> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Response {
        status: String,
        country: Option<String>,
        country_code: Option<String>,
        region_name: Option<String>,
        city: Option<String>,
        timezone: Option<String>,
    }
    let response = serde_json::from_slice::<Response>(body).ok()?;
    if response.status != "success" {
        return None;
    }
    let clean = |value: Option<String>| {
        value.map(|value| value.trim().to_owned()).filter(|value| {
            !value.is_empty()
                && value.chars().count() <= 128
                && !value.chars().any(char::is_control)
        })
    };
    let country_code = response.country_code?.trim().to_ascii_uppercase();
    if country_code.len() != 2 || !country_code.bytes().all(|byte| byte.is_ascii_uppercase()) {
        return None;
    }
    Some(ProxyExitGeo {
        country: clean(response.country)?,
        country_code,
        region: clean(response.region_name),
        city: clean(response.city),
        timezone: clean(response.timezone),
    })
}

/// Cloudflare 只在 403/429 上下发挑战；优先认响应头，其次认挑战页特征。
#[must_use]
pub fn is_cloudflare_challenge(
    status: u16,
    cf_mitigated: Option<&str>,
    content_type: Option<&str>,
    body: &str,
) -> bool {
    if !matches!(status, 403 | 429) {
        return false;
    }
    if cf_mitigated.is_some_and(|value| value.to_ascii_lowercase().contains("challenge")) {
        return true;
    }
    let body = body.to_ascii_lowercase();
    if [
        "cf-chl",
        "_cf_chl_opt",
        "challenge-platform",
        "just a moment",
    ]
    .iter()
    .any(|marker| body.contains(marker))
    {
        return true;
    }
    content_type.is_some_and(|value| value.to_ascii_lowercase().contains("text/html"))
        && body.contains("cloudflare")
        && body.contains("challenge")
}

#[async_trait]
impl ProxyProbe for HttpProxyProbe {
    async fn test(&self, proxy: &OutboundProxy) -> ProxyTestResult {
        self.base(proxy).await.0
    }

    async fn quality(&self, proxy: &OutboundProxy) -> ProxyQualityProbe {
        let (base, _) = self.base(proxy).await;
        if !base.success {
            return ProxyQualityProbe {
                base,
                items: Vec::new(),
            };
        }
        // 各目标相互独立，并发执行把单次检测控制在一个目标超时之内。
        let items = match self.client(proxy, QUALITY_TIMEOUT) {
            Ok(client) => {
                futures::future::join_all(
                    self.quality_targets
                        .iter()
                        .map(|target| self.quality_target(&client, target)),
                )
                .await
            }
            Err(message) => self
                .quality_targets
                .iter()
                .map(|target| ProxyQualityItem {
                    target: target.name.clone(),
                    status: ProxyQualityItemStatus::Fail,
                    http_status: None,
                    latency_ms: None,
                    message: message.to_owned(),
                    cf_ray: None,
                })
                .collect(),
        };
        ProxyQualityProbe { base, items }
    }
}
