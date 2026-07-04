mod config;
mod freshrss;
mod qdrant;
mod ollama;
mod email;
mod digest;

use clap::Parser;
use config::AppConfig;
use freshrss::FreshRSSClient;
use qdrant::QdrantClientWrapper;
use ollama::OllamaClient;
use email::EmailClient;
use digest::generate_and_send_digest;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

#[derive(Parser, Debug)]
#[command(name = "rss_digest", about = "Daily RSS digest with RAG-powered summarization")]
struct Cli {
    /// Run a single digest now (override cron schedule)
    #[arg(short, long)]
    run_once: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

    let config = AppConfig::load_from_env()?;
    let config = Arc::new(config);

    let freshrss_client = FreshRSSClient::new(config.freshrss.clone());
    let freshrss_client = Arc::new(freshrss_client);

    let qdrant_client = QdrantClientWrapper::new(config.qdrant.clone()).await?;
    let qdrant_client = Arc::new(qdrant_client);

    let ollama_client = OllamaClient::new(config.ollama.clone());
    let ollama_client = Arc::new(ollama_client);

    let email_client = EmailClient::new(config.smtp.clone())?;
    let email_client = Arc::new(email_client);

    let cli = Cli::parse();

    if cli.run_once {
        log::info!("Running single digest...");
        generate_and_send_digest(
            config.clone(),
            freshrss_client.clone(),
            qdrant_client.clone(),
            ollama_client.clone(),
            email_client.clone(),
        ).await?;
        return Ok(());
    }

    // Otherwise, run on the cron schedule
    log::info!("Starting digest scheduler: {}", config.cron.time);
    
    loop {
        let now = chrono::Utc::now();
        let hour = now.hour();
        let minute = now.minute();
        let schedule = &config.cron.time;
        let (sched_hour, sched_minute) = parse_cron_time(schedule)?;

        let next_run = if hour > sched_hour {
            // Next day
            chrono::Utc::now().date_naive().next_day().unwrap_or_else(|| now.date_naive())
                .and_hms_opt(sched_hour, sched_minute, 0).unwrap()
        } else if hour == sched_hour && minute > sched_minute {
            // Next day
            chrono::Utc::now().date_naive().next_day().unwrap_or_else(|| now.date_naive())
                .and_hms_opt(sched_hour, sched_minute, 0).unwrap()
        } else {
            chrono::Utc::now().date_naive().and_hms_opt(sched_hour, sched_minute, 0).unwrap()
        };

        let wait_duration = Duration::from_secs(
            (next_run - now).num_seconds().max(0) as u64
        );

        log::info!("Next digest run: {} (waiting {} seconds)", 
            next_run, wait_duration.as_secs());

        time::sleep(wait_duration).await;

        log::info!("Executing scheduled digest...");
        generate_and_send_digest(
            config.clone(),
            freshrss_client.clone(),
            qdrant_client.clone(),
            ollama_client.clone(),
            email_client.clone(),
        ).await?;

        // Brief pause before next loop
        time::sleep(Duration::from_secs(60)).await;
    }
}

fn parse_cron_time(time_str: &str) -> anyhow::Result<(u32, u32)> {
    let parts: Vec<&str> = time_str.split(':').collect();
    if parts.len() != 2 {
        return Err(anyhow::anyhow!("Invalid time format, expected HH:MM"));
    }
    let hour: u32 = parts[0].parse()?;
    let minute: u32 = parts[1].parse()?;
    if hour > 23 || minute > 59 {
        return Err(anyhow::anyhow!("Invalid time values"));
    }
    Ok((hour, minute))
}
