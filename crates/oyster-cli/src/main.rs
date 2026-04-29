#![allow(missing_docs)]

mod client;
mod config;
mod output;

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use client::{AccountSummary, ApiError, ApiKeyMetadata, OysterClient};
use config::{AppEntry, ConfigError};
use output::Output;

#[derive(Parser)]
#[command(name = "oyster", about = "Oyster object storage CLI")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    context: Option<String>,
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
        /// Tag to attach (repeat for multiple), `key=value` form
        #[arg(long = "tag", value_parser = parse_tag_kv)]
        tags: Vec<(String, String)>,
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
    /// Show wallet information
    Wallet,
    /// Show resolved configuration
    Info,
    /// App admin-key management
    App {
        #[command(subcommand)]
        command: AppCommand,
    },
    /// Blob tag management
    Tags {
        #[command(subcommand)]
        command: TagCommand,
    },
}

#[derive(Subcommand)]
enum TagCommand {
    /// List tags on a blob
    List {
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Object key
        #[arg(long)]
        key: String,
    },
    /// Upsert a single tag (key=value)
    Set {
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Object key
        #[arg(long)]
        key: String,
        /// Tag as key=value
        #[arg(value_parser = parse_tag_kv)]
        tag: (String, String),
    },
    /// Remove a single tag by key
    Rm {
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Object key
        #[arg(long)]
        key: String,
        /// Tag key to remove
        tag_key: String,
    },
    /// Remove all tags from a blob
    Clear {
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Object key
        #[arg(long)]
        key: String,
    },
    /// Replace the entire tag set (full replace)
    Replace {
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Object key
        #[arg(long)]
        key: String,
        /// Tag as key=value (repeat)
        #[arg(long = "tag", value_parser = parse_tag_kv)]
        tags: Vec<(String, String)>,
    },
    /// Merge (upsert) a partial tag set into the existing tags
    Merge {
        /// Bucket name
        #[arg(long)]
        bucket: String,
        /// Object key
        #[arg(long)]
        key: String,
        /// Tag as key=value (repeat)
        #[arg(long = "tag", value_parser = parse_tag_kv)]
        tags: Vec<(String, String)>,
    },
}

fn parse_tag_kv(s: &str) -> Result<(String, String), String> {
    let (k, v) = s
        .split_once('=')
        .ok_or_else(|| format!("tag '{s}' must be in key=value form"))?;
    if k.is_empty() {
        return Err("tag key must be non-empty".into());
    }
    Ok((k.to_string(), v.to_string()))
}

#[derive(Subcommand)]
enum AppCommand {
    /// Import an admin key for an app (read interactively, hidden input when tty).
    Import {
        /// Name of the app to store the admin key under in the active context.
        app_name: String,
    },
    /// Account enumeration + active-key rotation.
    Account {
        /// App name in the active context (required when the context has
        /// more than one app).
        #[arg(long, global = true)]
        app: Option<String>,
        #[command(subcommand)]
        command: AccountCommand,
    },
}

#[derive(Subcommand)]
enum AccountCommand {
    /// List accounts owned by the selected app.
    List,
    /// Create a new account on the selected app.
    Create {
        /// Optional human-readable account name.
        #[arg(long)]
        name: Option<String>,
        /// Optional note attached to the initial api_key.
        #[arg(long)]
        note: Option<String>,
        /// Save the new account's bearer to context.api_key.
        #[arg(long)]
        activate: bool,
    },
    /// Mint a fresh api_key for an account and save it to context.api_key.
    Use {
        /// Account ID or name.
        id_or_name: String,
        /// On 409, revoke this key id and retry mint (non-interactive).
        #[arg(long)]
        revoke: Option<String>,
        /// On 409, revoke the oldest active key and retry mint.
        #[arg(long, conflicts_with = "revoke")]
        revoke_oldest: bool,
    },
    /// Interactive picker over accounts, then mint and activate a new api_key.
    Select,
    /// List api_keys for an account.
    Keys {
        /// Account ID or name.
        id_or_name: String,
    },
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
    #[error("{0}")]
    Message(String),
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
    tags: &[(String, String)],
) -> Result<(), CliError> {
    let data = std::fs::read(file)?;
    let ct = content_type.unwrap_or_else(|| guess_content_type(file));
    let resp = client.store_blob(bucket, key, data, ct, tags).await?;
    out.print(&resp, |r| {
        println!("Stored blob:");
        println!("  key:            {}", r.key);
        println!("  blob_id:        {}", r.blob_id);
        println!("  size:           {} bytes", r.size);
        println!("  md5:            {}", r.md5);
        if let Some(ref id) = r.pooled_blob_object_id {
            println!("  pooled_blob_object_id:  {id}");
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

async fn cmd_wallet(client: &OysterClient, out: &Output) -> Result<(), CliError> {
    let resp = client.get_wallet().await?;
    out.print(&resp, |r| {
        println!("address: {}", r.address);
    });
    Ok(())
}

/// Import an admin key for an app, reading interactively (no echo when stdin
/// is a tty). Persists to the active context via atomic write.
fn cmd_app_import(
    cli_config: Option<&std::path::Path>,
    cli_context: Option<&str>,
    app_name: &str,
) -> Result<(), CliError> {
    let (mut file, config_path) = config::load_file_config(cli_config)?;
    let ctx_name = config::require_context_name(cli_context, &file)?;
    let path = config_path.ok_or(ConfigError::NoConfigFile)?;

    let prompt = format!("Admin key for {app_name}: ");
    let admin_key = if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        rpassword::prompt_password(&prompt)?
    } else {
        use std::io::BufRead;
        let stdin = std::io::stdin();
        let mut line = String::new();
        stdin.lock().read_line(&mut line)?;
        line
    };
    let admin_key = admin_key.trim().to_string();
    if admin_key.is_empty() {
        return Err(CliError::Message("empty admin-key input".to_string()));
    }

    let ctx = file.contexts.entry(ctx_name.clone()).or_default();
    ctx.apps
        .insert(app_name.to_string(), AppEntry { admin_key });
    config::save_config(&path, &file)?;
    eprintln!("imported {app_name} into context {ctx_name}");
    Ok(())
}

async fn cmd_account(
    out: &Output,
    cli_config: Option<&std::path::Path>,
    cli_context: Option<&str>,
    cli_url: Option<&str>,
    app: Option<&str>,
    command: &AccountCommand,
) -> Result<(), CliError> {
    let resolved = config::resolve_admin(cli_config, cli_context, cli_url, app)?;
    let client = OysterClient::with_admin_key(resolved.url.clone(), resolved.admin_key.clone());
    match command {
        AccountCommand::List => cmd_account_list(&client, out).await,
        AccountCommand::Create {
            name,
            note,
            activate,
        } => {
            cmd_account_create(
                &client,
                out,
                resolved,
                name.as_deref(),
                note.as_deref(),
                *activate,
            )
            .await
        }
        AccountCommand::Use {
            id_or_name,
            revoke,
            revoke_oldest,
        } => {
            cmd_account_use(
                &client,
                out,
                resolved,
                id_or_name,
                revoke.as_deref(),
                *revoke_oldest,
            )
            .await
        }
        AccountCommand::Select => cmd_account_select(&client, out, resolved).await,
        AccountCommand::Keys { id_or_name } => cmd_account_keys(&client, out, id_or_name).await,
    }
}

async fn cmd_account_list(client: &OysterClient, out: &Output) -> Result<(), CliError> {
    let accounts = client.list_accounts().await?;
    out.print(&accounts, |rows| {
        println!(
            "{:<38} {:<24} {:<26} ACTIVE_KEYS",
            "ID", "NAME", "CREATED_AT"
        );
        for a in rows {
            println!(
                "{:<38} {:<24} {:<26} {}",
                a.id, a.name, a.created_at, a.active_api_key_count
            );
        }
    });
    Ok(())
}

async fn cmd_account_keys(
    client: &OysterClient,
    out: &Output,
    id_or_name: &str,
) -> Result<(), CliError> {
    let account_id = resolve_account_id(client, id_or_name).await?;
    let keys = client.list_api_keys(&account_id).await?;
    out.print(&keys, |rows| {
        println!(
            "{:<38} {:<10} {:<24} {:<26} REVOKED_AT",
            "ID", "PREFIX", "NOTE", "CREATED_AT"
        );
        for k in rows {
            println!(
                "{:<38} {:<10} {:<24} {:<26} {}",
                k.id,
                k.prefix,
                k.note,
                k.created_at,
                k.revoked_at.as_deref().unwrap_or("-")
            );
        }
    });
    Ok(())
}

async fn cmd_account_create(
    client: &OysterClient,
    out: &Output,
    resolved: config::AdminResolved,
    name: Option<&str>,
    note: Option<&str>,
    activate: bool,
) -> Result<(), CliError> {
    let resp = client.create_account(name, note).await?;
    if activate {
        let mut file = resolved.file;
        let ctx = file.contexts.entry(resolved.ctx_name.clone()).or_default();
        ctx.api_key = Some(resp.api_key.bearer_token.clone());
        config::save_config(&resolved.config_path, &file)?;
    }
    if out.json {
        // In JSON mode, emit the full server response (includes bearer).
        println!(
            "{}",
            serde_json::to_string_pretty(&resp).expect("serialize CreateAccountResponse")
        );
    } else if activate {
        println!("created account:");
        println!("  account_id:     {}", resp.account_id);
        println!("  api_key.id:     {}", resp.api_key.id);
        println!("  api_key.prefix: {}", resp.api_key.prefix);
        println!("  created_at:     {}", resp.api_key.created_at);
        println!(
            "saved to context {} (app {})",
            resolved.ctx_name, resolved.app_name
        );
    } else {
        println!("created account:");
        println!("  account_id:     {}", resp.account_id);
        println!("  api_key.id:     {}", resp.api_key.id);
        println!("  api_key.prefix: {}", resp.api_key.prefix);
        println!("  created_at:     {}", resp.api_key.created_at);
        println!("  bearer_token:   {}", resp.api_key.bearer_token);
        println!();
        println!("store this bearer safely — it will not be shown again");
    }
    Ok(())
}

async fn cmd_account_use(
    client: &OysterClient,
    out: &Output,
    resolved: config::AdminResolved,
    id_or_name: &str,
    revoke: Option<&str>,
    revoke_oldest: bool,
) -> Result<(), CliError> {
    let account_id = resolve_account_id(client, id_or_name).await?;
    let note = format!("oyster-cli: activate {id_or_name}");

    let mut consumed_revoke = false;
    for _attempt in 0..2 {
        match client.mint_api_key(&account_id, Some(&note)).await {
            Ok(key) => {
                let mut file = resolved.file;
                let ctx = file.contexts.entry(resolved.ctx_name.clone()).or_default();
                ctx.api_key = Some(key.bearer_token.clone());
                config::save_config(&resolved.config_path, &file)?;
                if out.json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&key).expect("serialize ApiKey")
                    );
                } else {
                    println!("rotated active api_key:");
                    println!("  account_id:     {account_id}");
                    println!("  api_key.id:     {}", key.id);
                    println!("  api_key.prefix: {}", key.prefix);
                    println!("  created_at:     {}", key.created_at);
                    println!(
                        "saved to context {} (app {})",
                        resolved.ctx_name, resolved.app_name
                    );
                }
                return Ok(());
            }
            Err(ApiError::Server { status: 409, .. }) => {
                let to_revoke = pick_revoke_target(
                    client,
                    out,
                    &account_id,
                    if consumed_revoke { None } else { revoke },
                    revoke_oldest,
                )
                .await?;
                consumed_revoke = true;
                client.revoke_api_key(&account_id, &to_revoke).await?;
                continue;
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(CliError::Message(
        "two consecutive 409 responses while minting api_key — concurrent modification, retry"
            .to_string(),
    ))
}

async fn cmd_account_select(
    client: &OysterClient,
    out: &Output,
    resolved: config::AdminResolved,
) -> Result<(), CliError> {
    if !is_interactive(out) {
        return Err(CliError::Message(
            "`account select` requires an interactive TTY (not --json, stdin and stdout must be \
             ttys); use `account list` + `account use <id>` non-interactively"
                .to_string(),
        ));
    }
    let accounts = client.list_accounts().await?;
    if accounts.is_empty() {
        return Err(CliError::Message(
            "no accounts in this app; run `app account create` first".to_string(),
        ));
    }
    let choices: Vec<AccountChoice> = accounts.iter().map(AccountChoice).collect();
    let chosen = inquire::Select::new("Pick an account to activate:", choices)
        .prompt()
        .map_err(|e| CliError::Message(format!("inquire: {e}")))?;
    let id = chosen.0.id.clone();
    cmd_account_use(client, out, resolved, &id, None, false).await
}

async fn resolve_account_id(client: &OysterClient, id_or_name: &str) -> Result<String, CliError> {
    let accounts = client.list_accounts().await?;
    if let Some(a) = accounts.iter().find(|a| a.id == id_or_name) {
        return Ok(a.id.clone());
    }
    let by_name: Vec<&AccountSummary> = accounts.iter().filter(|a| a.name == id_or_name).collect();
    match by_name.len() {
        0 => Err(CliError::Message(format!(
            "no account matches '{id_or_name}' (no exact id or name match)"
        ))),
        1 => Ok(by_name[0].id.clone()),
        _ => Err(CliError::Message(format!(
            "account name '{id_or_name}' is ambiguous ({} matches); use the account id instead",
            by_name.len()
        ))),
    }
}

async fn pick_revoke_target(
    client: &OysterClient,
    out: &Output,
    account_id: &str,
    revoke: Option<&str>,
    revoke_oldest: bool,
) -> Result<String, CliError> {
    if let Some(id) = revoke {
        return Ok(id.to_string());
    }
    if revoke_oldest {
        let keys = client.list_api_keys(account_id).await?;
        let oldest = keys
            .iter()
            .filter(|k| k.revoked_at.is_none())
            .min_by(|a, b| a.created_at.cmp(&b.created_at))
            .ok_or_else(|| {
                CliError::Message(
                    "server reported 409 but no active api_keys were returned by list".to_string(),
                )
            })?;
        return Ok(oldest.id.clone());
    }
    if !is_interactive(out) {
        return Err(CliError::Message(
            "api_key cap reached (HTTP 409); pass --revoke <id> or --revoke-oldest, or run \
             interactively for a TUI prompt"
                .to_string(),
        ));
    }
    let keys = client.list_api_keys(account_id).await?;
    let active: Vec<&ApiKeyMetadata> = keys.iter().filter(|k| k.revoked_at.is_none()).collect();
    if active.is_empty() {
        return Err(CliError::Message(
            "server reported 409 but no active api_keys were returned by list".to_string(),
        ));
    }
    let choices: Vec<ApiKeyChoice> = active.iter().map(|k| ApiKeyChoice(k)).collect();
    let chosen = inquire::Select::new("Revoke an api_key to make room:", choices)
        .prompt()
        .map_err(|e| CliError::Message(format!("inquire: {e}")))?;
    Ok(chosen.0.id.clone())
}

fn is_interactive(out: &Output) -> bool {
    use std::io::IsTerminal;
    !out.json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

struct ApiKeyChoice<'a>(&'a ApiKeyMetadata);

impl std::fmt::Display for ApiKeyChoice<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} · {} · {}",
            self.0.prefix, self.0.note, self.0.created_at
        )
    }
}

struct AccountChoice<'a>(&'a AccountSummary);

impl std::fmt::Display for AccountChoice<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({}) — {} keys",
            self.0.name, self.0.id, self.0.active_api_key_count
        )
    }
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
            let (url, _) = config::resolve_url_only(
                cli.config.as_deref(),
                cli.context.as_deref(),
                cli.url.as_deref(),
            )?;
            let client = OysterClient::new(url, None);
            cmd_read(&client, bucket, key, output.as_deref()).await
        }

        Command::Info => {
            // Info is special: best-effort resolve, never errors on missing fields
            let (file_config, config_path) =
                config::load_file_config(cli.config.as_deref()).unwrap_or_default();
            let env_ctx = std::env::var("OYSTER_CONTEXT").ok();
            let ctx_name = config::resolve_context_name(
                cli.context.as_deref(),
                env_ctx.as_deref(),
                &file_config,
            )
            .ok()
            .flatten();
            let ctx = ctx_name.as_ref().and_then(|n| file_config.contexts.get(n));
            let url = cli
                .url
                .as_deref()
                .or_else(|| ctx.and_then(|c| c.url.as_deref()))
                .unwrap_or("(not set)");
            let api_key = cli
                .api_key
                .as_deref()
                .or_else(|| ctx.and_then(|c| c.api_key.as_deref()));
            cmd_info(out, config_path.as_deref(), url, api_key);
            Ok(())
        }

        Command::App { ref command } => match command {
            AppCommand::Import { app_name } => {
                cmd_app_import(cli.config.as_deref(), cli.context.as_deref(), app_name)
            }
            AppCommand::Account { app, command } => {
                cmd_account(
                    out,
                    cli.config.as_deref(),
                    cli.context.as_deref(),
                    cli.url.as_deref(),
                    app.as_deref(),
                    command,
                )
                .await
            }
        },

        // Authenticated commands
        command => {
            let resolved = config::resolve(
                cli.config.as_deref(),
                cli.context.as_deref(),
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
                    ref tags,
                } => {
                    let default_key = file
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "unnamed".to_string());
                    let key = key.as_deref().unwrap_or(&default_key);
                    cmd_store(
                        &client,
                        out,
                        file,
                        bucket,
                        key,
                        content_type.as_deref(),
                        tags,
                    )
                    .await
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
                Command::Wallet => cmd_wallet(&client, out).await,
                Command::Tags { ref command } => cmd_tags(&client, out, command).await,
                Command::Read { .. } | Command::Info | Command::App { .. } => unreachable!(),
            }
        }
    }
}

async fn cmd_tags(client: &OysterClient, out: &Output, cmd: &TagCommand) -> Result<(), CliError> {
    use std::collections::BTreeMap;

    match cmd {
        TagCommand::List { bucket, key } => {
            let resp = client.list_blob_tags(bucket, key).await?;
            out.print(&resp.tags, |tags| {
                if tags.is_empty() {
                    println!("(no tags)");
                } else {
                    for (k, v) in tags {
                        println!("{k}={v}");
                    }
                }
            });
        }
        TagCommand::Set { bucket, key, tag } => {
            client.put_blob_tag(bucket, key, &tag.0, &tag.1).await?;
            out.success(&format!("Set tag {}={} on {bucket}/{key}", tag.0, tag.1));
        }
        TagCommand::Rm {
            bucket,
            key,
            tag_key,
        } => {
            client.delete_blob_tag(bucket, key, tag_key).await?;
            out.success(&format!("Removed tag {tag_key} from {bucket}/{key}"));
        }
        TagCommand::Clear { bucket, key } => {
            client.clear_blob_tags(bucket, key).await?;
            out.success(&format!("Cleared tags on {bucket}/{key}"));
        }
        TagCommand::Replace { bucket, key, tags } => {
            let map: BTreeMap<String, String> = tags.iter().cloned().collect();
            client.set_blob_tags_full(bucket, key, &map).await?;
            out.success(&format!(
                "Replaced tags on {bucket}/{key} ({} tags)",
                map.len()
            ));
        }
        TagCommand::Merge { bucket, key, tags } => {
            let map: BTreeMap<String, String> = tags.iter().cloned().collect();
            client.patch_blob_tags(bucket, key, &map).await?;
            out.success(&format!("Merged {} tag(s) into {bucket}/{key}", map.len()));
        }
    }
    Ok(())
}
