mod channel;
mod config;
mod darwin;
mod domain;
mod handlers;
mod metrics;
mod notifier;
mod poller;
mod stations;
mod storage;

use anyhow::{Context, Result};
use domain::Channel;
use handlers::AppState;
use notifier::multi::MultiChannelNotifier;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;

#[tokio::main]
async fn main() -> Result<()> {
    let cfg = config::load().context("Failed to load configuration")?;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&cfg.logging.level));

    // _guard must live for the duration of main; dropping it flushes the writer.
    let _guard;

    match &cfg.logging.log_dir {
        Some(log_dir) => {
            let appender = tracing_appender::rolling::daily(log_dir, "waiting-for-a-signal.log");
            let (non_blocking, guard) = tracing_appender::non_blocking(appender);
            _guard = guard;
            tracing_subscriber::registry()
                .with(filter)
                .with(
                    tracing_subscriber::fmt::layer()
                        .with_writer(non_blocking)
                        .with_ansi(cfg.logging.ansi),
                )
                .init();
        }
        None => {
            _guard = tracing_appender::non_blocking(std::io::stdout()).1;
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(cfg.logging.ansi)
                .init();
        }
    }
    info!("Configuration loaded");

    let station_index = Arc::new(
        stations::StationIndex::load(&cfg.assets.crs_csv_path)
            .context("Failed to load CRS station list")?,
    );
    info!("Station index loaded");

    let user_store = Arc::new(
        storage::UserStore::load(&cfg.storage.user_data_dir)
            .await
            .context("Failed to load user store")?,
    );
    info!("User store loaded");

    // Metrics CSVs live next to the users directory, e.g. /var/lib/waiting-for-a-signal/metrics.csv.
    let metrics_store = Arc::new(metrics::MetricsStore::new(
        cfg.storage.user_data_dir.with_file_name("metrics.csv"),
        cfg.storage
            .user_data_dir
            .with_file_name("metrics_users.csv"),
    ));
    info!("Metrics store ready");

    let departure_source: Arc<dyn darwin::DepartureSource> = if cfg.darwin.simulate {
        info!("Darwin simulator enabled — using fake departure data");
        Arc::new(darwin::simulate::SimulatedDepartureSource::new())
    } else {
        Arc::new(darwin::DarwinClient::new(&cfg.darwin))
    };

    // -----------------------------------------------------------------------
    // Phase 1: open each channel's transport and collect its Notifier.
    // -----------------------------------------------------------------------

    let mut telegram_notifier: Option<Box<dyn notifier::Notifier>> = None;
    let mut telegram_bot: Option<teloxide::Bot> = None;

    if let Some(tg_cfg) = &cfg.telegram {
        let n = notifier::telegram::TelegramNotifier::new(&tg_cfg.token)
            .await
            .context("Failed to initialise Telegram notifier")?;
        telegram_bot = Some(n.bot().clone());
        telegram_notifier = Some(Box::new(n));
        info!("Telegram notifier ready");
    }

    let mut twilio_notifier: Option<Box<dyn notifier::Notifier>> = None;

    if let Some(tw_cfg) = &cfg.twilio {
        let n = channel::twilio::TwilioNotifier::new(tw_cfg)
            .context("Failed to create Twilio notifier")?;
        twilio_notifier = Some(Box::new(n));
        info!(from_number = %tw_cfg.from_number, "Twilio notifier ready");
    }

    // -----------------------------------------------------------------------
    // Phase 2: build the shared command state.
    // -----------------------------------------------------------------------

    let mut multi = MultiChannelNotifier::new();
    if let Some(n) = telegram_notifier {
        multi.register(Channel::Telegram, n);
    }
    if let Some(n) = twilio_notifier {
        multi.register(Channel::Twilio, n);
    }
    let multi_notifier: Arc<dyn notifier::Notifier> = Arc::new(multi);

    let (shutdown_tx, mut shutdown_rx) = tokio::sync::mpsc::unbounded_channel::<()>();

    let app_state = AppState {
        store: Arc::clone(&user_store),
        stations: Arc::clone(&station_index),
        darwin: Arc::clone(&departure_source),
        notifier: Arc::clone(&multi_notifier),
        metrics: Arc::clone(&metrics_store),
        polling_rows: cfg.polling.departure_rows,
        shutdown: shutdown_tx,
        kill_switch_enabled: cfg.kill_switch.enabled,
    };

    // -----------------------------------------------------------------------
    // Phase 3: start the polling loop and all channel adapters.
    // -----------------------------------------------------------------------

    let poller_handle = poller::spawn(
        Arc::clone(&user_store),
        Arc::clone(&departure_source),
        Arc::clone(&multi_notifier),
        Arc::clone(&station_index),
        cfg.polling.clone(),
        Arc::clone(&metrics_store),
    );

    use channel::ChannelAdapter as _;
    let mut adapter_tasks: Vec<tokio::task::JoinHandle<Result<()>>> = Vec::new();

    if let (Some(tg_cfg), Some(bot)) = (&cfg.telegram, telegram_bot) {
        let adapter = channel::telegram::TelegramAdapter::new(bot, app_state.clone());
        let _ = tg_cfg;
        adapter_tasks.push(tokio::spawn(async move { adapter.run().await }));
    }

    if let Some(tw_cfg) = &cfg.twilio {
        let adapter = channel::twilio::TwilioAdapter::new(tw_cfg, app_state.clone())
            .context("Failed to create Twilio adapter")?;
        adapter_tasks.push(tokio::spawn(async move { adapter.run().await }));
    }

    info!(
        adapters = adapter_tasks.len(),
        "All channel adapters started"
    );

    let abort_handles: Vec<_> = adapter_tasks.iter().map(|h| h.abort_handle()).collect();

    let adapters_done = async move {
        for task in adapter_tasks {
            task.await
                .context("Channel adapter task panicked")?
                .context("Channel adapter exited with error")?;
        }
        Ok::<(), anyhow::Error>(())
    };

    tokio::select! {
        _ = shutdown_rx.recv() => {
            info!("Kill command received, shutting down");
            // Wait for teloxide to complete its update cycle so it calls getUpdates
            // with an advanced offset, ACKing the /kill message before we abort.
            // Without this the update is re-delivered on every restart.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            for h in &abort_handles {
                h.abort();
            }
        }
        result = adapters_done => {
            result?;
        }
    }

    poller_handle.abort();
    Ok(())
}
