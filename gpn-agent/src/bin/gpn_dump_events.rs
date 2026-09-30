//! gpn_dump_events — decode System::Events for a block (diagnostic).
use gpn_agent::chain::signer::{self, GpnConfig};
use subxt::{backend::legacy::LegacyRpcMethods, backend::rpc::RpcClient, config::substrate::H256};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let endpoint = std::env::args().nth(1).unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let block_arg = std::env::args().nth(2);

    let client = signer::connect(&endpoint).await?;
    let rpc = RpcClient::from_insecure_url(&endpoint).await?;
    let methods = LegacyRpcMethods::<GpnConfig>::new(rpc.clone());
    let hh: H256 = match block_arg {
        Some(s) => {
            if s.starts_with("0x") {
                let b = hex::decode(&s[2..])?;
                let mut a = [0u8; 32];
                a.copy_from_slice(&b);
                H256(a)
            } else {
                let n: u64 = s.parse()?;
                let noh = subxt::backend::legacy::rpc_methods::NumberOrHex::Number(n);
                methods.chain_get_block_hash(Some(noh)).await?.unwrap()
            }
        }
        None => methods.chain_get_block_hash(None).await?.unwrap(),
    };
    println!("block {:?}", hh);

    let addr = subxt::dynamic::storage("System", "Events", vec![]);
    let thunk = client.storage().at(hh).fetch(&addr).await?.unwrap();
    let enc = thunk.into_encoded();
    let events = subxt::events::Events::<GpnConfig>::decode_from(enc, client.metadata());
    println!("event count: {}", events.len());
    for (i, ev) in events.iter().enumerate() {
        match ev {
            Ok(e) => {
                let fb = e.field_bytes();
                let hx: String = fb.iter().map(|b| format!("{:02x}", b)).collect();
                println!("[{i}] {}::{} fields=0x{}", e.pallet_name(), e.variant_name(), hx);
            }
            Err(err) => println!("[{i}] <decode error: {err}>"),
        }
    }
    Ok(())
}
