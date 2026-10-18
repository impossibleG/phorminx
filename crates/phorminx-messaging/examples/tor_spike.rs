//! M0 spike: host an onion service in one process and connect to it from another.
//!
//! serve <state_dir>            prints the onion address, echoes lines back
//! connect <state_dir> <addr>   sends N pings and prints timing
use std::time::Instant;

use arti_client::{TorClient, TorClientConfig, config::CfgPath};
use futures::StreamExt;
use safelog::DisplayRedacted;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tor_cell::relaycell::msg::Connected;
use tor_hsservice::{HsNickname, config::OnionServiceConfigBuilder, handle_rend_requests};

fn config(dir: &str) -> anyhow::Result<TorClientConfig> {
    let mut b = TorClientConfig::builder();
    b.storage()
        .state_dir(CfgPath::new_literal(format!("{dir}/state")))
        .cache_dir(CfgPath::new_literal(format!("{dir}/cache")));
    Ok(b.build()?)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("");
    let dir = args.get(2).cloned().unwrap_or_else(|| "spike".into());
    let t0 = Instant::now();
    let client = TorClient::create_bootstrapped(config(&dir)?).await?;
    println!("bootstrapped in {:?}", t0.elapsed());
    match mode {
        "serve" => {
            let svc = OnionServiceConfigBuilder::default()
                .nickname(HsNickname::new("spike".into())?)
                .build()?;
            let (running, rend) = client.launch_onion_service(svc)?.expect("enabled");
            println!(
                "address {}",
                running.onion_address().expect("hsid").display_unredacted()
            );
            let mut status = running.status_events();
            let t1 = Instant::now();
            tokio::spawn(async move {
                while let Some(s) = status.next().await {
                    println!("[{:?}] status {:?}", t1.elapsed(), s.state());
                }
            });
            let mut streams = handle_rend_requests(rend);
            while let Some(req) = streams.next().await {
                tokio::spawn(async move {
                    let Ok(stream) = req.accept(Connected::new_empty()).await else {
                        return;
                    };
                    let (r, mut w) = tokio::io::split(stream);
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        if w.write_all(format!("{line}\n").as_bytes()).await.is_err() {
                            break;
                        }
                        let _ = w.flush().await;
                    }
                });
            }
        }
        "connect" => {
            let addr = args.get(3).expect("addr").clone();
            let t1 = Instant::now();
            let stream = client.connect((addr.as_str(), 80)).await?;
            println!("connected (descriptor+rendezvous) in {:?}", t1.elapsed());
            let (r, mut w) = tokio::io::split(stream);
            let mut lines = BufReader::new(r).lines();
            for i in 0..10 {
                let t = Instant::now();
                w.write_all(format!("ping {i}\n").as_bytes()).await?;
                w.flush().await?;
                let line = lines.next_line().await?.expect("echo");
                println!("rtt {i} {:?} ({line})", t.elapsed());
            }
        }
        _ => anyhow::bail!("usage: serve|connect <dir> [addr]"),
    }
    Ok(())
}
