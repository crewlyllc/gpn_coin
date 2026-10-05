//! gpn_cancel_jobs — cancel orphaned `Pending` marketplace jobs (requestor-only).
//!
//! Orphaned Pending jobs (e.g. created for an unservable model, or a fast-path
//! create that was never routed) never reach a terminal state and permanently
//! hold a global `MaxActiveJobs` slot → job creation eventually fails with
//! `TooManyActiveJobs`. This tool cancels every Pending job owned by the
//! configured signer (the requestor), refunding escrow and freeing the slots.
//!
//! Usage:
//!   gpn_cancel_jobs [endpoint] [signer_uri] [id ...]
//!   - no ids  → scan all jobs and cancel every Pending job owned by the signer
//!   - ids     → cancel exactly those ids (still must be Pending + owned)
//!
//! Env: ENDPOINT, SIGNER_URI.

use gpn_agent::chain::ChainClient;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .or_else(|| std::env::var("ENDPOINT").ok())
        .unwrap_or_else(|| "ws://127.0.0.1:9944".to_string());
    let signer_uri = args
        .get(2)
        .cloned()
        .or_else(|| std::env::var("SIGNER_URI").ok())
        .unwrap_or_else(|| "//Alice".to_string());

    let client = ChainClient::new(&endpoint).await?.with_signer(&signer_uri);
    let me = client
        .signer_address()
        .ok_or("could not resolve signer address")?;
    // Hex form (0x…) for comparison against get_job_state's requestor string.
    let me_hex = {
        use subxt::tx::Signer as _;
        let s = gpn_agent::chain::signer::Sr25519Signer::from_uri(&signer_uri)?;
        let aid = s.account_id();
        let b: &[u8] = aid.as_ref();
        format!("0x{}", hex::encode(b))
    };
    println!("gpn_cancel_jobs — endpoint={endpoint} signer={me}");

    let explicit: Vec<u64> = args
        .iter()
        .skip(3)
        .filter_map(|s| s.parse::<u64>().ok())
        .collect();

    // Which ids to consider?
    let ids: Vec<u64> = if !explicit.is_empty() {
        explicit
    } else {
        let next = client.get_next_job_id().await;
        (0..next).collect()
    };

    let mut cancelled = 0usize;
    let mut scanned = 0usize;
    for id in ids {
        let Some((state, _price, requestor, _provider)) = client.get_job_state(id).await else {
            continue;
        };
        scanned += 1;
        if state != "Pending" {
            continue;
        }
        let owned = matches!(requestor.as_deref(), Some(r) if r == me || r == me_hex);
        if !owned {
            continue;
        }
        match client.cancel_job(id).await {
            Ok(()) => {
                cancelled += 1;
                println!("  cancelled #{id}");
            }
            Err(e) => eprintln!("  cancel #{id} failed: {e}"),
        }
    }
    println!("done — scanned {scanned} job(s), cancelled {cancelled}");
    Ok(())
}
