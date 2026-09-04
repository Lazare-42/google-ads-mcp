use std::sync::Arc;

use rmcp::{
    handler::server::wrapper::Parameters, model::*, schemars, tool, tool_handler, tool_router,
    ErrorData as McpError, ServerHandler,
};
use serde::{Deserialize, Serialize};

use crate::ads::{AdTextReplacement, BudgetAllocation, GoogleAdsClient};

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct EmptyArgs {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CustomerArgs {
    /// Optional 10-digit client customer ID. Defaults to GOOGLE_ADS_CUSTOMER_ID.
    pub customer_id: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DateRangeArgs {
    pub customer_id: Option<String>,
    /// Inclusive date in YYYY-MM-DD format.
    pub start_date: String,
    /// Inclusive date in YYYY-MM-DD format.
    pub end_date: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct LimitedDateRangeArgs {
    pub customer_id: Option<String>,
    pub start_date: String,
    pub end_date: String,
    /// Maximum rows, 1..1000. Defaults to 100.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct GaqlArgs {
    pub customer_id: Option<String>,
    /// One read-only Google Ads Query Language SELECT statement.
    pub query: String,
    /// nextPageToken returned by a previous call.
    pub page_token: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SegmentPerformanceArgs {
    pub customer_id: Option<String>,
    pub start_date: String,
    pub end_date: String,
    /// device, day_of_week, hour, or network.
    pub breakdown: String,
    /// Maximum rows, 1..1000. Defaults to 100.
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct CampaignStatusArgs {
    pub customer_id: Option<String>,
    pub campaign_id: String,
    /// ENABLED or PAUSED. REMOVED is deliberately unsupported.
    pub status: String,
    /// False/omitted returns a preview. True applies the change when mutations are enabled.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct KeywordUpdateArgs {
    pub customer_id: Option<String>,
    pub ad_group_id: String,
    pub criterion_id: String,
    /// Optional ENABLED or PAUSED status.
    pub status: Option<String>,
    /// Optional keyword max CPC in micros. Account daily budget remains unchanged.
    pub cpc_bid_micros: Option<u64>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NegativeKeywordArgs {
    pub customer_id: Option<String>,
    pub ad_group_id: String,
    pub text: String,
    /// EXACT, PHRASE, or BROAD.
    pub match_type: String,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct ResponsiveSearchAdTextArgs {
    pub customer_id: Option<String>,
    pub ad_group_id: String,
    pub ad_id: String,
    /// Exact existing text and its approved replacement. All entries must match.
    pub replacements: Vec<AdTextReplacement>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct BudgetRebalanceArgs {
    pub customer_id: Option<String>,
    /// Complete set of budgets participating in this atomic rebalance.
    pub allocations: Vec<BudgetAllocation>,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Clone)]
pub struct GoogleAdsServer {
    client: Arc<GoogleAdsClient>,
}

impl GoogleAdsServer {
    pub fn new(client: Arc<GoogleAdsClient>) -> Self {
        Self { client }
    }

    fn ok<T: Serialize>(value: &T) -> Result<CallToolResult, McpError> {
        let text = serde_json::to_string_pretty(value)
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        Ok(CallToolResult::success(vec![Content::text(text)]))
    }

    fn err(error: crate::error::Error) -> McpError {
        McpError::internal_error(error.to_string(), None)
    }
}

#[tool_router]
impl GoogleAdsServer {
    #[tool(
        description = "Show credential and mutation-gate readiness without revealing secrets.",
        annotations(read_only_hint = true)
    )]
    async fn get_setup_status(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        Self::ok(&self.client.setup_status())
    }

    #[tool(
        description = "List Google Ads customers directly accessible to the configured service account.",
        annotations(read_only_hint = true)
    )]
    async fn list_accessible_customers(
        &self,
        Parameters(_args): Parameters<EmptyArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .list_accessible_customers()
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Account-level spend, clicks, impressions, and conversions for a date range.",
        annotations(read_only_hint = true)
    )]
    async fn account_performance(
        &self,
        Parameters(args): Parameters<DateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .account_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Campaign spend and conversion performance, ordered by spend.",
        annotations(read_only_hint = true)
    )]
    async fn campaign_performance(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .campaign_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Ad-group spend, CPC, and conversion performance, ordered by spend.",
        annotations(read_only_hint = true)
    )]
    async fn ad_group_performance(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .ad_group_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Ad-level spend and conversion performance with RSA headlines, descriptions, and final URLs.",
        annotations(read_only_hint = true)
    )]
    async fn ad_performance(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .ad_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Keyword performance with match type, quality score, effective CPC bid, spend, and conversions.",
        annotations(read_only_hint = true)
    )]
    async fn keyword_performance(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .keyword_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Performance grouped by device, day of week, hour, or ad network for scheduling and targeting decisions.",
        annotations(read_only_hint = true)
    )]
    async fn traffic_segment_performance(
        &self,
        Parameters(args): Parameters<SegmentPerformanceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .traffic_segment_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                &args.breakdown,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Campaign performance by geographic location for country and presence/interest cleanup.",
        annotations(read_only_hint = true)
    )]
    async fn geographic_performance(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .geographic_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Paid landing-page spend and conversion performance by final URL.",
        annotations(read_only_hint = true)
    )]
    async fn landing_page_performance(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .landing_page_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Recent Google Ads change history (maximum API-supported history is 30 days).",
        annotations(read_only_hint = true)
    )]
    async fn change_history(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .change_history(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "List active Google Ads recommendations for review. No apply-recommendation tool is exposed.",
        annotations(read_only_hint = true)
    )]
    async fn recommendations(
        &self,
        Parameters(args): Parameters<CustomerArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .recommendations(args.customer_id.as_deref())
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "List configured conversion actions and whether each is primary/included in Conversions.",
        annotations(read_only_hint = true)
    )]
    async fn conversion_actions(
        &self,
        Parameters(args): Parameters<CustomerArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .conversion_actions(args.customer_id.as_deref())
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Conversion performance grouped by conversion action for a date range.",
        annotations(read_only_hint = true)
    )]
    async fn conversion_performance(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .conversion_performance(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Google Ads search terms with spend and conversions. Email/phone-like values are redacted best-effort; free text can still contain PII and must be treated as sensitive.",
        annotations(read_only_hint = true)
    )]
    async fn search_terms(
        &self,
        Parameters(args): Parameters<LimitedDateRangeArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .search_terms(
                args.customer_id.as_deref(),
                &args.start_date,
                &args.end_date,
                args.limit.unwrap_or(100),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Run one arbitrary read-only GAQL SELECT query. Unredacted search terms, direct lead/user data, and click-level identifiers are blocked; no mutate endpoint is exposed.",
        annotations(read_only_hint = true)
    )]
    async fn run_gaql(
        &self,
        Parameters(args): Parameters<GaqlArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .run_gaql(
                args.customer_id.as_deref(),
                &args.query,
                args.page_token.as_deref(),
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Preview or apply ENABLED/PAUSED to one campaign. Never changes configured budgets and never removes campaigns.",
        annotations(destructive_hint = true)
    )]
    async fn set_campaign_status(
        &self,
        Parameters(args): Parameters<CampaignStatusArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .set_campaign_status(
                args.customer_id.as_deref(),
                &args.campaign_id,
                &args.status,
                args.confirm,
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Preview or apply keyword status/max-CPC changes. Bid increases are capped against the current effective bid; daily campaign budgets stay unchanged.",
        annotations(destructive_hint = true)
    )]
    async fn set_keyword(
        &self,
        Parameters(args): Parameters<KeywordUpdateArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .set_keyword(
                args.customer_id.as_deref(),
                &args.ad_group_id,
                &args.criterion_id,
                args.status.as_deref(),
                args.cpc_bid_micros,
                args.confirm,
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Preview or add one ad-group negative keyword. This can only restrict spend, not increase budget.",
        annotations(destructive_hint = true)
    )]
    async fn add_negative_keyword(
        &self,
        Parameters(args): Parameters<NegativeKeywordArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .add_negative_keyword(
                args.customer_id.as_deref(),
                &args.ad_group_id,
                &args.text,
                &args.match_type,
                args.confirm,
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Preview or atomically replace exact text in one enabled responsive search ad. Creates the corrected RSA and pauses only the selected old RSA; final URLs and URL tracking fields are preserved.",
        annotations(destructive_hint = true)
    )]
    async fn replace_responsive_search_ad_text(
        &self,
        Parameters(args): Parameters<ResponsiveSearchAdTextArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .replace_responsive_search_ad_text(
                args.customer_id.as_deref(),
                &args.ad_group_id,
                &args.ad_id,
                &args.replacements,
                args.confirm,
            )
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }

    #[tool(
        description = "Preview or atomically rebalance existing daily budgets. Hard guard rejects any increase in the summed budgets supplied.",
        annotations(destructive_hint = true)
    )]
    async fn rebalance_budgets(
        &self,
        Parameters(args): Parameters<BudgetRebalanceArgs>,
    ) -> Result<CallToolResult, McpError> {
        let value = self
            .client
            .rebalance_budgets(args.customer_id.as_deref(), &args.allocations, args.confirm)
            .await
            .map_err(Self::err)?;
        Self::ok(&value)
    }
}

#[tool_handler]
impl ServerHandler for GoogleAdsServer {
    fn get_info(&self) -> ServerInfo {
        let mut implementation = Implementation::from_build_env();
        implementation.name = env!("CARGO_PKG_NAME").to_owned();
        implementation.version = env!("CARGO_PKG_VERSION").to_owned();
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(implementation)
            .with_instructions(
                "Google Ads reporting and guarded optimization for Meeting BaaS. Read first, then preview mutations with confirm=false. Apply only after reviewing the preview and setting confirm=true. Mutations also require a server-side enable flag and explicit customer allowlist. Keyword bid increases are capped against the current Google Ads value. Budget rebalances are atomic, limited to DAILY budgets, and rejected when the requested sum exceeds the current sum. RSA text replacement fetches one known enabled ad, requires exact source text, preserves its URL configuration, then atomically creates the corrected RSA and pauses only that selected ad. Search-term email/phone-like values are redacted best-effort, but all report output remains sensitive. Raw GAQL blocks direct lead/user data, unredacted search terms, and click-level identifiers. No unrestricted mutate, campaign removal, arbitrary ad creation, or conversion deletion tool exists.",
            )
    }
}
