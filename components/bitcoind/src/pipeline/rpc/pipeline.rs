use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::sleep,
    time::Duration,
};

use config::Config;
use crossbeam_channel::bounded;
use reqwest::Client;
use tokio::task::JoinSet;

use crate::{
    pipeline::{
        rpc::{
            parse_downloaded_block, standardize_bitcoin_block, try_download_block_bytes_with_retry,
        },
        wait_for_thread_finish, BlockProcessor, BlockProcessorCommand,
    },
    try_debug, try_error, try_info,
    types::{BitcoinBlockData, BitcoinNetwork, BlockBytesCursor},
    utils::Context,
};

/// Message passed from BlockCompressor threads to the BlockDispatcher thread.
/// SKRYBITDEV-586: the `Failed` variant carries error metadata so the dispatcher
/// can forward a `RecordFailed` command to the processor, which persists the
/// failure to the `failed_blocks` table for later retry.
///
/// Note: `compacted` here is the serialized bytes (`Vec<u8>`), not a
/// `BlockBytesCursor` — the cursor is a zero-copy view that can't cross
/// thread boundaries. The cursor is reconstructed downstream when needed.
enum DispatcherMessage {
    Ok {
        block_height: u64,
        block: Option<BitcoinBlockData>,
        compacted: Option<Vec<u8>>,
    },
    Failed {
        block_height: u64,
        error_kind: &'static str,
        error_message: String,
    },
}

/// Wraps `try_download_block_bytes_with_retry` to preserve the block height
/// through the JoinSet. On download failure, returns `(height, Err)` so the
/// orchestrator can skip-and-log instead of panicking.
async fn download_block_tagged(
    http_client: reqwest::Client,
    block_height: u64,
    bitcoin_config: config::BitcoindConfig,
    ctx: Context,
) -> (u64, Result<Vec<u8>, String>) {
    let result =
        try_download_block_bytes_with_retry(http_client, block_height, bitcoin_config, ctx).await;
    (block_height, result)
}

/// Downloads historical blocks from bitcoind's RPC interface and pushes them to a [BlockProcessor] so they can be indexed
/// or ingested as needed.
pub(crate) async fn start_block_download_pipeline(
    config: &Config,
    rpc_client: &Client,
    block_heights: Vec<u64>,
    start_sequencing_blocks_at_height: u64,
    compress_blocks: bool,
    block_processor: &mut BlockProcessor,
    abort_signal: &Arc<AtomicBool>,
    ctx: &Context,
) -> Result<(), String> {
    let number_of_blocks_to_process = block_heights.len() as u64;
    let start_block_height = *block_heights.first().expect("no blocks to pipeline");
    let end_block_height = *block_heights.last().expect("no blocks to pipeline");
    let mut block_heights = VecDeque::from(block_heights);

    let channel_capacity = config.resources.indexer_channel_capacity;
    let block_compressor_thread_count = config.resources.get_optimal_thread_pool_capacity();
    let rpc_thread_count = config.resources.bitcoind_rpc_threads;

    // BlockCompressor threads
    // ------------------------------------------------------------------------------------------------
    // Responsible for compressing the block bytes received from bitcoind into a compact and standardized format. As soon as we
    // get bytes back from wire, processing is moved to this thread pool to defer parsing.
    try_info!(
        ctx,
        "Pipeline spawning {} BlockCompressor threads",
        block_compressor_thread_count
    );
    // Create the channel that will be used to send parsed blocks to the BlockDispatcher thread for sorting.
    // SKRYBITDEV-586: the Option wraps DispatcherMessage; None is the termination signal.
    let (block_dispatcher_tx, block_dispatcher_rx): (
        crossbeam_channel::Sender<Option<DispatcherMessage>>,
        crossbeam_channel::Receiver<Option<DispatcherMessage>>,
    ) = crossbeam_channel::bounded(channel_capacity);

    let mut compressor_tx_pool = Vec::with_capacity(block_compressor_thread_count);
    let mut compressor_rx_pool = Vec::with_capacity(block_compressor_thread_count);
    let mut compressor_handles = Vec::with_capacity(block_compressor_thread_count);
    for _ in 0..block_compressor_thread_count {
        // Channel message: Option<(block_height, block_bytes)>.
        // Height is tagged here so that on parse failure the BlockCompressor
        // can report the failed block height to the dispatcher (skip marker)
        // without having to parse the bytes first (chicken/egg otherwise).
        let (tx, rx) = bounded::<Option<(u64, Vec<u8>)>>(channel_capacity);
        compressor_tx_pool.push(tx);
        compressor_rx_pool.push(rx);
    }

    let moved_ctx: Context = ctx.clone();
    let moved_bitcoin_network = config.bitcoind.network;
    for (thread_index, rx) in compressor_rx_pool.into_iter().enumerate() {
        let cloned_abort_signal = abort_signal.clone();
        let block_dispatcher_tx_moved = block_dispatcher_tx.clone();
        let moved_ctx: Context = moved_ctx.clone();
        let handle = hiro_system_kit::thread_named(&format!("BlockCompressor[{thread_index}]"))
            .spawn(move || {
                loop {
                    if cloned_abort_signal.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(Some((block_height, block_bytes))) = rx.recv() {
                        // Parse — on failure, send Failed skip marker to dispatcher so the
                        // cursor can advance past this height without stalling the pipeline.
                        // SKRYBITDEV-586: was `.expect("unable to parse block")` which
                        // panicked the thread and stuck the watermark.
                        let raw_block_data = match parse_downloaded_block(block_bytes) {
                            Ok(data) => data,
                            Err(e) => {
                                try_error!(
                                    moved_ctx,
                                    "BlockCompressor[{thread_index}]: parse failed for block #{block_height}, skipping. error={e}"
                                );
                                let _ =
                                    block_dispatcher_tx_moved.send(Some(DispatcherMessage::Failed {
                                        block_height,
                                        error_kind: "parse",
                                        error_message: e,
                                    }));
                                continue;
                            }
                        };

                        // Compress — on failure, same skip-and-log behavior.
                        let compressed_block = if compress_blocks {
                            match BlockBytesCursor::from_full_block(&raw_block_data) {
                                Ok(c) => Some(c),
                                Err(e) => {
                                    try_error!(
                                        moved_ctx,
                                        "BlockCompressor[{thread_index}]: compress failed for block #{block_height}, skipping. error={e}"
                                    );
                                    let _ = block_dispatcher_tx_moved.send(Some(
                                        DispatcherMessage::Failed {
                                            block_height,
                                            error_kind: "compress",
                                            error_message: e.to_string(),
                                        },
                                    ));
                                    continue;
                                }
                            }
                        } else {
                            None
                        };

                        // Standardize (only for blocks >= start_sequencing_blocks_at_height) —
                        // on failure, same skip-and-log behavior.
                        let block_data = if block_height >= start_sequencing_blocks_at_height {
                            match standardize_bitcoin_block(
                                raw_block_data,
                                &BitcoinNetwork::from_network(moved_bitcoin_network),
                                &moved_ctx,
                            ) {
                                Ok(block) => Some(block),
                                Err((e, _fatal)) => {
                                    try_error!(
                                        moved_ctx,
                                        "BlockCompressor[{thread_index}]: standardize failed for block #{block_height}, skipping. error={e}"
                                    );
                                    let _ = block_dispatcher_tx_moved.send(Some(
                                        DispatcherMessage::Failed {
                                            block_height,
                                            error_kind: "standardize",
                                            error_message: e,
                                        },
                                    ));
                                    continue;
                                }
                            }
                        } else {
                            None
                        };

                        let _ = block_dispatcher_tx_moved.send(Some(DispatcherMessage::Ok {
                            block_height,
                            block: block_data,
                            compacted: compressed_block,
                        }));
                    }
                }
                try_info!(moved_ctx, "BlockCompressor[{thread_index}] thread complete");
            })
            .expect("unable to spawn thread");
        compressor_handles.push(handle);
    }

    // BlockDispatcher thread
    // ------------------------------------------------------------------------------------------------
    // Responsible for sending sorted and standardized blocks to the [BlockProcessor] for canonicalization. Blocks must be sent in
    // order so the [BlockProcessor] can follow along the canonical chain.
    let cloned_ctx = ctx.clone();
    let cloned_abort_signal = abort_signal.clone();
    let block_processor_commands_tx = block_processor.commands_tx.clone();
    let block_dispatcher_thread = hiro_system_kit::thread_named("BlockDispatcher")
        .spawn(move || {
            let mut inbox = HashMap::new();
            let mut inbox_cursor = start_sequencing_blocks_at_height.max(start_block_height);
            let mut blocks_processed = 0;
            let mut stop_runloop = false;

            loop {
                if stop_runloop {
                    try_debug!(
                        cloned_ctx,
                        "Pipeline successfully sent {blocks_processed} blocks to processor"
                    );
                    let _ = block_processor_commands_tx.send(BlockProcessorCommand::Terminate);
                    break;
                }

                // Dequeue all the blocks available
                let mut new_messages = vec![];
                while let Ok(message) = block_dispatcher_rx.try_recv() {
                    match message {
                        Some(msg) => {
                            new_messages.push(msg);
                            // Max batch size: 10_000 blocks
                            if new_messages.len() >= 10_000 {
                                break;
                            }
                        }
                        None => {
                            break;
                        }
                    }
                }

                if blocks_processed == number_of_blocks_to_process {
                    stop_runloop = true;
                }

                // Early "continue"
                if new_messages.is_empty() {
                    sleep(Duration::from_millis(500));
                    continue;
                }

                let mut ooo_compacted_blocks = vec![];
                for msg in new_messages.into_iter() {
                    match msg {
                        DispatcherMessage::Ok {
                            block_height,
                            block,
                            compacted,
                        } => {
                            if let Some(block) = block {
                                inbox.insert(block_height, Some((block, compacted)));
                            } else if let Some(compacted_block) = compacted {
                                ooo_compacted_blocks.push((block_height, compacted_block));
                            } else {
                                // Neither block nor compacted — treat as skip
                                // (shouldn't normally happen on the Ok path).
                                inbox.insert(block_height, None);
                            }
                        }
                        DispatcherMessage::Failed {
                            block_height,
                            error_kind,
                            error_message,
                        } => {
                            // SKRYBITDEV-586: persist the failure to the DB via the
                            // BlockProcessor. The cursor also advances past this height
                            // (None inbox entry), unblocking downstream processing.
                            let _ = block_processor_commands_tx.send(
                                BlockProcessorCommand::RecordFailed {
                                    block_height,
                                    error_kind: error_kind.to_string(),
                                    error_message,
                                },
                            );
                            inbox.insert(block_height, None);
                        }
                    }
                }

                // Early "continue"
                if !ooo_compacted_blocks.is_empty() {
                    blocks_processed += ooo_compacted_blocks.len() as u64;
                    let _ =
                        block_processor_commands_tx.send(BlockProcessorCommand::ProcessBlocks {
                            compacted_blocks: ooo_compacted_blocks,
                            blocks: vec![],
                        });
                }

                if inbox.is_empty() {
                    continue;
                }

                // In order processing: construct the longest sequence of known blocks
                let mut compacted_blocks = vec![];
                let mut blocks = vec![];
                while let Some(entry) = inbox.remove(&inbox_cursor) {
                    if let Some((block, compacted_block)) = entry {
                        if let Some(compacted_block) = compacted_block {
                            compacted_blocks.push((inbox_cursor, compacted_block));
                        }
                        blocks.push(block);
                    }
                    // SKRYBITDEV-586: if entry is None (skip marker), we simply
                    // advance the cursor without pushing anything to the processor.
                    // The failure was already logged; the pipeline continues.
                    // Count toward blocks_processed so the loop termination condition
                    // (blocks_processed == number_of_blocks_to_process) can still be reached.
                    blocks_processed += 1;
                    inbox_cursor += 1;
                }

                if !blocks.is_empty() {
                    let _ =
                        block_processor_commands_tx.send(BlockProcessorCommand::ProcessBlocks {
                            compacted_blocks,
                            blocks,
                        });
                }

                if inbox_cursor > end_block_height || cloned_abort_signal.load(Ordering::SeqCst) {
                    stop_runloop = true;
                }
            }
            try_info!(cloned_ctx, "BlockDispatcher thread complete");
        })
        .expect("unable to spawn thread");

    // BitcoinRpc threads
    // ------------------------------------------------------------------------------------------------
    // Responsible for downloading block bytes from bitcoind's RPC interface in parallel. The number of threads is determined by
    // the `bitcoind_rpc_threads` configuration option.
    try_info!(
        ctx,
        "Pipeline spawning {} BitcoinRpc threads",
        rpc_thread_count
    );
    let mut rpc_handles = JoinSet::new();
    for _ in 0..rpc_thread_count {
        if let Some(block_height) = block_heights.pop_front() {
            let config = config.bitcoind.clone();
            let ctx = ctx.clone();
            let rpc_client = rpc_client.clone();
            // We interleave the initial requests to avoid DDOSing bitcoind from the get go.
            sleep(Duration::from_millis(500));
            rpc_handles.spawn(download_block_tagged(rpc_client, block_height, config, ctx));
        }
    }
    // As soon as we receive block bytes from bitcoind's RPC interface via any of the BitcoinRpc threads, we send them to the
    // BlockCompressor thread pool and download the next block.
    let mut round_robin_worker_thread_index = 0;
    while let Some(res) = rpc_handles.join_next().await {
        if abort_signal.load(Ordering::SeqCst) {
            break;
        }

        // SKRYBITDEV-586: handle download failures gracefully. JoinSet task may
        // itself have failed (join error) OR the download function returned Err.
        // In either case, we log and send a skip marker downstream so the pipeline
        // can advance past this height without stalling.
        let (block_height, block_bytes) = match res {
            Err(join_err) => {
                try_error!(
                    ctx,
                    "BitcoinRpc: download task panicked/cancelled, cannot identify block height. error={join_err}"
                );
                // We don't know which block_height this was; we can't emit a skip marker.
                // The BlockDispatcher will eventually time out when inbox_cursor exceeds
                // end_block_height. Try to queue the next block so the pipeline continues.
                if let Some(next_height) = block_heights.pop_front() {
                    let config = config.bitcoind.clone();
                    let ctx = ctx.clone();
                    let rpc_client = rpc_client.clone();
                    rpc_handles.spawn(download_block_tagged(
                        rpc_client, next_height, config, ctx,
                    ));
                }
                continue;
            }
            Ok((height, Err(download_err))) => {
                try_error!(
                    ctx,
                    "BitcoinRpc: download failed for block #{height}, skipping. error={download_err}"
                );
                // SKRYBITDEV-586: send Failed marker directly to dispatcher so
                // the cursor can advance + the failure gets persisted in the
                // `failed_blocks` table. Bypass the compressor since there's
                // nothing to parse.
                let _ = block_dispatcher_tx.send(Some(DispatcherMessage::Failed {
                    block_height: height,
                    error_kind: "download",
                    error_message: download_err,
                }));
                if let Some(next_height) = block_heights.pop_front() {
                    let config = config.bitcoind.clone();
                    let ctx = ctx.clone();
                    let rpc_client = rpc_client.clone();
                    rpc_handles.spawn(download_block_tagged(
                        rpc_client, next_height, config, ctx,
                    ));
                }
                continue;
            }
            Ok((height, Ok(bytes))) => (height, bytes),
        };

        loop {
            if abort_signal.load(Ordering::SeqCst) {
                break;
            }
            let res = compressor_tx_pool[round_robin_worker_thread_index]
                .send(Some((block_height, block_bytes.clone())));
            round_robin_worker_thread_index =
                (round_robin_worker_thread_index + 1) % block_compressor_thread_count;
            if res.is_ok() {
                break;
            }
            sleep(Duration::from_millis(500));
        }

        if let Some(block_height) = block_heights.pop_front() {
            let config = config.bitcoind.clone();
            let ctx = ctx.clone();
            let rpc_client = rpc_client.clone();
            rpc_handles.spawn(download_block_tagged(rpc_client, block_height, config, ctx));
        }
    }

    for tx in compressor_tx_pool.iter() {
        let _ = tx.send(None);
    }

    try_debug!(ctx, "Enqueued pipeline termination commands");

    for handle in compressor_handles.into_iter() {
        let _ = handle.join();
    }

    try_debug!(ctx, "Pipeline successfully terminated");

    wait_for_thread_finish(&mut block_processor.thread_handle)?;

    let _ = block_dispatcher_tx.send(None);

    let _ = block_dispatcher_thread.join();
    let _ = rpc_handles.shutdown().await;

    try_debug!(
        ctx,
        "Pipeline successfully processed sequence of blocks ({} to {})",
        start_block_height,
        end_block_height
    );

    Ok(())
}
