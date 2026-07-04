mod config;
mod digest;
mod email;
mod freshrss;
mod ollama;
mod qdrant;

use chrono::TimeZone;
use chrono_tz::Tz;
use clap::Parser;
use config::AppConfig;
use digest::generate_and_send_digest;
use email::EmailClient;
use freshrss::FreshRSSClient;
use ollama::OllamaClient;
use qdrant::QdrantClientWrapper;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;

#[derive(Parser, Debug)]
#[command(
    name = "rss_digest",
    about = "Daily RSS digest with RAG-powered summarization"
)]
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
        )
        .await?;
        return Ok(());
    }

    // Otherwise, run on the cron schedule
    let tz = Tz::from_str(&config.cron.timezone).unwrap_or_else(|e| {
        log::warn!(
            "Invalid timezone '{}', falling back to UTC: {}",
            config.cron.timezone,
            e
        );
        chrono_tz::UTC
    });

    log::info!(
        "Starting digest scheduler: {} ({})",
        config.cron.time,
        config.cron.timezone
    );

    loop {
        let now = chrono::Utc::now();
        let now_in_tz = now.with_timezone(&tz);
        let schedule = &config.cron.time;
        let (sched_hour, sched_minute) = parse_cron_time(schedule)?;

        // Build the target time in the user's timezone
        let today_naive = now_in_tz.date_naive();
        let target_naive = today_naive
            .and_hms_opt(sched_hour, sched_minute, 0)
            .unwrap_or_else(|| {
                (today_naive + chrono::Duration::days(1))
                    .and_hms_opt(sched_hour, sched_minute, 0)
                    .unwrap()
            });
        let target_in_tz = tz.from_utc_datetime(&target_naive);

        let next_run = if now_in_tz >= target_in_tz {
            // Past today, schedule for next day
            let next_day_naive = (today_naive + chrono::Duration::days(1))
                .and_hms_opt(sched_hour, sched_minute, 0)
                .unwrap_or_else(|| {
                    (today_naive + chrono::Duration::days(2))
                        .and_hms_opt(sched_hour, sched_minute, 0)
                        .unwrap()
                });
            tz.from_utc_datetime(&next_day_naive)
        } else {
            target_in_tz
        };

        // Convert back to UTC for sleep duration
        let next_run_utc = next_run.with_timezone(&chrono::Utc);
        let wait_duration = Duration::from_secs((next_run_utc - now).num_seconds().max(0) as u64);

        log::info!(
            "Next digest run: {} (waiting {} seconds)",
            next_run,
            wait_duration.as_secs()
        );

        time::sleep(wait_duration).await;

        log::info!("Executing scheduled digest...");
        if let Err(e) = generate_and_send_digest(
            config.clone(),
            freshrss_client.clone(),
            qdrant_client.clone(),
            ollama_client.clone(),
            email_client.clone(),
        )
        .await
        {
            log::error!("Digest generation failed: {}", e);
        }

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
