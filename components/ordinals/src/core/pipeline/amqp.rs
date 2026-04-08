//! AMQP event publisher — publishes block.indexed events to RabbitMQ.
//!
//! Initialized once at startup. If AMQP is not configured or connection
//! fails, all publish calls are silent no-ops (indexing never fails
//! because of messaging).

use lapin::{
    options::{BasicPublishOptions, ExchangeDeclareOptions},
    types::FieldTable,
    BasicProperties, Channel, Connection, ConnectionProperties, ExchangeKind,
};
use tokio::sync::OnceCell;

static CHANNEL: OnceCell<Channel> = OnceCell::const_new();
static EXCHANGE_NAME: OnceCell<String> = OnceCell::const_new();

/// Initialize the AMQP connection and declare the exchange.
/// Called once at indexer startup. If AMQP_URL env var is set, it
/// overrides the config file URL (allows SOPS secret injection).
pub async fn init(url: &str, exchange: &str) -> Result<(), String> {
    let effective_url = std::env::var("AMQP_URL").unwrap_or_else(|_| url.to_string());
    let conn = Connection::connect(&effective_url, ConnectionProperties::default())
        .await
        .map_err(|e| format!("AMQP connect failed: {e}"))?;
    let channel = conn
        .create_channel()
        .await
        .map_err(|e| format!("AMQP channel failed: {e}"))?;
    channel
        .exchange_declare(
            exchange,
            ExchangeKind::Topic,
            ExchangeDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await
        .map_err(|e| format!("AMQP exchange declare failed: {e}"))?;

    EXCHANGE_NAME
        .set(exchange.to_string())
        .map_err(|_| "AMQP exchange name already set".to_string())?;
    CHANNEL
        .set(channel)
        .map_err(|_| "AMQP channel already initialized".to_string())?;
    Ok(())
}

/// Publish a block.indexed event. Silent no-op if AMQP not initialized.
pub async fn publish_block_event(
    routing_key: &str,
    block_height: u64,
    block_hash: &str,
    reveals: u64,
    transfers: u64,
    elapsed_ms: u64,
) -> Result<(), String> {
    let channel = match CHANNEL.get() {
        Some(ch) => ch,
        None => return Ok(()), // not configured
    };
    let exchange = match EXCHANGE_NAME.get() {
        Some(ex) => ex.as_str(),
        None => return Ok(()),
    };

    let payload = serde_json::json!({
        "event": "block.indexed",
        "block_height": block_height,
        "block_hash": block_hash,
        "reveals": reveals,
        "transfers": transfers,
        "elapsed_ms": elapsed_ms,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    channel
        .basic_publish(
            exchange,
            routing_key,
            BasicPublishOptions::default(),
            payload.to_string().as_bytes(),
            BasicProperties::default()
                .with_content_type("application/json".into())
                .with_delivery_mode(2), // persistent
        )
        .await
        .map_err(|e| format!("AMQP publish failed: {e}"))?
        .await
        .map_err(|e| format!("AMQP publish confirm failed: {e}"))?;

    Ok(())
}
