// Copyright (c) Mysten Labs, Inc.
// SPDX-License-Identifier: Apache-2.0

use clap::Parser;
use sui_indexer::errors::IndexerError;
use sui_indexer::store::PgIndexerStore;
use sui_indexer::{new_pg_connection_pool, Indexer, IndexerConfig};
use sui_node::metrics::start_prometheus_server;

#[tokio::main]
async fn main() -> Result<(), IndexerError> {
    let _guard = telemetry_subscribers::TelemetryConfig::new()
        .with_env()
        .init();

    let indexer_config = IndexerConfig::parse();
    let registry_service = start_prometheus_server(
        // NOTE: this parses the input host addr and port number for socket addr,
        // so unwrap() is safe here.
        format!(
            "{}:{}",
            indexer_config.client_metric_host, indexer_config.client_metric_port
        )
        .parse()
        .unwrap(),
    );

    let registry = registry_service.default_registry();
    let pg_connection_pool = new_pg_connection_pool(&indexer_config.db_url).await?;
    let store = PgIndexerStore::new(pg_connection_pool);

    Indexer::start(&indexer_config, &registry, store).await
}
