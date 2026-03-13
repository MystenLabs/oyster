#![allow(missing_docs)]

mod client;
mod config;
mod output;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use client::{ApiError, OysterClient};
use config::ConfigError;
use output::Output;

#[derive(Parser)]
#[command(name = "oyster", about = "Oyster object storage CLI")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    url: Option<String>,
    #[arg(long, global = true)]
    api_key: Option<String>,
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Upload a file to a bucket
    Store {
        /// Path to the file to upload
        file: PathBuf,
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Object key (defaults to filename)
        #[arg(long)]
        key: Option<String>,
        /// Content type (guessed from extension if omitted)
        #[arg(long)]
        content_type: Option<String>,
    },
    /// Download a blob by bucket and key
    Read {
        /// Object key
        key: String,
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Write to file instead of stdout
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Delete a blob by bucket and key
    Delete {
        /// Object key
        key: String,
        /// Bucket name
        #[arg(long)]
        bucket: String,
    },
    /// List blobs in a bucket
    ListBlobs {
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Maximum number of results
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Create a new bucket
    CreateBucket {
        /// Bucket name
        name: String,
    },
    /// List all buckets
    ListBuckets {
        /// Maximum number of results
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Delete a bucket by name
    DeleteBucket {
        /// Bucket name
        name: String,
    },
    /// Create a new API key
    CreateApiKey,
    /// Revoke an API key
    RevokeApiKey {
        /// API key ID to revoke
        key_id: String,
    },
    /// Create a new S3 access key
    CreateAccessKey,
    /// List S3 access keys
    ListAccessKeys,
    /// Delete an S3 access key
    DeleteAccessKey {
        /// Access key ID to delete
        access_key_id: String,
    },
    /// Show wallet information
    Wallet,
    /// Show resolved configuration
    Info,
}

fn guess_content_type(path: &std::path::Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .as_deref()
    {
        Some("txt") => "text/plain",
        Some("html" | "htm") => "text/html",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        Some("yaml" | "yml") => "application/yaml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("gz" | "gzip") => "application/gzip",
        Some("tar") => "application/x-tar",
        Some("wasm") => "application/wasm",
        Some("csv") => "text/csv",
        Some("mp3") => "audio/mpeg",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error("{0}")]
    Config(#[from] ConfigError),
    #[error("{0}")]
    Api(#[from] ApiError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

impl CliError {
    fn exit_code(&self) -> i32 {
        match self {
            CliError::Api(e) => e.exit_code(),
            _ => 1,
        }
    }
}

// Command handlers

async fn cmd_store(
    client: &OysterClient,
    out: &Output,
    file: &std::path::Path,
    bucket: &str,
    key: &str,
    content_type: Option<&str>,
) -> Result<(), CliError> {
    let data = std::fs::read(file)?;
    let ct = content_type.unwrap_or_else(|| guess_content_type(file));
    let resp = client.store_blob(bucket, key, data, ct).await?;
    out.print(&resp, |r| {
        println!("Stored blob:");
        println!("  key:            {}", r.key);
        println!("  blob_id:        {}", r.blob_id);
        println!("  size:           {} bytes", r.size);
        println!("  md5:            {}", r.md5);
        if let Some(ref sui) = r.sui_object_id {
            println!("  sui_object_id:  {sui}");
        }
        if let Some(ref exp) = r.expires_at {
            println!("  expires_at:     {exp}");
        }
    });
    Ok(())
}

async fn cmd_read(
    client: &OysterClient,
    bucket: &str,
    key: &str,
    output_path: Option<&std::path::Path>,
) -> Result<(), CliError> {
    let (bytes, _content_type) = client.read_blob(bucket, key).await?;
    match output_path {
        Some(path) => {
            std::fs::write(path, &bytes)?;
            eprintln!("Wrote {} bytes to {}", bytes.len(), path.display());
        }
        None => {
            use std::io::Write;
            std::io::stdout().write_all(&bytes)?;
        }
    }
    Ok(())
}

async fn cmd_delete(
    client: &OysterClient,
    out: &Output,
    bucket: &str,
    key: &str,
) -> Result<(), CliError> {
    client.delete_blob(bucket, key).await?;
    out.success(&format!("Deleted blob {bucket}/{key}"));
    Ok(())
}

async fn cmd_list_blobs(
    client: &OysterClient,
    out: &Output,
    bucket: &str,
    limit: Option<u32>,
) -> Result<(), CliError> {
    let resp = client.list_blobs(bucket, None, limit).await?;
    out.print(&resp, |r| {
        println!(
            "{:<40} {:<20} {:>10} CREATED",
            "KEY", "CONTENT_TYPE", "SIZE"
        );
        for b in &r.data {
            println!(
                "{:<40} {:<20} {:>10} {}",
                b.key, b.content_type, b.size, b.created_at
            );
        }
        if let Some(ref cursor) = r.next_cursor {
            println!("\n(more results available, cursor: {cursor})");
        }
    });
    Ok(())
}

async fn cmd_create_bucket(
    client: &OysterClient,
    out: &Output,
    name: &str,
) -> Result<(), CliError> {
    let bucket = client.create_bucket(name).await?;
    out.print(&bucket, |b| {
        println!("Created bucket:");
        println!("  name:  {}", b.name);
    });
    Ok(())
}

async fn cmd_list_buckets(
    client: &OysterClient,
    out: &Output,
    limit: Option<u32>,
) -> Result<(), CliError> {
    let resp = client.list_buckets(None, limit).await?;
    out.print(&resp, |r| {
        println!("{:<20} CREATED", "NAME");
        for b in &r.data {
            println!("{:<20} {}", b.name, b.created_at);
        }
        if let Some(ref cursor) = r.next_cursor {
            println!("\n(more results available, cursor: {cursor})");
        }
    });
    Ok(())
}

async fn cmd_delete_bucket(
    client: &OysterClient,
    out: &Output,
    name: &str,
) -> Result<(), CliError> {
    client.delete_bucket(name).await?;
    out.success(&format!("Deleted bucket '{name}'"));
    Ok(())
}

async fn cmd_create_api_key(client: &OysterClient, out: &Output) -> Result<(), CliError> {
    let key = client.create_api_key().await?;
    out.print(&key, |k| {
        println!("API key created:");
        println!("  id:      {}", k.id);
        println!("  prefix:  {}", k.prefix);
        println!("  secret:  {}", k.secret);
        println!();
        println!("Save this secret — it cannot be retrieved again.");
    });
    Ok(())
}

async fn cmd_revoke_api_key(
    client: &OysterClient,
    out: &Output,
    key_id: &str,
) -> Result<(), CliError> {
    client.revoke_api_key(key_id).await?;
    out.success(&format!("Revoked API key {key_id}"));
    Ok(())
}

async fn cmd_create_access_key(client: &OysterClient, out: &Output) -> Result<(), CliError> {
    let key = client.create_access_key().await?;
    out.print(&key, |k| {
        println!("Access key created:");
        println!("  access_key_id:      {}", k.access_key_id);
        println!("  secret_access_key:  {}", k.secret_access_key);
        println!();
        println!("Save this secret — it cannot be retrieved again.");
    });
    Ok(())
}

async fn cmd_list_access_keys(client: &OysterClient, out: &Output) -> Result<(), CliError> {
    let keys = client.list_access_keys().await?;
    out.print(&keys, |keys| {
        println!("{:<24} {:<20} REVOKED", "ACCESS_KEY_ID", "CREATED");
        for k in keys {
            let revoked = k.revoked_at.as_deref().unwrap_or("-");
            println!("{:<24} {:<20} {}", k.access_key_id, k.created_at, revoked);
        }
    });
    Ok(())
}

async fn cmd_delete_access_key(
    client: &OysterClient,
    out: &Output,
    access_key_id: &str,
) -> Result<(), CliError> {
    client.delete_access_key(access_key_id).await?;
    out.success(&format!("Deleted access key {access_key_id}"));
    Ok(())
}

async fn cmd_wallet(client: &OysterClient, out: &Output) -> Result<(), CliError> {
    let resp = client.get_wallet().await?;
    out.print(&resp, |r| {
        println!("address: {}", r.address);
    });
    Ok(())
}

fn cmd_info(out: &Output, config_path: Option<&std::path::Path>, url: &str, api_key: Option<&str>) {
    if out.json {
        let info = serde_json::json!({
            "config_file": config_path.map(|p| p.display().to_string()),
            "url": url,
            "api_key_prefix": api_key.map(|k| &k[..k.len().min(12)]),
        });
        println!("{}", serde_json::to_string_pretty(&info).unwrap());
    } else {
        match config_path {
            Some(p) => println!("config: {}", p.display()),
            None => println!("config: (none)"),
        }
        println!("url:    {url}");
        match api_key {
            Some(k) => println!("key:    {}...", &k[..k.len().min(12)]),
            None => println!("key:    (not set)"),
        }
    }
}

#[tokio::main]
async fn main() {
    // reqwest pulls in rustls which needs an explicit CryptoProvider when
    // multiple backends (aws-lc-rs + ring) are in the dependency tree.
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let cli = Cli::parse();
    let out = Output { json: cli.json };

    let result = run(cli, &out).await;
    if let Err(e) = result {
        output::error(&e.to_string());
        std::process::exit(e.exit_code());
    }
}

async fn run(cli: Cli, out: &Output) -> Result<(), CliError> {
    match cli.command {
        // Public commands — only need URL
        Command::Read {
            ref key,
            ref bucket,
            ref output,
        } => {
            let (url, _) = config::resolve_url_only(cli.config.as_deref(), cli.url.as_deref())?;
            let client = OysterClient::new(url, None);
            cmd_read(&client, bucket, key, output.as_deref()).await
        }

        Command::Info => {
            // Info is special: best-effort resolve, never errors on missing fields
            let (file_config, config_path) =
                config::load_file_config(cli.config.as_deref()).unwrap_or_default();
            let url = cli
                .url
                .as_deref()
                .or(file_config.url.as_deref())
                .unwrap_or("(not set)");
            let api_key = cli.api_key.as_deref().or(file_config.api_key.as_deref());
            cmd_info(out, config_path.as_deref(), url, api_key);
            Ok(())
        }

        // Authenticated commands
        command => {
            let resolved = config::resolve(
                cli.config.as_deref(),
                cli.url.as_deref(),
                cli.api_key.as_deref(),
            )?;
            let client = OysterClient::new(resolved.url, Some(resolved.api_key));

            match command {
                Command::Store {
                    ref file,
                    ref bucket,
                    ref key,
                    ref content_type,
                } => {
                    let default_key = file
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unnamed".to_string());
                    let key = key.as_deref().unwrap_or(&default_key);
                    cmd_store(&client, out, file, bucket, key, content_type.as_deref()).await
                }
                Command::Delete {
                    ref key,
                    ref bucket,
                } => cmd_delete(&client, out, bucket, key).await,
                Command::ListBlobs { ref bucket, limit } => {
                    cmd_list_blobs(&client, out, bucket, limit).await
                }
                Command::CreateBucket { ref name } => cmd_create_bucket(&client, out, name).await,
                Command::ListBuckets { limit } => cmd_list_buckets(&client, out, limit).await,
                Command::DeleteBucket { ref name } => cmd_delete_bucket(&client, out, name).await,
                Command::CreateApiKey => cmd_create_api_key(&client, out).await,
                Command::RevokeApiKey { ref key_id } => {
                    cmd_revoke_api_key(&client, out, key_id).await
                }
                Command::Wallet => cmd_wallet(&client, out).await,
                Command::CreateAccessKey => cmd_create_access_key(&client, out).await,
                Command::ListAccessKeys => cmd_list_access_keys(&client, out).await,
                Command::DeleteAccessKey { ref access_key_id } => {
                    cmd_delete_access_key(&client, out, access_key_id).await
                }
                Command::Read { .. } | Command::Info => unreachable!(),
            }
        }
    }
}
