//! gpn_dump_pallets — print pallets (name → index) from the node's best-block
//! metadata, optionally with a named call's index. Diagnostic for dynamic-call
//! encoding: a wrong pallet/call index silently dispatches the wrong call.
//!
//! Usage: gpn_dump_pallets [endpoint] [pallet_filter] [call_name]
use gpn_agent::chain::signer;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    let endpoint = args
        .get(1)
        .cloned()
        .unwrap_or_else(|| "ws://127.0.0.1:9944".into());
    let filter = args.get(2).cloned();
    let call = args.get(3).cloned();

    let client = signer::connect(&endpoint).await?;
    let md = client.metadata();
    let pallets = md.pallets();
    println!("pallets: {}", pallets.len());
    for p in pallets {
        let matches = filter.as_deref().map(|f| f == p.name()).unwrap_or(true);
        if !matches {
            continue;
        }
        print!("  [{:>3}] {}", p.index(), p.name());
        if let Some(c) = &call {
            match p.call_variant_by_name(c) {
                Some(v) => print!("  call '{c}' idx={}", v.index),
                None => print!("  call '{c}' <none>"),
            }
        }
        println!();
    }
    Ok(())
}
