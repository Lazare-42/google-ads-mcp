# google-ads-mcp

Google Ads API v25 MCP for Meeting BaaS reporting and guarded optimization.
It supports stdio and Streamable HTTP (`/mcp`).

## Safety model

- Reporting uses GAQL `SELECT` only.
- No unrestricted mutate endpoint exists.
- Every write tool previews by default and requires `confirm=true`.
- Writes are globally gated by `GOOGLE_ADS_MUTATIONS_ENABLED=true` and an
  explicit customer allowlist. The default reporting customer is not silently
  allowlisted.
- Campaign/keyword changes do not change configured daily budgets.
- Keyword bid increases are checked against the current Google Ads value and
  capped at 25% by default; decreases remain allowed.
- Budget rebalances fetch current values first and reject a requested sum
  above the current sum. The accepted operations are sent atomically.
- Confirmed changes are written to the systemd journal without credentials.
- Campaign removal, arbitrary ad creation, conversion deletion, and raw
  mutations are deliberately not exposed. RSA copy replacement is constrained
  to exact text in one known enabled ad and atomically pauses only that old ad.

## Privacy boundary

- Keep the HTTP listener on loopback and place authentication in the MCP proxy;
  the server does not implement HTTP authentication itself.
- Google Ads report output is commercially sensitive. `search_terms` replaces
  email- and phone/long-ID-like queries with `[REDACTED_POTENTIAL_PII]`, but
  free text can still contain names, addresses, or other PII that cannot be
  identified reliably. Ad and landing-page reports return ad copy and
  configured URLs. Do not persist tool responses in public logs.
- Arbitrary GAQL rejects the unredacted search-term field, direct user-access
  emails, change-author emails, click-level identifiers (`click_view`), Local
  Services leads/conversations, verification artifacts, and hotel
  reconciliation records. This is a conservative denylist, not a general DLP
  guarantee. OAuth error bodies are never copied into errors. Google Ads errors
  keep the HTTP status, the opaque request ID, and Google's own error codes and
  messages (for example `queryError.UNRECOGNIZED_FIELD: Unrecognized fields in
  the query: 'campaign.start_date'`). Those messages describe the caller's
  request, not account data; `trigger` values and the raw body are dropped and
  the detail is capped at 600 characters.
- Confirmed-mutation logs contain customer/resource IDs, but not keyword text,
  credentials, tokens, or upstream response bodies.

## Credentials

The service account needs Google Ads account access and the OAuth scope
`https://www.googleapis.com/auth/adwords`. Google Ads additionally requires a
developer token.

```dotenv
GOOGLE_ADS_DEVELOPER_TOKEN=
GOOGLE_ADS_CUSTOMER_ID=1234567890
GOOGLE_ADS_LOGIN_CUSTOMER_ID=
GOOGLE_ADS_API_VERSION=v25
GOOGLE_ADS_MUTATIONS_ENABLED=false
GOOGLE_ADS_ALLOWED_CUSTOMER_IDS=1234567890
GOOGLE_ADS_MAX_BID_INCREASE_PERCENT=25
```

Set `GOOGLE_APPLICATION_CREDENTIALS` separately to a service-account JSON key.
Customer IDs may be entered with hyphens; the server normalizes them.

Keep mutations disabled until `list_accessible_customers`, account reports,
and preview results have all been checked against the Google Ads UI. Enabling
mutations without an explicit `GOOGLE_ADS_ALLOWED_CUSTOMER_IDS` entry still
leaves every write blocked.

## Report output

Every report tool returns one compact JSON object:

```json
{"customerId":"1234567890","currency":"EUR","startDate":"2026-09-01","endDate":"2026-09-04",
 "rowCount":2,"limit":100,"columns":["campaign.id","campaign.name","metrics.costMicros"],
 "rows":[["21638916637","Meeting Bots","240000"],["...","...","..."]]}
```

- `columns` are Google's camelCase field paths from the response `fieldMask`;
  `rows` hold one value per column in the same order. Field names therefore
  appear once per response instead of once per row, and `resourceName`
  duplicates are dropped. In practice this is roughly an 8x smaller payload
  than the nested pretty-printed response.
- `*Micros` values are millionths of `currency`; int64 values arrive as strings.
- `limitReached: true` appears when `rowCount` equals `limit`; `nextPageToken`
  appears when Google paginated (`run_gaql` accepts it back as `page_token`).
- Date-ranged reports accept `campaign_id` and/or `ad_group_id` filters (digits
  only, injected as numeric literals) and `daily: true` for one row per entity
  per day. `campaign_performance` and `geographic_performance` accept only the
  campaign filter; `account_performance` and `conversion_performance` accept
  only `daily`.

## Tools

Read: `get_setup_status`, `list_accessible_customers`, `account_performance`,
`campaign_performance`, `ad_group_performance`, `ad_performance`,
`keyword_performance`, `traffic_segment_performance`,
`geographic_performance`, `landing_page_performance`, `change_history`,
`recommendations`, `conversion_actions`, `conversion_performance`,
`search_terms`, `run_gaql`.

Guarded write: `set_campaign_status`, `set_keyword`,
`add_negative_keyword`, `replace_responsive_search_ad_text`,
`rebalance_budgets`.
