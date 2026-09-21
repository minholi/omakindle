mod amazon;
mod highlights;
mod models;
mod secrets;
mod socket;
mod state;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use serde::Serialize;

#[derive(Parser)]
#[command(name = "omakindle-backend", version, about = "Read-only read.amazon.com client for the Omarchy Kindle plugin")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch library, progress, and highlights once and print a JSON report.
    Fetch {
        #[arg(long)]
        cookies_file: PathBuf,
        #[arg(long)]
        device_token_file: PathBuf,
        #[arg(long, default_value = "us")]
        region: String,
        /// Book to fetch progress and highlights for; defaults to the newest book.
        #[arg(long)]
        asin: Option<String>,
        /// Fetch every library page instead of the first.
        #[arg(long)]
        all_pages: bool,
    },
    /// Print the TLS/HTTP2 fingerprint and headers this client sends.
    Probe {
        #[arg(long, default_value = "https://tls.peet.ws/api/all")]
        url: String,
    },
    /// Print the stored session result for debugging.
    CheckSession,
    /// Run the plugin daemon on a Unix socket.
    Serve {
        /// Socket path; defaults to $XDG_RUNTIME_DIR/omakindle/backend.sock.
        /// Must live in a private, user-owned directory.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct FetchReport {
    ok: bool,
    error: Option<String>,
    library_count: usize,
    first_book: Option<models::Book>,
    progress: Option<models::Progress>,
    percentage_read: Option<f64>,
    highlights: Option<models::Highlights>,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    match cli.command {
        Command::Fetch {
            cookies_file,
            device_token_file,
            region,
            asin,
            all_pages,
        } => {
            let report = run_fetch(cookies_file, device_token_file, region, asin, all_pages).await;
            println!(
                "{}",
                serde_json::to_string_pretty(&report).expect("report serializes")
            );
            if !report.ok {
                std::process::exit(1);
            }
        }
        Command::Probe { url } => {
            run_probe(&url).await;
        }
        Command::CheckSession => match secrets::load().await {
            Ok(Some((cookies, token, region))) => println!(
                "ok cookies={} token={} region={}",
                cookies.len(),
                token.len(),
                region
            ),
            Ok(None) => println!("load: none"),
            Err(error) => println!("load: error: {error}"),
        },
        Command::Serve { socket } => {
            let path = match socket {
                Some(path) => path,
                None => match socket::default_socket_path() {
                    Ok(path) => path,
                    Err(error) => {
                        eprintln!("omakindle-backend: refusing to start: {error}");
                        std::process::exit(1);
                    }
                },
            };
            let app = state::App::new();
            if let Err(error) = socket::serve(app, &path).await {
                eprintln!("omakindle-backend: serve failed on {}: {error}", path.display());
                std::process::exit(1);
            }
        }
    }
}

async fn run_probe(url: &str) {
    let client = match amazon::build_client() {
        Ok(client) => client,
        Err(error) => {
            eprintln!("probe failed: {error}");
            std::process::exit(1);
        }
    };
    let response = client
        .get(url)
        .send()
        .await
        .expect("probe request");
    let body = response.text().await.expect("probe body");
    if !url.contains("peet.ws") {
        println!("{body}");
        return;
    }
    let parsed: serde_json::Value = serde_json::from_str(&body).expect("probe json");
    let sent_headers: Vec<(String, String)> = parsed["http2"]["sent_frames"]
        .as_array()
        .and_then(|frames| {
            frames.iter().find_map(|frame| {
                let headers = frame.get("headers")?.as_array()?;
                Some(
                    headers
                        .iter()
                        .filter_map(|entry| {
                            let name = entry.get(0)?.as_str()?.trim_start_matches(':').to_string();
                            let value = entry.get(1)?.as_str()?.to_string();
                            Some((name, value))
                        })
                        .collect(),
                )
            })
        })
        .unwrap_or_default();
    let summary = serde_json::json!({
        "userAgent": parsed["user_agent"],
        "ja3": parsed["tls"]["ja3_hash"],
        "ja4": parsed["tls"]["ja4"],
        "akamai": parsed["http2"]["akamai_fingerprint_hash"],
        "headers": sent_headers,
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).expect("probe serializes")
    );
}

async fn run_fetch(
    cookies_file: PathBuf,
    device_token_file: PathBuf,
    region: String,
    asin: Option<String>,
    all_pages: bool,
) -> FetchReport {
    let mut report = FetchReport {
        ok: false,
        error: None,
        library_count: 0,
        first_book: None,
        progress: None,
        percentage_read: None,
        highlights: None,
    };

    let cookies = match read_secret(&cookies_file) {
        Ok(value) => value,
        Err(error) => {
            report.error = Some(error);
            return report;
        }
    };
    let device_token = match read_secret(&device_token_file) {
        Ok(value) => value,
        Err(error) => {
            report.error = Some(error);
            return report;
        }
    };

    let credentials = match amazon::Credentials::new(&cookies, &device_token) {
        Ok(value) => value,
        Err(error) => {
            report.error = Some(error.to_string());
            return report;
        }
    };

    let mut client = match amazon::Amazon::new(&region, credentials) {
        Ok(value) => value,
        Err(error) => {
            report.error = Some(error.to_string());
            return report;
        }
    };

    let books = match client.library(all_pages).await {
        Ok(value) => value,
        Err(error) => {
            report.error = Some(error.to_string());
            return report;
        }
    };
    report.library_count = books.len();
    report.first_book = books.first().cloned();

    let target = asin.or_else(|| books.first().map(|book| book.asin.clone()));
    let Some(target) = target else {
        report.ok = true;
        return report;
    };

    match client.progress(&target).await {
        Ok(progress) => report.progress = Some(progress),
        Err(error) => {
            report.error = Some(format!("progress: {error}"));
            return report;
        }
    }

    match client.percentage_read(&target).await {
        Ok(percentage) => report.percentage_read = Some(percentage),
        Err(error) => {
            report.error = Some(format!("percentage: {error}"));
            return report;
        }
    }

    match client.highlights(&target).await {
        Ok(value) => report.highlights = Some(value),
        Err(error) => {
            report.error = Some(format!("highlights: {error}"));
            return report;
        }
    }

    report.ok = true;
    report
}

fn read_secret(path: &PathBuf) -> Result<String, String> {
    std::fs::read_to_string(path)
        .map(|value| value.trim().to_string())
        .map_err(|error| format!("cannot read {}: {error}", path.display()))
}
