mod bot;
mod config;
mod constants;
mod database;
mod engine;
mod helpers;
mod maintenance;
mod meme_store;
mod migrations;
mod reranker;
mod tagger;
mod vlm;
mod worker;

use std::{collections::HashMap, sync::Arc, time::Instant};

use teloxide::Bot;
use tokio::sync::{Mutex, Semaphore};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

use crate::{maintenance::maintance_worker, meme_store::MemeStore};

const MAX_CONCURRENT_RERANK_SLOTS: usize = 2;
const DEFAULT_LOG_FILTER: &str =
    "info,m_nemosyne_m=info,teloxide=warn,ort=warn,reqwest=warn,hyper=warn,hyper_util=warn,h2=warn,rustls=warn";

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();
}

struct AppState {
    meme_store: Mutex<MemeStore>,
    engine: Mutex<engine::ClipEngine>,
    reranker: Option<Arc<std::sync::Mutex<reranker::RerankerEngine>>>,
    rerank_semaphore: Arc<Semaphore>,
    started_at: Instant,
    last_inline_query: Mutex<HashMap<i64, String>>,
    pending_reaction_resets: Mutex<HashMap<i64, bot::comand::PendingReactionReset>>,
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().ok();
    init_tracing();

    info!("Starting MnemosyneM bot");
    let app_config = config::load_config();

    info!("Loading storage and embedding models");

    let mut engine = engine::ClipEngine::new().expect("Failed to create clip engine");
    migrations::run(&app_config.db_path, &mut engine).expect("Failed to run migration");
    let store = MemeStore::open(&app_config.db_path).expect("Failed to open database");

    let reranker = if app_config.enable_reranker {
        info!("Loading Stage-2 reranker (JINARerankerV2BaseMultiligual)");
        match reranker::RerankerEngine::new() {
            Ok(r) => {
                info!("Stage-2 reranker loaded");
                Some(Arc::new(std::sync::Mutex::new(r)))
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to load Stage-2 reranker; falling back to Phase 1A search"
                );
                None
            }
        }
    } else {
        info!("Stage-2 reranker disabled via ENABLE_RERANKER=false");
        None
    };

    let state = Arc::new(AppState {
        meme_store: Mutex::new(store),
        engine: Mutex::new(engine),
        reranker,
        rerank_semaphore: Arc::new(Semaphore::new(MAX_CONCURRENT_RERANK_SLOTS)),
        started_at: Instant::now(),
        last_inline_query: Mutex::new(HashMap::new()),
        pending_reaction_resets: Mutex::new(HashMap::new()),
    });

    let bot = Bot::from_env();

    let (sender, receiver) = tokio::sync::mpsc::channel(100);
    worker::start_worker(
        receiver,
        sender.clone(),
        Arc::clone(&state),
        bot.clone(),
        app_config,
    );

    maintance_worker::start_maintenance(
        Arc::clone(&state),
        bot.clone(),
        app_config,
        sender.clone(),
    );

    bot::drip::start_drip_scheduler(Arc::clone(&state), bot.clone());

    info!("Bot dispatcher starting");

    bot::run_bot(bot, state, app_config, sender).await;

    info!("MnemosyneM bot shut down cleanly");
}
