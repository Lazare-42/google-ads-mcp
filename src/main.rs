mod ads;
mod auth;
mod error;
mod server;

use std::{env, sync::Arc};

use anyhow::{Context, Result};
use rmcp::{
    transport::{
        io::stdio,
        streamable_http_server::{
            session::local::LocalSessionManager, StreamableHttpServerConfig, StreamableHttpService,
        },
    },
    ServiceExt,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::{
    ads::{AdsConfig, GoogleAdsClient},
    auth::GoogleAuth,
    server::GoogleAdsServer,
};

enum Transport {
    Stdio,
    Http { bind: String },
}

fn parse_transport() -> Result<Transport> {
    let mut args = env::args().skip(1);
    let mut http_bind = None;
    let mut force_stdio = false;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--stdio" => force_stdio = true,
            "--http" => {
                http_bind = Some(args.next().unwrap_or_else(|| "127.0.0.1:8080".into()));
            }
            "-h" | "--help" => {
                print_usage();
                std::process::exit(0);
            }
            other => anyhow::bail!("unknown argument: {other}. Use --help for usage."),
        }
    }
    if force_stdio && http_bind.is_some() {
        anyhow::bail!("cannot combine --stdio and --http");
    }
    if let Some(bind) = http_bind {
        return Ok(Transport::Http { bind });
    }
    if let Ok(bind) = env::var("GOOGLE_ADS_MCP_HTTP_BIND") {
        if !bind.is_empty() {
            return Ok(Transport::Http { bind });
        }
    }
    Ok(Transport::Stdio)
}

fn print_usage() {
    eprintln!(
        "google-ads-mcp {version}\n\nUSAGE:\n  google-ads-mcp [--stdio | --http <BIND>]\n\nENV:\n  GOOGLE_APPLICATION_CREDENTIALS       service-account JSON key path\n  GOOGLE_ADS_DEVELOPER_TOKEN            Google Ads API developer token\n  GOOGLE_ADS_CUSTOMER_ID                default 10-digit client customer ID\n  GOOGLE_ADS_LOGIN_CUSTOMER_ID          optional manager account ID\n  GOOGLE_ADS_API_VERSION                default v25\n  GOOGLE_ADS_MUTATIONS_ENABLED          true to permit confirmed guarded writes\n  GOOGLE_ADS_ALLOWED_CUSTOMER_IDS       comma-separated mutation allowlist\n  GOOGLE_ADS_MAX_BID_INCREASE_PERCENT   maximum keyword bid increase, default 25\n",
        version = env!("CARGO_PKG_VERSION")
    );
}

fn build_client() -> Result<GoogleAdsClient> {
    let auth = GoogleAuth::from_env().context("failed to load Google service-account key")?;
    let config = AdsConfig::from_env().context("failed to load Google Ads configuration")?;
    GoogleAdsClient::new(auth, config).context("failed to build Google Ads client")
}

#[tokio::main]
async fn main() -> Result<()> {
    let transport = parse_transport()?;
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .init();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "starting google-ads-mcp"
    );
    let client = Arc::new(build_client()?);

    match transport {
        Transport::Stdio => {
            let service = GoogleAdsServer::new(client)
                .serve(stdio())
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            service.waiting().await?;
        }
        Transport::Http { bind } => {
            let cancellation = tokio_util::sync::CancellationToken::new();
            let service = StreamableHttpService::new(
                {
                    let client = client.clone();
                    move || Ok(GoogleAdsServer::new(client.clone()))
                },
                LocalSessionManager::default().into(),
                StreamableHttpServerConfig::default()
                    .with_cancellation_token(cancellation.child_token()),
            );
            let router = axum::Router::new().nest_service("/mcp", service);
            let listener = tokio::net::TcpListener::bind(&bind)
                .await
                .with_context(|| format!("failed to bind {bind}"))?;
            tracing::info!(%bind, "listening (endpoint /mcp)");
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = tokio::signal::ctrl_c().await;
                    cancellation.cancel();
                })
                .await?;
        }
    }
    Ok(())
}
