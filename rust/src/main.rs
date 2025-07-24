#![allow(unused)]
use bitcoin::hex::DisplayHex;
use bitcoincore_rpc::bitcoin::Amount;
use bitcoincore_rpc::{Auth, Client, RpcApi};
use serde::Deserialize;
use serde_json::json;
use std::fs::File;
use std::io::Write;

// Node access params
const RPC_URL: &str = "http://127.0.0.1:18443"; // Default regtest RPC port
const RPC_USER: &str = "alice";
const RPC_PASS: &str = "password";

// You can use calls not provided in RPC lib API using the generic `call` function.
// An example of using the `send` RPC call, which doesn't have exposed API.
// You can also use serde_json `Deserialize` derivation to capture the returned json result.
fn send(rpc: &Client, addr: &str) -> bitcoincore_rpc::Result<String> {
    let args = [
        json!([{addr : 100 }]), // recipient address
        json!(null),            // conf target
        json!(null),            // estimate mode
        json!(null),            // fee rate in sats/vb
        json!(null),            // Empty option object
    ];

    #[derive(Deserialize)]
    struct SendResult {
        complete: bool,
        txid: String,
    }
    let send_result = rpc.call::<SendResult>("send", &args)?;
    assert!(send_result.complete);
    Ok(send_result.txid)
}

fn main() -> bitcoincore_rpc::Result<()> {
    // 1. Connect to Bitcoin Core RPC
    let rpc = Client::new(
        RPC_URL,
        Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()),
    )?;

    let miner_wallet = "Miner";
    let trader_wallet = "Trader";

    let wallets = rpc.list_wallets()?;

    if !wallets.contains(&miner_wallet.to_string()) {
        if rpc
            .call::<serde_json::Value>("loadwallet", &[json!(miner_wallet)])
            .is_err()
        {
            rpc.call::<serde_json::Value>(
                "createwallet",
                &[json!(miner_wallet), json!(false), json!(false)],
            )?;
        }
    }

    if !wallets.contains(&trader_wallet.to_string()) {
        if rpc
            .call::<serde_json::Value>("loadwallet", &[json!(trader_wallet)])
            .is_err()
        {
            rpc.call::<serde_json::Value>(
                "createwallet",
                &[json!(trader_wallet), json!(false), json!(false)],
            )?;
        }
    }

    let miner_rpc = Client::new(
        &format!("{}/wallet/{}", RPC_URL, miner_wallet),
        Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()),
    )?;

    let trader_rpc = Client::new(
        &format!("{}/wallet/{}", RPC_URL, trader_wallet),
        Auth::UserPass(RPC_USER.to_owned(), RPC_PASS.to_owned()),
    )?;

    let miner_addr = miner_rpc.call::<String>("getnewaddress", &[json!("Mining Reward")])?;
    let trader_addr = trader_rpc.call::<String>("getnewaddress", &[json!("Received")])?;

    let blocks = rpc.call::<Vec<String>>(
        "generatetoaddress",
        &[json!(101), json!(miner_addr.clone())],
    )?;

    // Block rewards require 100 confirmations before they can be spent
    // We mine 101 blocks: 1 to get the reward + 100 for maturity
    println!(
        "Mined 101 blocks. Last block hash: {}",
        blocks[blocks.len() - 1]
    );

    std::thread::sleep(std::time::Duration::from_secs(1));

    let initial_balance = miner_rpc.call::<f64>("getbalance", &[])?;
    println!("Initial miner balance: {} BTC", initial_balance);

    let consolidate_addr = miner_rpc.call::<String>("getnewaddress", &[json!("Consolidate")])?;
    miner_rpc.call::<String>(
        "sendtoaddress",
        &[
            json!(consolidate_addr.clone()),
            json!(initial_balance - 0.01),
        ],
    )?;

    rpc.call::<Vec<String>>("generatetoaddress", &[json!(1), json!(miner_addr.clone())])?;

    std::thread::sleep(std::time::Duration::from_secs(1));
    let txid =
        miner_rpc.call::<String>("sendtoaddress", &[json!(trader_addr.clone()), json!(20.0)])?;
    println!("Sent 20 BTC. TXID: {}", txid);

    // After sending 20 BTC but before mining confirmation block:
    // Check transaction in mempool
    let mempool_info = rpc.call::<serde_json::Value>("getmempoolentry", &[json!(txid)])?;
    println!("Transaction in mempool: {:?}", mempool_info);

    let confirm_block =
        rpc.call::<Vec<String>>("generatetoaddress", &[json!(1), json!(miner_addr.clone())])?;
    println!("Mined confirmation block: {}", confirm_block[0]);

    let blockhash = &confirm_block[0];
    let block_info = rpc.call::<serde_json::Value>("getblock", &[json!(blockhash)])?;
    let block_height = block_info["height"].as_u64().unwrap();

    let tx = miner_rpc.call::<serde_json::Value>("gettransaction", &[json!(txid.clone())])?;
    let decoded = rpc.call::<serde_json::Value>(
        "decoderawtransaction",
        &[json!(tx["hex"].as_str().unwrap())],
    )?;

    let vin_arr = decoded["vin"].as_array().unwrap();
    assert_eq!(
        vin_arr.len(),
        1,
        "Transaction should have exactly one input"
    );

    let input_txid = vin_arr[0]["txid"].as_str().unwrap();
    let input_vout = vin_arr[0]["vout"].as_u64().unwrap();
    let input_detail =
        rpc.call::<serde_json::Value>("getrawtransaction", &[json!(input_txid), json!(true)])?;
    let vout_obj = &input_detail["vout"][input_vout as usize]["scriptPubKey"];
    let input_addr = vout_obj
        .get("address")
        .and_then(|a| a.as_str())
        .or_else(|| {
            vout_obj
                .get("addresses")
                .and_then(|addrs| addrs.as_array())
                .and_then(|arr| arr.get(0))
                .and_then(|a| a.as_str())
        })
        .unwrap_or("<unknown address>");
    let input_amt = input_detail["vout"][input_vout as usize]["value"]
        .as_f64()
        .unwrap();

    let vouts = decoded["vout"].as_array().unwrap();
    let mut trader_output_amt = 0.0;
    let mut miner_change_amt = 0.0;
    let mut miner_change_addr = String::new();

    for vout in vouts {
        let value = vout["value"].as_f64().unwrap();
        let address = vout["scriptPubKey"]
            .get("address")
            .and_then(|a| a.as_str())
            .or_else(|| {
                vout["scriptPubKey"]
                    .get("addresses")
                    .and_then(|addrs| addrs.as_array())
                    .and_then(|arr| arr.get(0))
                    .and_then(|a| a.as_str())
            })
            .unwrap_or("");
        if address == trader_addr {
            trader_output_amt = value;
        } else if address != "" && address != trader_addr {
            miner_change_amt = value;
            miner_change_addr = address.to_string();
        }
    }

    let output_sum: f64 = vouts.iter().map(|v| v["value"].as_f64().unwrap()).sum();
    let fee = input_amt - output_sum;

    let mut out = File::create("../out.txt").expect("Failed to create output file");
    writeln!(out, "{}", txid).unwrap();
    writeln!(out, "{}", input_addr).unwrap();
    writeln!(out, "{}", input_amt).unwrap();
    writeln!(out, "{}", trader_addr).unwrap();
    writeln!(out, "{}", trader_output_amt).unwrap();
    writeln!(out, "{}", miner_change_addr).unwrap();
    writeln!(out, "{}", miner_change_amt).unwrap();
    writeln!(out, "{:.8}", fee).unwrap();
    writeln!(out, "{}", block_height).unwrap();
    writeln!(out, "{}", blockhash).unwrap();

    println!("Successfully wrote transaction details to out.txt");

    Ok(())
}
