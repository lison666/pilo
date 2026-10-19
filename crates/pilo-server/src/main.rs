#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("--version") => {
            println!("{}", pilo_server::SERVER_VERSION);
        }
        Some("--fingerprint") => {
            let executable = std::env::current_exe()?;
            println!("{}", pilo_server::binary_fingerprint(&executable)?);
        }
        // Agent-facing interface: `pilo-server call <method> <params-json>
        // [binary-hex]` runs one request; `pilo-server call` alone reads
        // JSON-lines requests on stdin so multi-step flows (pi.start →
        // pi.send → pi.stop) share one ServerState. Events go to stderr.
        Some("call") => match args.next() {
            Some(method) => {
                let params = args
                    .next()
                    .map(|raw| serde_json::from_str(&raw))
                    .transpose()?
                    .unwrap_or(serde_json::Value::Null);
                let binary = args
                    .next()
                    .map(|raw| pilo_server::hex_decode(&raw))
                    .transpose()?
                    .map(|data| vec![data])
                    .unwrap_or_default();
                match pilo_server::call_once(&method, params, binary).await {
                    Ok((value, binary)) => println!(
                        "{}",
                        serde_json::json!({
                            "ok": true,
                            "result": value,
                            "binaryHex": binary.iter().map(|b| pilo_server::hex_encode(b)).collect::<Vec<_>>(),
                        })
                    ),
                    Err(error) => {
                        eprintln!("{}", serde_json::json!({"ok": false, "error": error}));
                        std::process::exit(1);
                    }
                }
            }
            None => pilo_server::call_jsonl().await?,
        },
        _ => pilo_server::serve_stdio().await?,
    }
    Ok(())
}
