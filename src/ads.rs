use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::Duration,
};

use reqwest::{Client, Method, RequestBuilder};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use tokio::sync::RwLock;

use crate::{
    auth::GoogleAuth,
    error::{Error, Result},
};

const DEFAULT_API_VERSION: &str = "v25";
const MAX_MICROS: u64 = i64::MAX as u64;
const MAX_ERROR_DETAIL_CHARS: usize = 600;
const SEARCH_TERM_COLUMN: &str = "searchTermView.searchTerm";
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

#[derive(Debug, Clone, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdTextReplacement {
    /// Existing headline or description text to replace exactly.
    pub from: String,
    /// New headline (maximum 30 characters) or description (maximum 90 characters).
    pub to: String,
}

/// Optional narrowing applied to report queries. IDs are validated as digits and
/// injected as numeric literals, never as free text.
#[derive(Debug, Clone, Default)]
pub struct ReportFilter {
    pub campaign_id: Option<String>,
    pub ad_group_id: Option<String>,
    pub daily: bool,
}

/// Columnar rendering of a Google Ads search response: `columns` are the
/// camelCase field paths from Google's `fieldMask`, `rows` hold one value per
/// column. This keeps every field name once instead of once per row.
#[derive(Debug, Clone, PartialEq)]
pub struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

#[derive(Debug, Clone, Default)]
struct ReportMeta<'a> {
    start_date: Option<&'a str>,
    end_date: Option<&'a str>,
    limit: Option<u32>,
}

struct ReportQuery<'a> {
    select: &'a str,
    from: &'a str,
    conditions: Vec<String>,
    order_by: Option<&'a str>,
    limit: u32,
    filter: &'a ReportFilter,
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

/// Extract only Google Ads error codes, messages, and field paths from an error
/// body. These echo the caller's own request (GAQL field names, operation
/// fields), which the calling model needs to self-correct. The generic
/// top-level message, `trigger` values, and the raw body are never returned.
fn extract_api_error_detail(body: &str) -> Option<String> {
    let parsed: Value = serde_json::from_str(body).ok()?;
    let details = parsed.get("error")?.get("details")?.as_array()?;
    let mut parts = Vec::new();
    for detail in details {
        let Some(errors) = detail.get("errors").and_then(Value::as_array) else {
            continue;
        };
        for error in errors {
            let code = error
                .get("errorCode")
                .and_then(Value::as_object)
                .and_then(|codes| codes.iter().next())
                .map(|(kind, value)| format!("{kind}.{}", value.as_str().unwrap_or("UNKNOWN")))
                .unwrap_or_else(|| "UNKNOWN".to_string());
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map(sanitize_message)
                .unwrap_or_default();
            let path = error
                .get("location")
                .and_then(|location| location.get("fieldPathElements"))
                .and_then(Value::as_array)
                .map(|elements| {
                    elements
                        .iter()
                        .filter_map(|element| {
                            let name = element.get("fieldName")?.as_str()?;
                            Some(match element.get("index").and_then(Value::as_u64) {
                                Some(index) => format!("{name}[{index}]"),
                                None => name.to_string(),
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(".")
                })
                .filter(|path| !path.is_empty());
            let mut part = code;
            if !message.is_empty() {
                part.push_str(": ");
                part.push_str(&message);
            }
            if let Some(path) = path {
                part.push_str(" (at ");
                part.push_str(&path);
                part.push(')');
            }
            parts.push(part);
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut detail = parts.join("; ");
    if detail.chars().count() > MAX_ERROR_DETAIL_CHARS {
        detail = detail.chars().take(MAX_ERROR_DETAIL_CHARS).collect();
        detail.push('…');
    }
    Some(detail)
}

fn sanitize_message(value: &str) -> String {
    value
        .split(|c: char| c.is_control())
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn lookup_path<'a>(row: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(row, |value, key| value.get(key))
}

/// Convert Google's `{fieldMask, results: [{resource: {field: ..}}]}` shape into
/// columns and rows. Missing values become null so every row has one cell per
/// column. `resourceName` entries are not part of `fieldMask` and are dropped.
fn tabulate(response: &Value) -> Table {
    let columns = response
        .get("fieldMask")
        .and_then(Value::as_str)
        .map(|mask| {
            mask.split(',')
                .map(str::trim)
                .filter(|field| !field.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let rows = response
        .get("results")
        .and_then(Value::as_array)
        .map(|results| {
            results
                .iter()
                .map(|row| {
                    columns
                        .iter()
                        .map(|column| lookup_path(row, column).cloned().unwrap_or(Value::Null))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Table { columns, rows }
}

fn build_report_query(query: ReportQuery<'_>) -> Result<String> {
    let ReportQuery {
        select,
        from,
        mut conditions,
        order_by,
        limit,
        filter,
    } = query;
    let limit = validate_limit(limit)?;
    // Google requires a filtered attributed-resource field (campaign.id,
    // ad_group.id) to also appear in SELECT for views such as landing_page_view
    // (EXPECTED_REFERENCED_FIELD_IN_SELECT_CLAUSE), so add it when missing.
    let mut select = select.to_string();
    if let Some(campaign_id) = filter.campaign_id.as_deref() {
        let campaign_id = numeric_id(campaign_id, "campaign_id")?;
        conditions.push(format!("campaign.id = {campaign_id}"));
        if !selects_field(&select, "campaign.id") {
            select = format!("campaign.id, {select}");
        }
    }
    if let Some(ad_group_id) = filter.ad_group_id.as_deref() {
        let ad_group_id = numeric_id(ad_group_id, "ad_group_id")?;
        conditions.push(format!("ad_group.id = {ad_group_id}"));
        if !selects_field(&select, "ad_group.id") {
            select = format!("ad_group.id, {select}");
        }
    }
    if filter.daily {
        select = format!("segments.date, {select}");
    }
    let mut sql = format!("SELECT {select} FROM {from}");
    if !conditions.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&conditions.join(" AND "));
    }
    let mut order = Vec::new();
    if filter.daily {
        order.push("segments.date");
    }
    if let Some(order_by) = order_by {
        order.push(order_by);
    }
    if !order.is_empty() {
        sql.push_str(" ORDER BY ");
        sql.push_str(&order.join(", "));
    }
    sql.push_str(&format!(" LIMIT {limit}"));
    Ok(sql)
}

fn selects_field(select: &str, field: &str) -> bool {
    select
        .split(',')
        .map(str::trim)
        .any(|candidate| candidate == field)
}

fn dated<'a>(start_date: &'a str, end_date: &'a str, limit: Option<u32>) -> ReportMeta<'a> {
    ReportMeta {
        start_date: Some(start_date),
        end_date: Some(end_date),
        limit,
    }
}

fn date_condition(start_date: &str, end_date: &str) -> Result<String> {
    validate_date_range(start_date, end_date)?;
    Ok(format!(
        "segments.date BETWEEN '{start_date}' AND '{end_date}'"
    ))
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

fn redact_search_term_pii(table: &mut Table) {
    let Some(index) = table
        .columns
        .iter()
        .position(|column| column == SEARCH_TERM_COLUMN)
    else {
        return;
    };
    for row in &mut table.rows {
        let Some(cell) = row.get_mut(index) else {
            continue;
        };
        let should_redact = cell
            .as_str()
            .is_some_and(|term| contains_email_like(term) || contains_phone_or_long_id(term));
        if should_redact {
            *cell = Value::String("[REDACTED_POTENTIAL_PII]".into());
        }
    }
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
    /// Customer ID -> currency code. Currency is stable per account, so one
    /// lookup per process is enough to label every report.
    currency_cache: RwLock<HashMap<String, String>>,
}

impl GoogleAdsClient {
    pub fn new(auth: Arc<GoogleAuth>, config: AdsConfig) -> Result<Self> {
        let http = Client::builder()
            .user_agent(concat!("google-ads-mcp/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self {
            auth,
            http,
            config,
            currency_cache: RwLock::new(HashMap::new()),
        })
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
            // Keep Google's error codes and messages (they describe the caller's
            // own request, e.g. "Unrecognized fields in the query: ...") so the
            // calling model can self-correct. The raw body is never forwarded.
            let detail = response
                .text()
                .await
                .ok()
                .and_then(|body| extract_api_error_detail(&body));
            return Err(Error::Api {
                status,
                request_id,
                detail,
            });
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

    /// Best-effort currency label. Failure here must not fail a report whose
    /// main query already succeeded.
    async fn currency(&self, customer_id: &str) -> Option<String> {
        if let Some(currency) = self.currency_cache.read().await.get(customer_id) {
            return Some(currency.clone());
        }
        let response = self
            .search(
                Some(customer_id),
                "SELECT customer.currency_code FROM customer LIMIT 1",
                None,
            )
            .await
            .ok()?;
        let currency = response
            .get("results")?
            .as_array()?
            .first()?
            .get("customer")?
            .get("currencyCode")?
            .as_str()?
            .to_string();
        self.currency_cache
            .write()
            .await
            .insert(customer_id.to_string(), currency.clone());
        Some(currency)
    }

    /// Run a read query and return it as columns/rows plus Google's page token.
    async fn report_table(
        &self,
        customer_id: &str,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<(Table, Option<String>)> {
        let response = self.search(Some(customer_id), query, page_token).await?;
        let next_page_token = response
            .get("nextPageToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_string);
        Ok((tabulate(&response), next_page_token))
    }

    async fn render_report(
        &self,
        customer_id: &str,
        meta: ReportMeta<'_>,
        table: Table,
        next_page_token: Option<String>,
    ) -> Value {
        let mut out = Map::new();
        out.insert("customerId".into(), json!(customer_id));
        out.insert("currency".into(), json!(self.currency(customer_id).await));
        if let Some(start_date) = meta.start_date {
            out.insert("startDate".into(), json!(start_date));
        }
        if let Some(end_date) = meta.end_date {
            out.insert("endDate".into(), json!(end_date));
        }
        out.insert("rowCount".into(), json!(table.rows.len()));
        if let Some(limit) = meta.limit {
            out.insert("limit".into(), json!(limit));
            if table.rows.len() >= limit as usize {
                out.insert("limitReached".into(), json!(true));
            }
        }
        if let Some(token) = next_page_token {
            out.insert("nextPageToken".into(), json!(token));
        }
        out.insert("columns".into(), json!(table.columns));
        out.insert("rows".into(), json!(table.rows));
        Value::Object(out)
    }

    async fn report(
        &self,
        customer_id: Option<&str>,
        query: &str,
        meta: ReportMeta<'_>,
    ) -> Result<Value> {
        let customer_id = self.customer_id(customer_id)?;
        let (table, next_page_token) = self.report_table(&customer_id, query, None).await?;
        Ok(self
            .render_report(&customer_id, meta, table, next_page_token)
            .await)
    }

    pub async fn run_gaql(
        &self,
        customer_id: Option<&str>,
        query: &str,
        page_token: Option<&str>,
    ) -> Result<Value> {
        validate_gaql_privacy(query)?;
        let customer_id = self.customer_id(customer_id)?;
        let (table, next_page_token) = self.report_table(&customer_id, query, page_token).await?;
        Ok(self
            .render_report(&customer_id, ReportMeta::default(), table, next_page_token)
            .await)
    }

    pub async fn account_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        daily: bool,
    ) -> Result<Value> {
        let filter = ReportFilter {
            daily,
            ..ReportFilter::default()
        };
        let query = build_report_query(ReportQuery {
            select: "customer.id, customer.descriptive_name, customer.currency_code, \
                     customer.time_zone, metrics.impressions, metrics.clicks, \
                     metrics.cost_micros, metrics.conversions, metrics.conversions_value, \
                     metrics.all_conversions",
            from: "customer",
            conditions: vec![date_condition(start_date, end_date)?],
            order_by: None,
            limit: 1000,
            filter: &filter,
        })?;
        self.report(customer_id, &query, dated(start_date, end_date, None))
            .await
    }

    pub async fn campaign_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
        filter: &ReportFilter,
    ) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "campaign.id, campaign.name, campaign.status, \
                     campaign.advertising_channel_type, campaign.campaign_budget, \
                     metrics.impressions, metrics.clicks, metrics.ctr, metrics.average_cpc, \
                     metrics.cost_micros, metrics.conversions, metrics.conversions_value, \
                     metrics.cost_per_conversion",
            from: "campaign",
            conditions: vec![date_condition(start_date, end_date)?],
            order_by: Some("metrics.cost_micros DESC"),
            limit,
            filter,
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn ad_group_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
        filter: &ReportFilter,
    ) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "campaign.id, campaign.name, ad_group.id, ad_group.name, ad_group.status, \
                     ad_group.type, ad_group.cpc_bid_micros, metrics.impressions, \
                     metrics.clicks, metrics.ctr, metrics.average_cpc, metrics.cost_micros, \
                     metrics.conversions, metrics.conversions_value, \
                     metrics.cost_per_conversion",
            from: "ad_group",
            conditions: vec![date_condition(start_date, end_date)?],
            order_by: Some("metrics.cost_micros DESC"),
            limit,
            filter,
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn ad_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
        filter: &ReportFilter,
    ) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "campaign.id, campaign.name, ad_group.id, ad_group.name, \
                     ad_group_ad.ad.id, ad_group_ad.ad.type, ad_group_ad.status, \
                     ad_group_ad.ad.final_urls, ad_group_ad.ad.responsive_search_ad.headlines, \
                     ad_group_ad.ad.responsive_search_ad.descriptions, metrics.impressions, \
                     metrics.clicks, metrics.ctr, metrics.average_cpc, metrics.cost_micros, \
                     metrics.conversions, metrics.conversions_value, \
                     metrics.cost_per_conversion",
            from: "ad_group_ad",
            conditions: vec![
                "ad_group_ad.status != 'REMOVED'".to_string(),
                date_condition(start_date, end_date)?,
            ],
            order_by: Some("metrics.cost_micros DESC"),
            limit,
            filter,
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn keyword_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
        filter: &ReportFilter,
    ) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "campaign.id, campaign.name, ad_group.id, ad_group.name, \
                     ad_group_criterion.criterion_id, ad_group_criterion.status, \
                     ad_group_criterion.keyword.text, ad_group_criterion.keyword.match_type, \
                     ad_group_criterion.quality_info.quality_score, \
                     ad_group_criterion.effective_cpc_bid_micros, metrics.impressions, \
                     metrics.clicks, metrics.ctr, metrics.average_cpc, metrics.cost_micros, \
                     metrics.conversions, metrics.conversions_value, \
                     metrics.cost_per_conversion",
            from: "keyword_view",
            conditions: vec![
                "ad_group_criterion.status != 'REMOVED'".to_string(),
                date_condition(start_date, end_date)?,
            ],
            order_by: Some("metrics.cost_micros DESC"),
            limit,
            filter,
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn traffic_segment_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        breakdown: &str,
        limit: u32,
    ) -> Result<Value> {
        let segment = segment_field(breakdown)?;
        let select = format!(
            "{segment}, metrics.impressions, metrics.clicks, metrics.ctr, \
             metrics.average_cpc, metrics.cost_micros, metrics.conversions, \
             metrics.conversions_value, metrics.cost_per_conversion"
        );
        let query = build_report_query(ReportQuery {
            select: &select,
            from: "customer",
            conditions: vec![date_condition(start_date, end_date)?],
            order_by: Some("metrics.cost_micros DESC"),
            limit,
            filter: &ReportFilter::default(),
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn geographic_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
        filter: &ReportFilter,
    ) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "campaign.id, campaign.name, geographic_view.country_criterion_id, \
                     geographic_view.location_type, metrics.impressions, metrics.clicks, \
                     metrics.ctr, metrics.average_cpc, metrics.cost_micros, \
                     metrics.conversions, metrics.conversions_value, \
                     metrics.cost_per_conversion",
            from: "geographic_view",
            conditions: vec![date_condition(start_date, end_date)?],
            order_by: Some("metrics.cost_micros DESC"),
            limit,
            filter,
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn landing_page_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
        filter: &ReportFilter,
    ) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "landing_page_view.unexpanded_final_url, metrics.impressions, \
                     metrics.clicks, metrics.ctr, metrics.average_cpc, metrics.cost_micros, \
                     metrics.conversions, metrics.conversions_value, \
                     metrics.cost_per_conversion",
            from: "landing_page_view",
            conditions: vec![date_condition(start_date, end_date)?],
            order_by: Some("metrics.cost_micros DESC"),
            limit,
            filter,
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn change_history(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
    ) -> Result<Value> {
        validate_date_range(start_date, end_date)?;
        let query = build_report_query(ReportQuery {
            select: "change_event.change_date_time, change_event.change_resource_type, \
                     change_event.change_resource_name, change_event.client_type, \
                     change_event.changed_fields",
            from: "change_event",
            conditions: vec![format!(
                "change_event.change_date_time BETWEEN '{start_date} 00:00:00' \
                 AND '{end_date} 23:59:59'"
            )],
            order_by: Some("change_event.change_date_time DESC"),
            limit,
            filter: &ReportFilter::default(),
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn recommendations(&self, customer_id: Option<&str>) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "recommendation.type, recommendation.impact, recommendation.campaign, \
                     recommendation.resource_name",
            from: "recommendation",
            conditions: vec!["recommendation.dismissed = FALSE".to_string()],
            order_by: None,
            limit: 100,
            filter: &ReportFilter::default(),
        })?;
        self.report(
            customer_id,
            &query,
            ReportMeta {
                limit: Some(100),
                ..ReportMeta::default()
            },
        )
        .await
    }

    pub async fn conversion_actions(&self, customer_id: Option<&str>) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "conversion_action.id, conversion_action.name, conversion_action.status, \
                     conversion_action.type, conversion_action.category, \
                     conversion_action.primary_for_goal, \
                     conversion_action.include_in_conversions_metric",
            from: "conversion_action",
            conditions: Vec::new(),
            order_by: Some("conversion_action.name"),
            limit: 1000,
            filter: &ReportFilter::default(),
        })?;
        self.report(customer_id, &query, ReportMeta::default())
            .await
    }

    pub async fn conversion_performance(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
        daily: bool,
    ) -> Result<Value> {
        // Conversion segments only permit conversion metrics. Cost-derived
        // metrics such as cost_per_conversion make Google reject the query with
        // PROHIBITED_SEGMENT_WITH_METRIC_IN_SELECT_OR_WHERE_CLAUSE.
        let filter = ReportFilter {
            daily,
            ..ReportFilter::default()
        };
        let query = build_report_query(ReportQuery {
            select: "segments.conversion_action_name, segments.conversion_action_category, \
                     metrics.conversions, metrics.conversions_value, metrics.all_conversions, \
                     metrics.all_conversions_value",
            from: "customer",
            conditions: vec![date_condition(start_date, end_date)?],
            order_by: Some("metrics.conversions DESC"),
            limit,
            filter: &filter,
        })?;
        self.report(
            customer_id,
            &query,
            dated(start_date, end_date, Some(limit)),
        )
        .await
    }

    pub async fn search_terms(
        &self,
        customer_id: Option<&str>,
        start_date: &str,
        end_date: &str,
        limit: u32,
        filter: &ReportFilter,
    ) -> Result<Value> {
        let query = build_report_query(ReportQuery {
            select: "search_term_view.search_term, search_term_view.status, campaign.id, \
                     campaign.name, ad_group.id, ad_group.name, metrics.impressions, \
                     metrics.clicks, metrics.cost_micros, metrics.conversions, \
                     metrics.conversions_value",
            from: "search_term_view",
            conditions: vec![date_condition(start_date, end_date)?],
            order_by: Some("metrics.cost_micros DESC"),
            limit,
            filter,
        })?;
        let customer_id = self.customer_id(customer_id)?;
        let (mut table, next_page_token) = self.report_table(&customer_id, &query, None).await?;
        redact_search_term_pii(&mut table);
        Ok(self
            .render_report(
                &customer_id,
                dated(start_date, end_date, Some(limit)),
                table,
                next_page_token,
            )
            .await)
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

    pub async fn replace_responsive_search_ad_text(
        &self,
        customer_id: Option<&str>,
        ad_group_id: &str,
        ad_id: &str,
        replacements: &[AdTextReplacement],
        confirm: bool,
    ) -> Result<Value> {
        let customer_id = self.customer_id(customer_id)?;
        let ad_group_id = numeric_id(ad_group_id, "ad_group_id")?;
        let ad_id = numeric_id(ad_id, "ad_id")?;
        if replacements.is_empty() || replacements.len() > 10 {
            return Err(Error::Invalid(
                "replacements must contain between 1 and 10 entries".into(),
            ));
        }

        let mut requested = HashMap::new();
        for replacement in replacements {
            if replacement.from.is_empty()
                || replacement.to.is_empty()
                || replacement.from.chars().any(char::is_control)
                || replacement.to.chars().any(char::is_control)
            {
                return Err(Error::Invalid(
                    "replacement text must be non-empty and contain no control characters".into(),
                ));
            }
            if requested
                .insert(replacement.from.as_str(), replacement.to.as_str())
                .is_some()
            {
                return Err(Error::Invalid("replacement sources must be unique".into()));
            }
        }

        let query = format!(
            "SELECT ad_group_ad.ad.id, ad_group_ad.status, ad_group_ad.ad.type, \
             ad_group_ad.ad.final_urls, ad_group_ad.ad.tracking_url_template, \
             ad_group_ad.ad.final_url_suffix, ad_group_ad.ad.url_custom_parameters, \
             ad_group_ad.ad.responsive_search_ad.headlines, \
             ad_group_ad.ad.responsive_search_ad.descriptions, \
             ad_group_ad.ad.responsive_search_ad.path1, \
             ad_group_ad.ad.responsive_search_ad.path2 \
             FROM ad_group_ad WHERE ad_group.id = {ad_group_id} \
             AND ad_group_ad.ad.id = {ad_id} LIMIT 1"
        );
        let response = self.search(Some(&customer_id), &query, None).await?;
        let ad_group_ad = response
            .get("results")
            .and_then(Value::as_array)
            .and_then(|rows| rows.first())
            .and_then(|row| row.get("adGroupAd"))
            .ok_or_else(|| Error::Invalid("RSA lookup returned no matching ad".into()))?;
        if ad_group_ad.get("status").and_then(Value::as_str) != Some("ENABLED") {
            return Err(Error::Invalid("only an ENABLED RSA can be replaced".into()));
        }
        let ad = ad_group_ad
            .get("ad")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::Invalid("RSA lookup result is missing ad".into()))?;
        if ad.get("type").and_then(Value::as_str) != Some("RESPONSIVE_SEARCH_AD") {
            return Err(Error::Invalid(
                "the selected ad is not a responsive search ad".into(),
            ));
        }
        let rsa = ad
            .get("responsiveSearchAd")
            .and_then(Value::as_object)
            .ok_or_else(|| Error::Invalid("RSA payload is missing".into()))?;

        let mut matched = HashSet::new();
        let headlines = replaced_text_assets(
            rsa.get("headlines"),
            &requested,
            30,
            "headline",
            &mut matched,
        )?;
        let descriptions = replaced_text_assets(
            rsa.get("descriptions"),
            &requested,
            90,
            "description",
            &mut matched,
        )?;
        if matched.len() != requested.len() {
            let missing = requested
                .keys()
                .filter(|text| !matched.contains(**text))
                .copied()
                .collect::<Vec<_>>();
            return Err(Error::Invalid(format!(
                "replacement source text was not found exactly: {}",
                missing.join(", ")
            )));
        }

        let final_urls = ad
            .get("finalUrls")
            .and_then(Value::as_array)
            .filter(|urls| !urls.is_empty())
            .cloned()
            .ok_or_else(|| Error::Invalid("RSA has no final URLs".into()))?;
        let mut new_ad = json!({
            "finalUrls": final_urls,
            "responsiveSearchAd": {
                "headlines": headlines,
                "descriptions": descriptions,
            }
        });
        copy_optional_string(ad, &mut new_ad, "trackingUrlTemplate");
        copy_optional_string(ad, &mut new_ad, "finalUrlSuffix");
        if let Some(parameters) = ad.get("urlCustomParameters") {
            new_ad["urlCustomParameters"] = parameters.clone();
        }
        if let Some(path1) = rsa
            .get("path1")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            new_ad["responsiveSearchAd"]["path1"] = json!(path1);
        }
        if let Some(path2) = rsa
            .get("path2")
            .and_then(Value::as_str)
            .filter(|v| !v.is_empty())
        {
            new_ad["responsiveSearchAd"]["path2"] = json!(path2);
        }

        let plan = json!({
            "operation": "replace_responsive_search_ad_text",
            "customerId": customer_id,
            "adGroupId": ad_group_id,
            "adId": ad_id,
            "replacements": replacements,
            "budgetChangeMicros": 0,
            "effect": "create corrected RSA and pause the selected RSA atomically",
        });
        if !confirm {
            return Ok(json!({ "mode": "preview", "confirmRequired": true, "plan": plan }));
        }

        let customer_id = self.mutation_customer(Some(&customer_id))?;
        tracing::warn!(%customer_id, %ad_group_id, %ad_id, "confirmed Google Ads RSA replacement");
        let response = self
            .mutate(
                &customer_id,
                "adGroupAds",
                json!({
                    "operations": [
                        {
                            "create": {
                                "adGroup": format!("customers/{customer_id}/adGroups/{ad_group_id}"),
                                "status": "ENABLED",
                                "ad": new_ad,
                            }
                        },
                        {
                            "updateMask": "status",
                            "update": {
                                "resourceName": format!(
                                    "customers/{customer_id}/adGroupAds/{ad_group_id}~{ad_id}"
                                ),
                                "status": "PAUSED",
                            }
                        }
                    ],
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

fn replaced_text_assets(
    value: Option<&Value>,
    replacements: &HashMap<&str, &str>,
    max_chars: usize,
    label: &str,
    matched: &mut HashSet<String>,
) -> Result<Vec<Value>> {
    let assets = value
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Invalid(format!("RSA {label}s are missing")))?;
    assets
        .iter()
        .map(|asset| {
            let text = asset
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| Error::Invalid(format!("RSA {label} text is missing")))?;
            let replacement = replacements.get(text).copied();
            let text = replacement.unwrap_or(text);
            if text.chars().count() > max_chars {
                return Err(Error::Invalid(format!(
                    "RSA {label} exceeds {max_chars} characters: {text}"
                )));
            }
            if replacement.is_some() {
                matched.insert(asset["text"].as_str().unwrap_or_default().to_string());
            }
            let mut result = json!({ "text": text });
            if let Some(pinned_field) = asset.get("pinnedField").and_then(Value::as_str) {
                result["pinnedField"] = json!(pinned_field);
            }
            Ok(result)
        })
        .collect()
}

fn copy_optional_string(source: &serde_json::Map<String, Value>, target: &mut Value, key: &str) {
    if let Some(value) = source
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty())
    {
        target[key] = json!(value);
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
            "fieldMask": "searchTermView.searchTerm,metrics.clicks",
            "results": [
                { "searchTermView": { "searchTerm": "meeting api" }, "metrics": { "clicks": "3" } },
                { "searchTermView": { "searchTerm": "mailto:jane@example.com?subject=demo" } },
                { "searchTermView": { "searchTerm": "call +33 6 12 34 56 78" } },
                { "searchTermView": { "searchTerm": "reach jane@example.com." } }
            ]
        });
        let mut table = tabulate(&response);
        redact_search_term_pii(&mut table);
        assert_eq!(
            table.columns,
            vec!["searchTermView.searchTerm", "metrics.clicks"]
        );
        assert_eq!(table.rows[0], vec![json!("meeting api"), json!("3")]);
        for row in &table.rows[1..] {
            assert_eq!(row[0], "[REDACTED_POTENTIAL_PII]");
            assert_eq!(row[1], Value::Null);
        }
    }

    #[test]
    fn tabulate_uses_field_mask_columns_and_drops_resource_names() {
        let response = json!({
            "fieldMask": "campaign.id,adGroupCriterion.keyword.text,metrics.costMicros",
            "queryResourceConsumption": "17",
            "results": [{
                "campaign": { "id": "1", "resourceName": "customers/9/campaigns/1" },
                "adGroupCriterion": { "keyword": { "text": "meeting bot" } },
                "metrics": { "costMicros": "240000" }
            }]
        });
        let table = tabulate(&response);
        assert_eq!(
            table,
            Table {
                columns: vec![
                    "campaign.id".into(),
                    "adGroupCriterion.keyword.text".into(),
                    "metrics.costMicros".into(),
                ],
                rows: vec![vec![json!("1"), json!("meeting bot"), json!("240000")]],
            }
        );
        assert_eq!(
            tabulate(&json!({})),
            Table {
                columns: vec![],
                rows: vec![]
            }
        );
    }

    #[test]
    fn api_error_detail_keeps_codes_and_messages_only() {
        let body = r#"{"error":{"code":400,"message":"Request contains an invalid argument.","status":"INVALID_ARGUMENT","details":[{"@type":"type.googleapis.com/google.ads.googleads.v25.errors.GoogleAdsFailure","errors":[{"errorCode":{"queryError":"UNRECOGNIZED_FIELD"},"message":"Unrecognized fields in the query: 'campaign.start_date'."},{"errorCode":{"fieldError":"REQUIRED"},"message":"missing","trigger":{"stringValue":"secret"},"location":{"fieldPathElements":[{"fieldName":"operations","index":0},{"fieldName":"update"}]}}],"requestId":"abc"}]}}"#;
        let detail = extract_api_error_detail(body).unwrap();
        assert_eq!(
            detail,
            "queryError.UNRECOGNIZED_FIELD: Unrecognized fields in the query: 'campaign.start_date'.; fieldError.REQUIRED: missing (at operations[0].update)"
        );
        assert!(!detail.contains("secret"));
        assert!(!detail.contains("abc"));
        assert_eq!(extract_api_error_detail("<html>not json</html>"), None);
        assert_eq!(
            extract_api_error_detail(r#"{"error":{"message":"x"}}"#),
            None
        );
    }

    #[test]
    fn api_error_detail_is_bounded() {
        let long = "x".repeat(2000);
        let body = format!(
            r#"{{"error":{{"details":[{{"errors":[{{"errorCode":{{"queryError":"BAD"}},"message":"{long}"}}]}}]}}}}"#
        );
        let detail = extract_api_error_detail(&body).unwrap();
        assert!(detail.chars().count() <= MAX_ERROR_DETAIL_CHARS + 1);
        assert!(detail.ends_with('…'));
    }

    #[test]
    fn report_query_adds_filters_and_daily_segmentation() {
        let filter = ReportFilter {
            campaign_id: Some("123".into()),
            ad_group_id: Some("456".into()),
            daily: true,
        };
        let query = build_report_query(ReportQuery {
            select: "campaign.id, ad_group.id, metrics.clicks",
            from: "ad_group",
            conditions: vec![date_condition("2026-09-01", "2026-09-04").unwrap()],
            order_by: Some("metrics.cost_micros DESC"),
            limit: 50,
            filter: &filter,
        })
        .unwrap();
        assert_eq!(
            query,
            "SELECT segments.date, campaign.id, ad_group.id, metrics.clicks FROM ad_group \
             WHERE segments.date BETWEEN '2026-09-01' AND '2026-09-04' \
             AND campaign.id = 123 AND ad_group.id = 456 \
             ORDER BY segments.date, metrics.cost_micros DESC LIMIT 50"
        );
    }

    #[test]
    fn report_query_selects_filtered_fields_google_requires() {
        let filter = ReportFilter {
            campaign_id: Some("123".into()),
            ad_group_id: Some("456".into()),
            daily: false,
        };
        let query = build_report_query(ReportQuery {
            select: "landing_page_view.unexpanded_final_url, metrics.clicks",
            from: "landing_page_view",
            conditions: Vec::new(),
            order_by: None,
            limit: 10,
            filter: &filter,
        })
        .unwrap();
        assert!(query.starts_with(
            "SELECT ad_group.id, campaign.id, landing_page_view.unexpanded_final_url, metrics.clicks FROM"
        ));
        assert!(selects_field("campaign.id, campaign.name", "campaign.id"));
        assert!(!selects_field(
            "campaign.id_x, campaign.name",
            "campaign.id"
        ));
    }

    #[test]
    fn report_query_rejects_non_numeric_filters_and_bad_limits() {
        let filter = ReportFilter {
            campaign_id: Some("123 OR 1=1".into()),
            ..ReportFilter::default()
        };
        assert!(build_report_query(ReportQuery {
            select: "campaign.id",
            from: "campaign",
            conditions: Vec::new(),
            order_by: None,
            limit: 10,
            filter: &filter,
        })
        .is_err());
        assert!(build_report_query(ReportQuery {
            select: "campaign.id",
            from: "campaign",
            conditions: Vec::new(),
            order_by: None,
            limit: 0,
            filter: &ReportFilter::default(),
        })
        .is_err());
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

    #[test]
    fn rsa_replacement_preserves_pinning() {
        let assets = json!([{
            "text": "99.5% SLA on Every Plan",
            "pinnedField": "HEADLINE_2",
            "assetPerformanceLabel": "PENDING"
        }]);
        let replacements = HashMap::from([("99.5% SLA on Every Plan", "Built for Production")]);
        let mut matched = HashSet::new();
        let result =
            replaced_text_assets(Some(&assets), &replacements, 30, "headline", &mut matched)
                .unwrap();
        assert_eq!(
            result,
            vec![json!({
                "text": "Built for Production",
                "pinnedField": "HEADLINE_2"
            })]
        );
        assert!(matched.contains("99.5% SLA on Every Plan"));
    }

    #[test]
    fn rsa_replacement_rejects_oversized_asset() {
        let assets = json!([{ "text": "old" }]);
        let replacements = HashMap::from([("old", "this headline is definitely too long")]);
        let mut matched = HashSet::new();
        assert!(
            replaced_text_assets(Some(&assets), &replacements, 30, "headline", &mut matched)
                .is_err()
        );
    }
}
