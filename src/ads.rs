use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use reqwest::{Client, Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::GoogleAuth,
    error::{Error, Result},
};

const DEFAULT_API_VERSION: &str = "v25";
const MAX_MICROS: u64 = i64::MAX as u64;
const SENSITIVE_GAQL_FRAGMENTS: &[&str] = &[
    "customer_user_access",
    "customer_user_access_invitation",
    "change_event.user_email",
    "click_view",
    "local_services_",
    "hotel_reconciliation",
    "search_term",
];

#[derive(Clone)]
pub struct AdsConfig {
    api_version: String,
    developer_token: Option<String>,
    default_customer_id: Option<String>,
    login_customer_id: Option<String>,
    allowed_mutation_customers: HashSet<String>,
    mutations_enabled: bool,
    max_bid_increase_percent: u32,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupStatus {
    pub api_version: String,
    pub service_account_email: String,
    pub developer_token_configured: bool,
    pub default_customer_id_configured: bool,
    pub login_customer_id_configured: bool,
    pub mutations_enabled: bool,
    pub allowed_mutation_customer_count: usize,
    pub max_bid_increase_percent: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct BudgetAllocation {
    pub budget_id: String,
    pub amount_micros: u64,
}

impl AdsConfig {
    pub fn from_env() -> Result<Self> {
        let api_version = std::env::var("GOOGLE_ADS_API_VERSION")
            .unwrap_or_else(|_| DEFAULT_API_VERSION.to_string());
        if !api_version.starts_with('v')
            || api_version.len() < 2
            || !api_version[1..].chars().all(|c| c.is_ascii_digit())
        {
            return Err(Error::Config(
                "GOOGLE_ADS_API_VERSION must look like v25".into(),
            ));
        }

        let developer_token = nonempty_env("GOOGLE_ADS_DEVELOPER_TOKEN");
        let default_customer_id = optional_customer_env("GOOGLE_ADS_CUSTOMER_ID")?;
        let login_customer_id = optional_customer_env("GOOGLE_ADS_LOGIN_CUSTOMER_ID")?;
        let mutations_enabled = nonempty_env("GOOGLE_ADS_MUTATIONS_ENABLED")
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes"));
        let max_bid_increase_percent = nonempty_env("GOOGLE_ADS_MAX_BID_INCREASE_PERCENT")
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    Error::Config("GOOGLE_ADS_MAX_BID_INCREASE_PERCENT must be an integer".into())
                })
            })
            .transpose()?
            .unwrap_or(25);
        if max_bid_increase_percent > 100 {
            return Err(Error::Config(
                "GOOGLE_ADS_MAX_BID_INCREASE_PERCENT must be between 0 and 100".into(),
            ));
        }

        let allowed_mutation_customers =
            parse_customer_allowlist(nonempty_env("GOOGLE_ADS_ALLOWED_CUSTOMER_IDS").as_deref())?;
        Ok(Self {
            api_version,
            developer_token,
            default_customer_id,
            login_customer_id,
            allowed_mutation_customers,
            mutations_enabled,
            max_bid_increase_percent,
        })
    }
}

fn nonempty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn optional_customer_env(name: &str) -> Result<Option<String>> {
    nonempty_env(name)
        .map(|v| normalize_customer_id(&v))
        .transpose()
}

fn parse_customer_allowlist(value: Option<&str>) -> Result<HashSet<String>> {
    let mut customers = HashSet::new();
    if let Some(value) = value {
        for customer_id in value.split(',').map(str::trim).filter(|v| !v.is_empty()) {
            customers.insert(normalize_customer_id(customer_id)?);
        }
    }
    Ok(customers)
}

pub fn normalize_customer_id(value: &str) -> Result<String> {
    let normalized = value.replace('-', "");
    if normalized.len() != 10 || !normalized.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::Invalid(format!(
            "Google Ads customer ID must contain exactly 10 digits: {value}"
        )));
    }
    Ok(normalized)
}

fn numeric_id(value: &str, label: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::Invalid(format!("{label} must contain digits only")));
    }
    Ok(value.to_string())
}

fn validate_date(value: &str) -> Result<()> {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(i, b)| i != 4 && i != 7 && !b.is_ascii_digit())
    {
        return Err(Error::Invalid(format!("date must be YYYY-MM-DD: {value}")));
    }
    Ok(())
}

fn validate_date_range(start_date: &str, end_date: &str) -> Result<()> {
    validate_date(start_date)?;
    validate_date(end_date)?;
    if start_date > end_date {
        return Err(Error::Invalid(
            "start_date must be earlier than or equal to end_date".into(),
        ));
    }
    Ok(())
}

fn validate_limit(limit: u32) -> Result<u32> {
    if !(1..=1000).contains(&limit) {
        return Err(Error::Invalid("limit must be between 1 and 1000".into()));
    }
    Ok(limit)
}

fn validate_micros(value: u64, label: &str) -> Result<u64> {
    if value == 0 || value > MAX_MICROS {
        return Err(Error::Invalid(format!(
            "{label} must be between 1 and {MAX_MICROS}"
        )));
    }
    Ok(value)
}

fn validate_gaql_privacy(query: &str) -> Result<()> {
    let normalized = query.to_ascii_lowercase();
    if let Some(field) = SENSITIVE_GAQL_FRAGMENTS
        .iter()
        .find(|field| normalized.contains(**field))
    {
        return Err(Error::Invalid(format!(
            "GAQL access to {field} is blocked because it can expose user identifiers"
        )));
    }
    Ok(())
}

fn contains_email_like(value: &str) -> bool {
    fn local_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'%' | b'+' | b'-')
    }
    fn domain_byte(byte: u8) -> bool {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-')
    }

    let bytes = value.as_bytes();
    bytes
        .iter()
        .enumerate()
        .filter(|(_, byte)| **byte == b'@')
        .any(|(at, _)| {
            let mut start = at;
            while start > 0 && local_byte(bytes[start - 1]) {
                start -= 1;
            }
            let mut end = at + 1;
            while end < bytes.len() && domain_byte(bytes[end]) {
                end += 1;
            }
            let local = &bytes[start..at];
            let mut domain = &bytes[at + 1..end];
            while domain
                .last()
                .is_some_and(|byte| matches!(*byte, b'.' | b'-'))
            {
                domain = &domain[..domain.len() - 1];
            }
            let Some(dot) = domain.iter().rposition(|byte| *byte == b'.') else {
                return false;
            };
            let suffix = &domain[dot + 1..];
            !local.is_empty()
                && !domain.is_empty()
                && suffix.len() >= 2
                && suffix.iter().all(|byte| byte.is_ascii_alphabetic())
        })
}

fn contains_phone_or_long_id(value: &str) -> bool {
    let mut digits = 0_u8;
    for character in value.chars().chain(std::iter::once('\0')) {
        if character.is_ascii_digit() {
            digits = digits.saturating_add(1);
        } else if matches!(character, '+' | '-' | '(' | ')' | '.' | ' ') {
            continue;
        } else {
            if digits >= 7 {
                return true;
            }
            digits = 0;
        }
    }
    digits >= 7
}

fn redact_search_term_pii(mut response: Value) -> Value {
    let Some(rows) = response.get_mut("results").and_then(Value::as_array_mut) else {
        return response;
    };
    for row in rows {
        let Some(search_term) = row
            .get_mut("searchTermView")
            .and_then(|view| view.get_mut("searchTerm"))
        else {
            continue;
        };
        let should_redact = search_term
            .as_str()
            .is_some_and(|term| contains_email_like(term) || contains_phone_or_long_id(term));
        if should_redact {
            *search_term = Value::String("[REDACTED_POTENTIAL_PII]".into());
        }
    }
    response
}

fn segment_field(breakdown: &str) -> Result<&'static str> {
    match breakdown.to_ascii_lowercase().as_str() {
        "device" => Ok("segments.device"),
        "day_of_week" => Ok("segments.day_of_week"),
        "hour" => Ok("segments.hour"),
        "network" => Ok("segments.ad_network_type"),
        _ => Err(Error::Invalid(
            "breakdown must be device, day_of_week, hour, or network".into(),
        )),
    }
}

fn validate_bid_increase(current: u64, requested: u64, max_percent: u32) -> Result<()> {
    if requested <= current {
        return Ok(());
    }
    if current == 0 {
        return Err(Error::Invalid(
            "cannot increase a keyword bid without a positive current bid baseline".into(),
        ));
    }
    let increase = requested - current;
    let max_increase = current
        .checked_mul(u64::from(max_percent))
        .ok_or_else(|| Error::Invalid("keyword bid guard overflowed".into()))?
        / 100;
    if increase > max_increase {
        return Err(Error::Invalid(format!(
            "keyword bid guard rejected increase from {current} to {requested} micros; maximum increase is {max_percent}%"
        )));
    }
    Ok(())
}

pub struct GoogleAdsClient {
    auth: Arc<GoogleAuth>,
    http: Client,
    config: AdsConfig,
}

impl GoogleAdsClient {
    pub fn new(auth: Arc<GoogleAuth>, config: AdsConfig) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("google-ads-mcp/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { auth, http, config })
    }

    pub fn setup_status(&self) -> SetupStatus {
        SetupStatus {
            api_version: self.config.api_version.clone(),
            service_account_email: self.auth.service_account_email().to_string(),
            developer_token_configured: self.config.developer_token.is_some(),
            default_customer_id_configured: self.config.default_customer_id.is_some(),
            login_customer_id_configured: self.config.login_customer_id.is_some(),
            mutations_enabled: self.config.mutations_enabled,
            allowed_mutation_customer_count: self.config.allowed_mutation_customers.len(),
            max_bid_increase_percent: self.config.max_bid_increase_percent,
        }
    }

    fn customer_id(&self, explicit: Option<&str>) -> Result<String> {
        if let Some(value) = explicit {
            return normalize_customer_id(value);
        }
        self.config.default_customer_id.clone().ok_or_else(|| {
            Error::Config(
                "customer_id was omitted and GOOGLE_ADS_CUSTOMER_ID is not configured".into(),
            )
        })
    }

    async fn authorized(&self, method: Method, url: String) -> Result<RequestBuilder> {
        let developer_token =
            self.config.developer_token.as_ref().ok_or_else(|| {
                Error::Config("GOOGLE_ADS_DEVELOPER_TOKEN is not configured".into())
            })?;
        let access_token = self.auth.access_token().await?;
        let mut request = self
            .http
            .request(method, url)
            .bearer_auth(access_token)
            .header("developer-token", developer_token);
        if let Some(login_customer_id) = self.config.login_customer_id.as_ref() {
            request = request.header("login-customer-id", login_customer_id);
        }
        Ok(request)
    }

    async fn response_json(&self, response: reqwest::Response) -> Result<Value> {
        let status = response.status();
        let request_id = response
            .headers()
            .get("request-id")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("unavailable")
            .to_string();
        if !status.is_success() {
            // Google error details can echo caller-provided GAQL literals or
            // mutation data. Keep only the opaque support ID in MCP errors.
            return Err(Error::Api { status, request_id });
        }
        let body = response.text().await?;
        Ok(serde_json::from_str(&body)?)
    }

    pub async fn list_accessible_customers(&self) -> Result<Value> {
        let url = format!(
            "https://googleads.googleapis.com/{}/customers:listAccessibleCustomers",
            self.config.api_version
        );
        let request = self.authorized(Method::GET, url).await?;
        self.response_json(request.send().await?).await
    }

    async fn search(
        &self,
        customer_id: Option<&str>,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<Value> {
        let customer_id = self.customer_id(customer_id)?;
        let query = query.trim();
        if query.len() > 20_000 || !query.to_ascii_lowercase().starts_with("select ") {
            return Err(Error::Invalid(
                "GAQL must be a SELECT query no longer than 20,000 characters".into(),
            ));
        }
        if query.contains(';') {
            return Err(Error::Invalid(
                "GAQL must contain one query and no semicolon".into(),
            ));
        }
        let url = format!(
            "https://googleads.googleapis.com/{}/customers/{customer_id}/googleAds:search",
            self.config.api_version
        );
        let mut body = json!({ "query": query });
        if let Some(page_token) = page_token.filter(|v| !v.is_empty()) {
            body["pageToken"] = json!(page_token);
        }
        let request = self.authorized(Method::POST, url).await?.json(&body);
        self.response_json(request.send().await?).await
    }

    pub async fn run_gaql(
        &self,
        customer_id: Option<&str>,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<Value> {
        validate_gaql_privacy(query)?;
        self.search(customer_id, query, page_token).await
    }

    pub async fn account_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let query = format!(
            "SELECT customer.id, customer.descriptive_name, customer.currency_code, \
             customer.time_zone, metrics.impressions, metrics.clicks, metrics.cost_micros, \
             metrics.conversions, metrics.conversions_value, metrics.all_conversions \
             FROM customer WHERE segments.date BETWEEN '{start_date}' AND '{end_date}'"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn campaign_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT campaign.id, campaign.name, campaign.status, campaign.advertising_channel_type, \
             campaign.campaign_budget, metrics.impressions, metrics.clicks, metrics.ctr, \
             metrics.average_cpc, metrics.cost_micros, metrics.conversions, \
             metrics.conversions_value, metrics.cost_per_conversion \
             FROM campaign WHERE segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.cost_micros DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn ad_group_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT campaign.id, campaign.name, ad_group.id, ad_group.name, ad_group.status, \
             ad_group.type, ad_group.cpc_bid_micros, metrics.impressions, metrics.clicks, \
             metrics.ctr, metrics.average_cpc, metrics.cost_micros, metrics.conversions, \
             metrics.conversions_value, metrics.cost_per_conversion \
             FROM ad_group WHERE segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.cost_micros DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn ad_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT campaign.id, campaign.name, ad_group.id, ad_group.name, \
             ad_group_ad.ad.id, ad_group_ad.ad.type, ad_group_ad.status, \
             ad_group_ad.ad.final_urls, ad_group_ad.ad.responsive_search_ad.headlines, \
             ad_group_ad.ad.responsive_search_ad.descriptions, metrics.impressions, \
             metrics.clicks, metrics.ctr, metrics.average_cpc, metrics.cost_micros, \
             metrics.conversions, metrics.conversions_value, metrics.cost_per_conversion \
             FROM ad_group_ad WHERE ad_group_ad.status != 'REMOVED' \
             AND segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.cost_micros DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn keyword_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT campaign.id, campaign.name, ad_group.id, ad_group.name, \
             ad_group_criterion.criterion_id, ad_group_criterion.status, \
             ad_group_criterion.keyword.text, ad_group_criterion.keyword.match_type, \
             ad_group_criterion.quality_info.quality_score, \
             ad_group_criterion.effective_cpc_bid_micros, metrics.impressions, \
             metrics.clicks, metrics.ctr, metrics.average_cpc, metrics.cost_micros, \
             metrics.conversions, metrics.conversions_value, metrics.cost_per_conversion \
             FROM keyword_view WHERE ad_group_criterion.status != 'REMOVED' \
             AND segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.cost_micros DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn traffic_segment_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        breakdown: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let segment = segment_field(breakdown)?;
        let query = format!(
            "SELECT {segment}, metrics.impressions, metrics.clicks, metrics.ctr, \
             metrics.average_cpc, metrics.cost_micros, metrics.conversions, \
             metrics.conversions_value, metrics.cost_per_conversion \
             FROM customer WHERE segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.cost_micros DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn geographic_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT campaign.id, campaign.name, geographic_view.country_criterion_id, \
             geographic_view.location_type, metrics.impressions, metrics.clicks, metrics.ctr, \
             metrics.average_cpc, metrics.cost_micros, metrics.conversions, \
             metrics.conversions_value, metrics.cost_per_conversion \
             FROM geographic_view WHERE segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.cost_micros DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn landing_page_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT landing_page_view.unexpanded_final_url, metrics.impressions, \
             metrics.clicks, metrics.ctr, metrics.average_cpc, metrics.cost_micros, \
             metrics.conversions, metrics.conversions_value, metrics.cost_per_conversion \
             FROM landing_page_view WHERE segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.cost_micros DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn change_history(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT change_event.change_date_time, change_event.change_resource_type, \
             change_event.change_resource_name, change_event.client_type, \
             change_event.changed_fields FROM change_event \
             WHERE change_event.change_date_time BETWEEN '{start_date} 00:00:00' \
             AND '{end_date} 23:59:59' ORDER BY change_event.change_date_time DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn recommendations(&self, customer_id: Option<&str>) -> Result<Value> {
        let query = "SELECT recommendation.type, recommendation.impact, \
                     recommendation.campaign, recommendation.resource_name \
                     FROM recommendation WHERE recommendation.dismissed = FALSE LIMIT 100";
        self.search(customer_id, query, None).await
    }

    pub async fn conversion_actions(&self, customer_id: Option<&str>) -> Result<Value> {
        let query = "SELECT conversion_action.id, conversion_action.name, conversion_action.status, \
                     conversion_action.type, conversion_action.category, \
                     conversion_action.primary_for_goal, conversion_action.include_in_conversions_metric \
                     FROM conversion_action ORDER BY conversion_action.name";
        self.search(customer_id, query, None).await
    }

    pub async fn conversion_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT segments.conversion_action_name, segments.conversion_action_category, \
             metrics.conversions, metrics.conversions_value, metrics.all_conversions, \
             metrics.cost_per_conversion FROM customer \
             WHERE segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.conversions DESC LIMIT {limit}"
        );
        self.search(customer_id, &query, None).await
    }

    pub async fn search_terms(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let limit = validate_limit(limit)?;
        let query = format!(
            "SELECT search_term_view.search_term, search_term_view.status, campaign.id, \
             campaign.name, ad_group.id, ad_group.name, metrics.impressions, metrics.clicks, \
             metrics.cost_micros, metrics.conversions, metrics.conversions_value \
             FROM search_term_view WHERE segments.date BETWEEN '{start_date}' AND '{end_date}' \
             ORDER BY metrics.cost_micros DESC LIMIT {limit}"
        );
        let response = self.search(customer_id, &query, None).await?;
        Ok(redact_search_term_pii(response))
    }

    fn mutation_customer(&self, explicit: Option<&str>) -> Result<String> {
        if !self.config.mutations_enabled {
            return Err(Error::Config(
                "mutations are disabled; set GOOGLE_ADS_MUTATIONS_ENABLED=true after account validation"
                    .into(),
            ));
        }
        let customer_id = self.customer_id(explicit)?;
        if !self
            .config
            .allowed_mutation_customers
            .contains(&customer_id)
        {
            return Err(Error::Config(format!(
                "customer {customer_id} is not in GOOGLE_ADS_ALLOWED_CUSTOMER_IDS"
            )));
        }
        Ok(customer_id)
    }

    async fn mutate(&self, customer_id: &str, service: &str, body: Value) -> Result<Value> {
        let url = format!(
            "https://googleads.googleapis.com/{}/customers/{customer_id}/{service}:mutate",
            self.config.api_version
        );
        let request = self.authorized(Method::POST, url).await?.json(&body);
        self.response_json(request.send().await?).await
    }

    pub async fn set_campaign_status(
        &self,
        customer_id: Option<&str>,
        campaign_id: &str,
        status: &str,
        confirm: bool,
    ) -> Result<Value> {
        let customer_id = self.customer_id(customer_id)?;
        let campaign_id = numeric_id(campaign_id, "campaign_id")?;
        let status = status.to_ascii_uppercase();
        if status != "ENABLED" && status != "PAUSED" {
            return Err(Error::Invalid("status must be ENABLED or PAUSED".into()));
        }
        let plan = json!({
            "operation": "set_campaign_status",
            "customerId": customer_id,
            "campaignId": campaign_id,
            "status": status,
            "budgetChangeMicros": 0,
        });
        if !confirm {
            return Ok(json!({ "mode": "preview", "confirmRequired": true, "plan": plan }));
        }
        let customer_id = self.mutation_customer(Some(&customer_id))?;
        tracing::warn!(%customer_id, %campaign_id, %status, "confirmed Google Ads mutation");
        let response = self
            .mutate(
                &customer_id,
                "campaigns",
                json!({
                    "operations": [{
                        "updateMask": "status",
                        "update": {
                            "resourceName": format!("customers/{customer_id}/campaigns/{campaign_id}"),
                            "status": status,
                        }
                    }],
                    "partialFailure": false,
                }),
            )
            .await?;
        Ok(json!({ "mode": "applied", "plan": plan, "response": response }))
    }

    pub async fn set_keyword(
        &self,
        customer_id: Option<&str>,
        ad_group_id: &str,
        criterion_id: &str,
        status: Option<&str>,
        cpc_bid_micros: Option<u64>,
        confirm: bool,
    ) -> Result<Value> {
        let customer_id = self.customer_id(customer_id)?;
        let ad_group_id = numeric_id(ad_group_id, "ad_group_id")?;
        let criterion_id = numeric_id(criterion_id, "criterion_id")?;
        if status.is_none() && cpc_bid_micros.is_none() {
            return Err(Error::Invalid(
                "provide status, cpc_bid_micros, or both".into(),
            ));
        }
        let status = status.map(str::to_ascii_uppercase);
        if let Some(status) = status.as_deref() {
            if status != "ENABLED" && status != "PAUSED" {
                return Err(Error::Invalid("status must be ENABLED or PAUSED".into()));
            }
        }
        if let Some(requested_bid) = cpc_bid_micros {
            validate_micros(requested_bid, "cpc_bid_micros")?;
        }

        // Resolve through keyword_view even for status-only updates. This prevents this
        // deliberately narrow tool from mutating another ad-group criterion type when a
        // caller supplies valid-looking IDs.
        let query = format!(
            "SELECT ad_group_criterion.criterion_id, \
             ad_group_criterion.effective_cpc_bid_micros FROM keyword_view \
             WHERE ad_group.id = {ad_group_id} \
             AND ad_group_criterion.criterion_id = {criterion_id} LIMIT 1"
        );
        let response = self.search(Some(&customer_id), &query, None).await?;
        let criterion = response
            .get("results")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("adGroupCriterion"))
            .ok_or_else(|| {
                Error::Invalid(
                    "keyword lookup returned no matching criterion; check IDs and access".into(),
                )
            })?;

        let current_cpc_bid_micros = if let Some(requested_bid) = cpc_bid_micros {
            let current_bid = criterion.get("effectiveCpcBidMicros");
            let current_bid = json_u64(current_bid, "effectiveCpcBidMicros")?;
            validate_bid_increase(
                current_bid,
                requested_bid,
                self.config.max_bid_increase_percent,
            )?;
            Some(current_bid)
        } else {
            None
        };

        let mut update = json!({
            "resourceName": format!(
                "customers/{customer_id}/adGroupCriteria/{ad_group_id}~{criterion_id}"
            )
        });
        let mut mask = Vec::new();
        if let Some(status) = status.as_ref() {
            update["status"] = json!(status);
            mask.push("status");
        }
        if let Some(cpc_bid_micros) = cpc_bid_micros {
            update["cpcBidMicros"] = json!(cpc_bid_micros.to_string());
            mask.push("cpcBidMicros");
        }
        let plan = json!({
            "operation": "set_keyword",
            "customerId": customer_id,
            "adGroupId": ad_group_id,
            "criterionId": criterion_id,
            "status": status,
            "cpcBidMicros": cpc_bid_micros,
            "currentCpcBidMicros": current_cpc_bid_micros,
            "maxBidIncreasePercent": self.config.max_bid_increase_percent,
            "budgetChangeMicros": 0,
        });
        if !confirm {
            return Ok(json!({ "mode": "preview", "confirmRequired": true, "plan": plan }));
        }
        let customer_id = self.mutation_customer(Some(&customer_id))?;
        tracing::warn!(%customer_id, %ad_group_id, %criterion_id, "confirmed Google Ads keyword mutation");
        let response = self
            .mutate(
                &customer_id,
                "adGroupCriteria",
                json!({
                    "operations": [{ "update": update, "updateMask": mask.join(",") }],
                    "partialFailure": false,
                }),
            )
            .await?;
        Ok(json!({ "mode": "applied", "plan": plan, "response": response }))
    }

    pub async fn add_negative_keyword(
        &self,
        customer_id: Option<&str>,
        ad_group_id: &str,
        text: &str,
        match_type: &str,
        confirm: bool,
    ) -> Result<Value> {
        let customer_id = self.customer_id(customer_id)?;
        let ad_group_id = numeric_id(ad_group_id, "ad_group_id")?;
        let text = text.trim();
        if text.is_empty() || text.len() > 80 || text.chars().any(char::is_control) {
            return Err(Error::Invalid(
                "negative keyword text must contain 1 to 80 non-control characters".into(),
            ));
        }
        let match_type = match_type.to_ascii_uppercase();
        if !matches!(match_type.as_str(), "EXACT" | "PHRASE" | "BROAD") {
            return Err(Error::Invalid(
                "match_type must be EXACT, PHRASE, or BROAD".into(),
            ));
        }
        let plan = json!({
            "operation": "add_negative_keyword",
            "customerId": customer_id,
            "adGroupId": ad_group_id,
            "text": text,
            "matchType": match_type,
            "budgetChangeMicros": 0,
        });
        if !confirm {
            return Ok(json!({ "mode": "preview", "confirmRequired": true, "plan": plan }));
        }
        let customer_id = self.mutation_customer(Some(&customer_id))?;
        tracing::warn!(%customer_id, %ad_group_id, "confirmed Google Ads negative keyword mutation");
        let response = self
            .mutate(
                &customer_id,
                "adGroupCriteria",
                json!({
                    "operations": [{
                        "create": {
                            "adGroup": format!("customers/{customer_id}/adGroups/{ad_group_id}"),
                            "status": "ENABLED",
                            "negative": true,
                            "keyword": { "text": text, "matchType": match_type },
                        }
                    }],
                    "partialFailure": false,
                }),
            )
            .await?;
        Ok(json!({ "mode": "applied", "plan": plan, "response": response }))
    }

    pub async fn rebalance_budgets(
        &self,
        customer_id: Option<&str>,
        allocations: &[BudgetAllocation],
        confirm: bool,
    ) -> Result<Value> {
        let customer_id = self.customer_id(customer_id)?;
        if allocations.is_empty() || allocations.len() > 50 {
            return Err(Error::Invalid(
                "allocations must contain between 1 and 50 budgets".into(),
            ));
        }

        let mut requested = HashMap::new();
        for allocation in allocations {
            let budget_id = numeric_id(&allocation.budget_id, "budget_id")?;
            validate_micros(allocation.amount_micros, "budget amount_micros")?;
            if requested
                .insert(budget_id, allocation.amount_micros)
                .is_some()
            {
                return Err(Error::Invalid("duplicate budget_id in allocations".into()));
            }
        }

        let ids = requested.keys().cloned().collect::<Vec<_>>().join(",");
        let query = format!(
            "SELECT campaign_budget.id, campaign_budget.name, campaign_budget.amount_micros, \
             campaign_budget.period FROM campaign_budget WHERE campaign_budget.id IN ({ids})"
        );
        let current_response = self.search(Some(&customer_id), &query, None).await?;
        let rows = current_response
            .get("results")
            .and_then(Value::as_array)
            .ok_or_else(|| Error::Invalid("budget lookup returned no results array".into()))?;
        let mut current = HashMap::new();
        for row in rows {
            let budget = row.get("campaignBudget").ok_or_else(|| {
                Error::Invalid("budget lookup row is missing campaignBudget".into())
            })?;
            let id = json_u64(budget.get("id"), "campaignBudget.id")?.to_string();
            let amount = json_u64(budget.get("amountMicros"), "campaignBudget.amountMicros")?;
            let period = budget
                .get("period")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Invalid("campaignBudget.period is missing".into()))?;
            if period != "DAILY" {
                return Err(Error::Invalid(format!(
                    "budget {id} has period {period}; only DAILY budgets can be rebalanced"
                )));
            }
            current.insert(id, amount);
        }
        if current.len() != requested.len() {
            return Err(Error::Invalid(format!(
                "requested {} budgets but Google Ads returned {}; check IDs and access",
                requested.len(),
                current.len()
            )));
        }

        let current_total = current.values().try_fold(0_u64, |sum, value| {
            sum.checked_add(*value)
                .ok_or_else(|| Error::Invalid("current budget total overflowed".into()))
        })?;
        let requested_total = requested.values().try_fold(0_u64, |sum, value| {
            sum.checked_add(*value)
                .ok_or_else(|| Error::Invalid("requested budget total overflowed".into()))
        })?;
        if requested_total > current_total {
            return Err(Error::Invalid(format!(
                "budget guard rejected increase: current total {current_total} micros, requested {requested_total} micros"
            )));
        }

        let plan = json!({
            "operation": "rebalance_budgets",
            "customerId": customer_id,
            "currentTotalMicros": current_total,
            "requestedTotalMicros": requested_total,
            "decreaseMicros": current_total - requested_total,
            "allocations": allocations,
        });
        if !confirm {
            return Ok(json!({
                "mode": "preview",
                "confirmRequired": true,
                "budgetInvariant": "requested total is less than or equal to current total",
                "plan": plan,
            }));
        }

        let customer_id = self.mutation_customer(Some(&customer_id))?;
        tracing::warn!(%customer_id, current_total, requested_total, "confirmed Google Ads budget rebalance");
        let operations = requested
            .into_iter()
            .map(|(budget_id, amount_micros)| {
                json!({
                    "updateMask": "amountMicros",
                    "update": {
                        "resourceName": format!("customers/{customer_id}/campaignBudgets/{budget_id}"),
                        "amountMicros": amount_micros.to_string(),
                    }
                })
            })
            .collect::<Vec<_>>();
        let response = self
            .mutate(
                &customer_id,
                "campaignBudgets",
                json!({ "operations": operations, "partialFailure": false }),
            )
            .await?;
        Ok(json!({ "mode": "applied", "plan": plan, "response": response }))
    }
}

fn json_u64(value: Option<&Value>, label: &str) -> Result<u64> {
    match value {
        Some(Value::String(value)) => value
            .parse()
            .map_err(|_| Error::Invalid(format!("{label} is not an unsigned integer"))),
        Some(Value::Number(value)) => value
            .as_u64()
            .ok_or_else(|| Error::Invalid(format!("{label} is not an unsigned integer"))),
        _ => Err(Error::Invalid(format!("{label} is missing"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn customer_ids_accept_ui_format() {
        assert_eq!(normalize_customer_id("123-456-7890").unwrap(), "1234567890");
    }

    #[test]
    fn customer_ids_reject_wrong_length() {
        assert!(normalize_customer_id("1234").is_err());
    }

    #[test]
    fn dates_require_iso_shape() {
        assert!(validate_date("2026-08-19").is_ok());
        assert!(validate_date("19/08/2026").is_err());
        assert!(validate_date_range("2026-08-18", "2026-08-19").is_ok());
        assert!(validate_date_range("2026-08-20", "2026-08-19").is_err());
    }

    #[test]
    fn report_limits_are_bounded() {
        assert!(validate_limit(1).is_ok());
        assert!(validate_limit(1000).is_ok());
        assert!(validate_limit(1001).is_err());
    }

    #[test]
    fn micros_fit_google_ads_signed_int64() {
        assert!(validate_micros(1, "bid").is_ok());
        assert!(validate_micros(MAX_MICROS, "bid").is_ok());
        assert!(validate_micros(0, "bid").is_err());
        assert!(validate_micros(MAX_MICROS + 1, "bid").is_err());
    }

    #[test]
    fn mutation_allowlist_is_explicit() {
        assert!(parse_customer_allowlist(None).unwrap().is_empty());
        let customers = parse_customer_allowlist(Some("123-456-7890, 0987654321")).unwrap();
        assert!(customers.contains("1234567890"));
        assert!(customers.contains("0987654321"));
    }

    #[test]
    fn raw_gaql_blocks_direct_user_identifiers() {
        assert!(validate_gaql_privacy("SELECT campaign.id FROM campaign").is_ok());
        assert!(validate_gaql_privacy("SELECT change_event.user_email FROM change_event").is_err());
        assert!(validate_gaql_privacy("SELECT click_view.gclid FROM click_view").is_err());
        assert!(validate_gaql_privacy(
            "SELECT local_services_lead.contact_details FROM local_services_lead"
        )
        .is_err());
        assert!(
            validate_gaql_privacy("SELECT search_term_view.search_term FROM search_term_view")
                .is_err()
        );
    }

    #[test]
    fn search_terms_redact_email_and_phone_like_values() {
        let response = json!({
            "results": [
                { "searchTermView": { "searchTerm": "meeting api" } },
                { "searchTermView": { "searchTerm": "mailto:jane@example.com?subject=demo" } },
                { "searchTermView": { "searchTerm": "call +33 6 12 34 56 78" } },
                { "searchTermView": { "searchTerm": "reach jane@example.com." } }
            ]
        });
        let redacted = redact_search_term_pii(response);
        let rows = redacted["results"].as_array().unwrap();
        assert_eq!(rows[0]["searchTermView"]["searchTerm"], "meeting api");
        assert_eq!(
            rows[1]["searchTermView"]["searchTerm"],
            "[REDACTED_POTENTIAL_PII]"
        );
        assert_eq!(
            rows[2]["searchTermView"]["searchTerm"],
            "[REDACTED_POTENTIAL_PII]"
        );
        assert_eq!(
            rows[3]["searchTermView"]["searchTerm"],
            "[REDACTED_POTENTIAL_PII]"
        );
    }

    #[test]
    fn traffic_breakdowns_are_allowlisted() {
        assert_eq!(segment_field("device").unwrap(), "segments.device");
        assert_eq!(
            segment_field("NETWORK").unwrap(),
            "segments.ad_network_type"
        );
        assert!(segment_field("campaign.name").is_err());
    }

    #[test]
    fn bid_guard_allows_decreases_and_small_increases() {
        assert!(validate_bid_increase(1_000_000, 900_000, 25).is_ok());
        assert!(validate_bid_increase(1_000_000, 1_250_000, 25).is_ok());
    }

    #[test]
    fn bid_guard_rejects_large_or_baseless_increases() {
        assert!(validate_bid_increase(1_000_000, 1_250_001, 25).is_err());
        assert!(validate_bid_increase(0, 1, 25).is_err());
    }
}
