# google-ads-mcp

Google Ads API v25 MCP for Meeting BaaS reporting and guarded optimization.
It supports stdio and Streamable HTTP (`/mcp`).

## Safety model

- Reporting uses GAQL `SELECT` only.
- No unrestricted mutate endpoint exists.
- Every write tool previews by default and requires `confirm=true`.
- Writes are globally gated by `GOOGLE_ADS_MUTATIONS_ENABLED=true` and a
  customer allowlist.
- Campaign/keyword changes do not change configured daily budgets.
- Budget rebalances fetch current values first and reject a requested sum
  above the current sum. The accepted operations are sent atomically.
- Confirmed changes are written to the systemd journal without credentials.
- Campaign removal, ad creation, conversion deletion, and raw mutations are
  deliberately not exposed.

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
```

Set `GOOGLE_APPLICATION_CREDENTIALS` separately to a service-account JSON key.
Customer IDs may be entered with hyphens; the server normalizes them.

Keep mutations disabled until `list_accessible_customers`, account reports,
and preview results have all been checked against the Google Ads UI.

## Tools

Read: `get_setup_status`, `list_accessible_customers`, `account_performance`,
`campaign_performance`, `conversion_actions`, `conversion_performance`,
`search_terms`, `run_gaql`.

Guarded write: `set_campaign_status`, `set_keyword`,
`add_negative_keyword`, `rebalance_budgets`.
