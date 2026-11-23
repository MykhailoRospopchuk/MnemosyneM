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

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Instant,
};

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

#[derive(Debug, Clone, Copy, Default)]
pub struct SearchMetricsSnapshot {
    pub count: u64,
    pub avg_embed_ms: u64,
    pub avg_stage1_ms: u64,
    pub avg_rerank_ms: u64,
    pub avg_total_ms: u64,
}

#[derive(Default)]
pub struct SearchMetrics {
    count: AtomicU64,
    total_embed_ms: AtomicU64,
    total_stage1_ms: AtomicU64,
    total_rerank_ms: AtomicU64,
    total_search_ms: AtomicU64,
}

impl SearchMetrics {
    pub fn record(&self, embed_ms: u64, stage1_ms: u64, rerank_ms: u64, total_ms: u64) {
        self.count.fetch_add(1, Ordering::Relaxed);
        self.total_embed_ms.fetch_add(embed_ms, Ordering::Relaxed);
        self.total_stage1_ms.fetch_add(stage1_ms, Ordering::Relaxed);
        self.total_rerank_ms.fetch_add(rerank_ms, Ordering::Relaxed);
        self.total_search_ms.fetch_add(total_ms, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> SearchMetricsSnapshot {
        let count = self.count.load(Ordering::Relaxed);
        if count == 0 {
            return SearchMetricsSnapshot::default();
        }
        SearchMetricsSnapshot {
            count,
            avg_embed_ms: self.total_embed_ms.load(Ordering::Relaxed) / count,
            avg_stage1_ms: self.total_stage1_ms.load(Ordering::Relaxed) / count,
            avg_rerank_ms: self.total_rerank_ms.load(Ordering::Relaxed) / count,
            avg_total_ms: self.total_search_ms.load(Ordering::Relaxed) / count,
        }
    }
}

struct AppState {
    meme_store: Mutex<MemeStore>,
    engine: Mutex<engine::ClipEngine>,
    reranker: Option<Arc<std::sync::Mutex<reranker::RerankerEngine>>>,
    rerank_semaphore: Arc<Semaphore>,
    vlm_router: Arc<vlm::VlmRouter>,
    search_metrics: SearchMetrics,
    started_at: Instant,
    last_inline_query: Mutex<HashMap<i64, String>>,
    pending_reaction_resets: Mutex<HashMap<i64, bot::comand::PendingReactionReset>>,
}

pub(crate) async fn persist_dirty_vlm_quotas(state: &AppState) {
    let dirty = state.vlm_router.drain_dirty_quotas().await;
    if dirty.is_empty() {
        return;
    }
    let store = state.meme_store.lock().await;
    for q in dirty {
        if let Err(e) = store.upsert_vlm_quota(&q) {
            warn!(model = %q.model_id, error = %e, "Failed to persist VLM quota state");
        }
    }
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
    let vlm_router = Arc::new(vlm::VlmRouter::new(store.load_vlm_quotas()));

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
        vlm_router,
        search_metrics: SearchMetrics::default(),
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
