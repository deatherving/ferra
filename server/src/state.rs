use std::sync::Arc;

use sqlx::PgPool;
use tokio::sync::broadcast;

use crate::{config::Config, events::ChangeEvent};

pub struct AppState {
    pub config: Config,
    pub pool: PgPool,
    pub events_tx: broadcast::Sender<ChangeEvent>,
}

impl AppState {
    pub fn new(config: Config, pool: PgPool) -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            config,
            pool,
            events_tx: tx,
        }
    }
}

pub type SharedState = Arc<AppState>;
