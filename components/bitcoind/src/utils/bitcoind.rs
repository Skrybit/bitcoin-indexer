use std::{thread::sleep, time::Duration};

use bitcoincore_rpc::{
    bitcoin::{BlockHash, Txid},
    Auth, Client, RpcApi,
};
use bitcoincore_rpc_json::GetRawTransactionResult;
use config::BitcoindConfig;

use crate::{try_error, try_info, types::BlockIdentifier, utils::Context};

pub fn bitcoind_get_client(config: &BitcoindConfig, ctx: &Context) -> Client {
    loop {
        let auth = Auth::UserPass(config.rpc_username.clone(), config.rpc_password.clone());
        match Client::new(&config.rpc_url, auth) {
            Ok(con) => {
                return con;
            }
            Err(e) => {
                try_error!(ctx, "bitcoind: Unable to get client: {}", e.to_string());
                sleep(Duration::from_secs(1));
            }
        }
    }
}

/// Retrieves the chain tip from bitcoind.
/// Uses raw JSON-RPC to handle Bitcoin Core 28+ `warnings` field change.
pub fn bitcoind_get_chain_tip(config: &BitcoindConfig, ctx: &Context) -> BlockIdentifier {
    let bitcoin_rpc = bitcoind_get_client(config, ctx);
    loop {
        match bitcoin_rpc.call::<serde_json::Value>("getblockchaininfo", &[]) {
            Ok(result) => {
                let blocks = result["blocks"].as_u64().unwrap_or(0);
                let hash = result["bestblockhash"].as_str().unwrap_or("0000000000000000000000000000000000000000000000000000000000000000");
                return BlockIdentifier {
                    index: blocks,
                    hash: format!("0x{}", hash),
                };
            }
            Err(e) => {
                try_error!(
                    ctx,
                    "bitcoind: Unable to get block height: {}",
                    e.to_string()
                );
                sleep(Duration::from_secs(1));
            }
        };
    }
}

/// Retrieves the block_height for a given blockhash.
pub fn bitcoind_get_block_height(
    bitcoin_rpc: &Client,
    ctx: &Context,
    blockhash: &BlockHash,
) -> Result<u32, String> {
    bitcoin_rpc
        .get_block_header_info(blockhash)
        .map(|result| result.height.try_into().unwrap())
        .map_err(|e| {
            try_error!(
                ctx,
                "bitcoind: Unable to get block header info: {}",
                e.to_string()
            );
            e.to_string()
        })
}

/// Retrieves the raw transaction for a given txid.
pub fn bitcoin_get_raw_transaction(
    bitcoin_rpc: &Client,
    ctx: &Context,
    txid: &Txid,
) -> Result<GetRawTransactionResult, String> {
    bitcoin_rpc
        .get_raw_transaction_info(txid, None)
        .map_err(|e| {
            try_error!(ctx, "bitcoind: Unable to get raw transaction: {e}",);
            e.to_string()
        })
}

/// Checks if bitcoind is still synchronizing blocks and waits until it's finished if that is the case.
/// Uses raw JSON-RPC to handle Bitcoin Core 28+ response format changes.
pub fn bitcoind_wait_for_chain_tip(config: &BitcoindConfig, ctx: &Context) -> BlockIdentifier {
    let bitcoin_rpc = bitcoind_get_client(config, ctx);
    let mut confirmations = 0;
    let mut logged_info = false;
    loop {
        match bitcoin_rpc.call::<serde_json::Value>("getblockchaininfo", &[]) {
            Ok(result) => {
                let blocks = result["blocks"].as_u64().unwrap_or(0);
                let headers = result["headers"].as_u64().unwrap_or(0);
                let ibd = result["initialblockdownload"].as_bool().unwrap_or(true);
                let hash = result["bestblockhash"].as_str().unwrap_or("0000000000000000000000000000000000000000000000000000000000000000");

                if !ibd && blocks == headers {
                    confirmations += 1;
                    if confirmations == 10 {
                        try_info!(ctx, "bitcoind chain tip is at block #{}", blocks);
                        return BlockIdentifier {
                            index: blocks,
                            hash: format!("0x{}", hash),
                        };
                    }
                    if !logged_info {
                        try_info!(ctx, "bitcoind verifying chain tip...");
                        logged_info = true;
                    }
                } else {
                    confirmations = 0;
                    try_info!(ctx, "bitcoind has not reached chain tip, trying again...");
                }
            }
            Err(e) => {
                try_error!(ctx, "bitcoind error checking for chain tip: {e}");
            }
        };
        sleep(Duration::from_secs(1));
    }
}
