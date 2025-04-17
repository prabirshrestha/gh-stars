use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use dirs::cache_dir;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use indicatif::{ProgressBar, ProgressStyle}; // Added for spinners
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, LINK, USER_AGENT};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs::{File, create_dir_all};
use std::io::BufReader;
use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Parser)]
#[command(
    name = "gh-stars",
    about = "A CLI tool to fetch, cache, and search GitHub stars",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Fetch and cache stars for a GitHub user
    Fetch {
        /// GitHub username
        username: String,

        /// Force refresh even if cache exists
        #[arg(short, long)]
        force: bool,

        /// GitHub API token (overrides GITHUB_TOKEN env var)
        #[arg(short, long)]
        token: Option<String>,
    },
    /// Search cached stars
    Search {
        /// GitHub username whose stars to search
        username: String,

        /// Programming language(s) to filter by (comma separated)
        #[arg(long, value_parser = parse_languages)]
        language: Option<Vec<String>>,

        /// Search text (searches across name, description, and other fields)
        #[arg(default_value = "")]
        query: String,

        /// Use semantic vector search instead of keyword search
        #[arg(long)]
        semantic: bool,
    },
    /// List all cached stars for a user
    List {
        /// GitHub username
        username: String,
    },
    /// Show detailed information about a specific repository
    Info {
        /// GitHub username
        username: String,

        /// Repository number from list/search results
        number: usize,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct Owner {
    login: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct StarredRepo {
    id: u64,
    name: String,
    full_name: String,
    owner: Owner,
    html_url: String,
    description: Option<String>,
    language: Option<String>,
    stargazers_count: u64,
    forks_count: Option<u64>,
    open_issues_count: Option<u64>,
    #[serde(rename = "updated_at")]
    updated_at: String,
    created_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Cache {
    username: String,
    timestamp: u64,
    repos: Vec<StarredRepo>,
}

fn get_cache_path(username: &str) -> Result<PathBuf> {
    let mut cache_path =
        cache_dir().ok_or_else(|| anyhow!("Failed to determine cache directory"))?;
    cache_path.push("gh-stars");
    create_dir_all(&cache_path)?;
    cache_path.push(format!("{}.json", username));
    Ok(cache_path)
}

fn load_cache(username: &str) -> Result<Option<Cache>> {
    let cache_path = get_cache_path(username)?;

    if !cache_path.exists() {
        return Ok(None);
    }

    let file = File::open(cache_path)?;
    let reader = BufReader::new(file);
    let cache: Cache = serde_json::from_reader(reader).context("Failed to parse cache file")?;

    Ok(Some(cache))
}

fn save_cache(cache: &Cache) -> Result<()> {
    let cache_path = get_cache_path(&cache.username)?;
    let file = File::create(cache_path)?;
    serde_json::to_writer_pretty(file, cache).context("Failed to write cache file")?;

    Ok(())
}

fn has_next_page(headers: &HeaderMap) -> bool {
    headers
        .get(LINK)
        .and_then(|link| link.to_str().ok())
        .map(|link| link.contains("rel=\"next\""))
        .unwrap_or(false)
}

fn get_github_token(cli_token: &Option<String>) -> Option<String> {
    // First check if token was provided via CLI
    if let Some(token) = cli_token {
        return Some(token.clone());
    }

    // Otherwise check environment variable
    env::var("GITHUB_TOKEN").ok()
}

async fn fetch_stars(username: &str, force: bool, token: &Option<String>) -> Result<Cache> {
    // Check cache first unless forced refresh
    if !force {
        if let Some(cache) = load_cache(username)? {
            // Check if cache is fresh (less than 1 day old)
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_secs();
            if now - cache.timestamp < 86400 {
                println!("Using cached data (less than 1 day old)");
                return Ok(cache);
            }
        }
    }

    println!("Fetching stars for GitHub user: {}", username);

    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static("gh-stars-cli"));

    // Add authentication token if available
    if let Some(github_token) = get_github_token(token) {
        let auth_header = format!("token {}", github_token);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth_header).context("Invalid GitHub token format")?,
        );
        println!("Using GitHub token for authentication");
    } else {
        println!("No GitHub token found. Using unauthenticated API (rate limits may apply)");
    }

    let mut all_repos = Vec::new();
    let mut page = 1;
    let per_page = 100; // Max allowed by GitHub API

    // Create spinner for fetch progress
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("{spinner} Fetching GitHub stars: {msg}")
            .unwrap(),
    );
    spinner.enable_steady_tick(std::time::Duration::from_millis(100)); // Make spinner update regularly
    spinner.set_message(format!("Loading page {}", page));

    loop {
        let url = format!(
            "https://api.github.com/users/{}/starred?page={}&per_page={}",
            username, page, per_page
        );

        spinner.set_message(format!(
            "Loading page {} (found {} repos so far)",
            page,
            all_repos.len()
        ));

        // Ensure spinner updates during network calls
        let response = client.get(&url).headers(headers.clone()).send().await?;

        if !response.status().is_success() {
            spinner.finish_with_message(format!("Error on page {}", page));
            return Err(anyhow!(
                "GitHub API error: {} - {}",
                response.status(),
                response.text().await?
            ));
        }

        // Check for pagination before consuming the response body
        let has_more = has_next_page(response.headers());

        // Now parse the JSON response
        let repos: Vec<StarredRepo> = response.json().await?;

        if repos.is_empty() {
            break;
        }

        all_repos.extend(repos);
        spinner.set_message(format!("Found {} repositories so far", all_repos.len()));

        if !has_more {
            break;
        }

        page += 1;
    }

    spinner.finish_with_message(format!("Fetched {} starred repositories", all_repos.len()));

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();

    let cache = Cache {
        username: username.to_string(),
        timestamp: now,
        repos: all_repos,
    };

    // Save to JSON cache
    save_cache(&cache)?;
    println!("Saved cache to {}", get_cache_path(username)?.display());

    // Also save to SQLite with embeddings for vector search
    let db_spinner = ProgressBar::new_spinner();
    db_spinner.set_style(
        ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("{spinner} {msg}")
            .unwrap(),
    );
    db_spinner.enable_steady_tick(std::time::Duration::from_millis(100)); // Keep spinner moving
    db_spinner.set_message("Generating embeddings and storing in SQLite database...");

    store_repos_in_db(username, &cache.repos)?;

    db_spinner.finish_with_message(format!(
        "Database prepared for vector search at {}",
        get_db_path(username)?.display()
    ));

    Ok(cache)
}

// Helper function to parse comma-separated languages
fn parse_languages(s: &str) -> Result<Vec<String>> {
    Ok(s.split(',')
        .map(|lang| lang.trim().to_string())
        .filter(|lang| !lang.is_empty())
        .collect())
}

// Get the path to the SQLite database
fn get_db_path(username: &str) -> Result<PathBuf> {
    let mut db_path = cache_dir().ok_or_else(|| anyhow!("Failed to determine cache directory"))?;
    db_path.push("gh-stars");
    create_dir_all(&db_path)?;
    db_path.push(format!("{}.db", username));
    Ok(db_path)
}

// Check if SQLite database exists and create if needed
fn ensure_db_exists(username: &str) -> Result<()> {
    let db_path = get_db_path(username)?;

    // If DB doesn't exist but cache does, create the DB
    if !db_path.exists() {
        if let Some(cache) = load_cache(username)? {
            println!("Database not found but cache exists. Creating database from cache...");
            let spinner = ProgressBar::new_spinner();
            spinner.set_style(
                ProgressStyle::default_spinner()
                    .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
                    .template("{spinner} {msg}")
                    .unwrap(),
            );
            spinner.enable_steady_tick(std::time::Duration::from_millis(100)); // Keep spinner moving
            spinner.set_message("Generating embeddings and storing in SQLite database...");

            store_repos_in_db(username, &cache.repos)?;

            spinner.finish_with_message("Database created successfully");
            return Ok(());
        }
    }

    Ok(())
}

// Initialize SQLite database with vector extension
fn init_db(username: &str) -> Result<Connection> {
    let db_path = get_db_path(username)?;
    let conn = Connection::open(&db_path)?;

    // Note: To use vector search functionality, you need to:
    // 1. Install sqlite-vec from https://github.com/asg017/sqlite-vec
    // 2. Build rusqlite with the loadable_extension feature
    // 3. Uncomment and modify the code below to load the extension

    // Uncomment when you have sqlite-vec installed and rusqlite with loadable_extension:
    /*
    #[cfg(target_os = "linux")]
    conn.load_extension("libsqlite_vec", None)
        .map_err(|e| anyhow!("Failed to load sqlite-vec extension: {}", e))?;

    #[cfg(target_os = "macos")]
    conn.load_extension("libsqlite_vec", None)
        .map_err(|e| anyhow!("Failed to load sqlite-vec extension: {}", e))?;

    #[cfg(target_os = "windows")]
    conn.load_extension("sqlite_vec", None)
        .map_err(|e| anyhow!("Failed to load sqlite-vec extension: {}", e))?;
    */

    // Create tables if they don't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS repos (
            id INTEGER PRIMARY KEY,
            full_name TEXT NOT NULL,
            name TEXT NOT NULL,
            owner TEXT NOT NULL,
            html_url TEXT NOT NULL,
            description TEXT,
            language TEXT,
            stars INTEGER NOT NULL,
            forks INTEGER,
            open_issues INTEGER,
            updated_at TEXT NOT NULL,
            created_at TEXT,
            json TEXT NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS repo_vectors USING vec(
            id INTEGER,
            embedding BLOB,
        )",
        [],
    )?;

    Ok(conn)
}

// Store repositories in SQLite database with embeddings
fn store_repos_in_db(username: &str, repos: &[StarredRepo]) -> Result<()> {
    // Create a progress bar for the embedding process
    let progress = ProgressBar::new(repos.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    progress.set_message("Processing repositories");

    let mut conn = init_db(username)?;

    // Clear existing data
    conn.execute("DELETE FROM repos", [])?;
    conn.execute("DELETE FROM repo_vectors", [])?;

    // Initialize the embedder using the new API
    let embedder = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(true),
    )
    .map_err(|e| anyhow!("Failed to initialize embedder: {}", e))?;

    // Begin transaction
    let tx = conn.transaction()?;

    for (i, repo) in repos.iter().enumerate() {
        // Update progress bar
        progress.set_position(i as u64);
        if i % 10 == 0 || i == repos.len() - 1 {
            progress.set_message(format!("Processed {}/{} repositories", i + 1, repos.len()));
        }

        // Create text for embedding (combine name and description)
        let embed_text = format!(
            "{} {} {}",
            repo.name,
            repo.language.as_deref().unwrap_or(""),
            repo.description.as_deref().unwrap_or("")
        );

        // Generate embedding with the new API
        let embedding = embedder
            .embed(vec![embed_text], None)
            .map_err(|e| anyhow!("Embedding failed: {}", e))?;

        // Insert repo data
        tx.execute(
            "INSERT INTO repos
            (id, full_name, name, owner, html_url, description, language, stars, forks, open_issues, updated_at, created_at, json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                repo.id,
                repo.full_name,
                repo.name,
                repo.owner.login,
                repo.html_url,
                repo.description,
                repo.language,
                repo.stargazers_count,
                repo.forks_count,
                repo.open_issues_count,
                repo.updated_at,
                repo.created_at,
                serde_json::to_string(repo)?
            ],
        )?;

        // Insert embedding - convert f32 array to bytes
        let embedding_bytes: Vec<u8> = embedding[0]
            .iter()
            .flat_map(|&f| f.to_ne_bytes().to_vec())
            .collect();

        tx.execute(
            "INSERT INTO repo_vectors (id, embedding) VALUES (?, ?)",
            params![repo.id, embedding_bytes],
        )?;
    }

    tx.commit()?;
    Ok(())
}

// Search repositories with traditional keyword search - Fixed with lifetime specifier
fn search_repos<'a>(
    cache: &'a Cache,
    languages: &Option<Vec<String>>,
    query: &str,
) -> Vec<&'a StarredRepo> {
    let query_lower = query.to_lowercase();

    cache
        .repos
        .iter()
        .filter(|repo| {
            // Language filter
            let language_match = match languages {
                Some(langs) if !langs.is_empty() => repo
                    .language
                    .as_ref()
                    .map(|rl| {
                        let rl_lower = rl.to_lowercase();
                        langs.iter().any(|l| rl_lower == l.to_lowercase())
                    })
                    .unwrap_or(false),
                _ => true,
            };

            // Text search (if query is not empty)
            let text_match = if query.is_empty() {
                true
            } else {
                // Search in name
                let name_match = repo.name.to_lowercase().contains(&query_lower);

                // Search in description
                let desc_match = repo
                    .description
                    .as_ref()
                    .map(|d| d.to_lowercase().contains(&query_lower))
                    .unwrap_or(false);

                // Search in full name
                let full_name_match = repo.full_name.to_lowercase().contains(&query_lower);

                name_match || desc_match || full_name_match
            };

            language_match && text_match
        })
        .collect()
}

// Search repositories with semantic vector search
fn search_repos_semantic(
    username: &str,
    languages: &Option<Vec<String>>,
    query: &str,
) -> Result<Vec<StarredRepo>> {
    if query.is_empty() {
        return Ok(Vec::new());
    }

    // Ensure database exists
    ensure_db_exists(username)?;

    let conn = init_db(username)?;

    // Initialize the embedder using the new API
    let embedder = TextEmbedding::try_new(InitOptions::new(EmbeddingModel::AllMiniLML6V2))
        .map_err(|e| anyhow!("Failed to initialize embedder: {}", e))?;

    // Generate embedding for the query with the new API
    let query_embedding = embedder
        .embed(vec![query.to_string()], None)
        .map_err(|e| anyhow!("Embedding query failed: {}", e))?;

    // Prepare language filter if needed
    let language_filter = match languages {
        Some(langs) if !langs.is_empty() => {
            let placeholders: Vec<String> =
                (0..langs.len()).map(|i| format!("?{}", i + 3)).collect();
            format!(" AND language IN ({})", placeholders.join(","))
        }
        _ => String::new(),
    };

    // Build query
    let sql = format!(
        "SELECT r.* FROM repos r
        JOIN (
            SELECT id, vec_cosine_similarity(embedding, ?) AS similarity
            FROM repo_vectors
            ORDER BY similarity DESC
            LIMIT 20
        ) v ON r.id = v.id
        WHERE v.similarity > ?{}
        ORDER BY v.similarity DESC",
        language_filter
    );

    // Prepare statement
    let mut stmt = conn.prepare(&sql)?;

    // Bind parameters - convert f32 embedding to bytes
    let query_embedding_bytes: Vec<u8> = query_embedding[0]
        .iter()
        .flat_map(|&f| f.to_ne_bytes().to_vec())
        .collect();

    stmt.raw_bind_parameter(1, rusqlite::types::Value::Blob(query_embedding_bytes))?;
    stmt.raw_bind_parameter(2, rusqlite::types::Value::Real(0.5))?; // Similarity threshold

    // Bind language parameters if needed
    if let Some(langs) = languages {
        if !langs.is_empty() {
            for (i, lang) in langs.iter().enumerate() {
                stmt.raw_bind_parameter(
                    (i + 3) as usize,
                    rusqlite::types::Value::Text(lang.clone()),
                )?;
            }
        }
    }

    // Execute query and collect results
    let mut results: Vec<StarredRepo> = Vec::new();
    let rows = stmt.query_map([], |row| {
        let json: String = row.get("json")?;
        let repo: StarredRepo = serde_json::from_str(&json).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })?;
        Ok(repo)
    })?;

    for row in rows {
        results.push(row.map_err(|e| anyhow!("Error processing row: {}", e))?);
    }

    Ok(results)
}

fn display_repos(repos: &[&StarredRepo]) {
    if repos.is_empty() {
        println!("No repositories found.");
        return;
    }

    println!("Found {} repositories:", repos.len());
    println!(
        "{:<4} {:<40} {:<15} {:<8}",
        "No.", "Repository", "Language", "Stars"
    );
    println!("{}", "-".repeat(80));

    for (i, repo) in repos.iter().enumerate() {
        println!(
            "{:<4} {:<40} {:<15} {:<8}",
            i + 1,
            repo.full_name,
            repo.language.as_deref().unwrap_or("N/A"),
            repo.stargazers_count
        );
    }

    println!("\nUse 'gh-stars info <username> <number>' to see more details about a repository.");
}

fn display_repo_info(repo: &StarredRepo) {
    println!("Repository: {}", repo.full_name);
    println!("URL: {}", repo.html_url);

    if let Some(desc) = &repo.description {
        println!("Description: {}", desc);
    }

    println!("Owner: {}", repo.owner.login);
    println!("Language: {}", repo.language.as_deref().unwrap_or("N/A"));
    println!("Stars: {}", repo.stargazers_count);

    if let Some(forks) = repo.forks_count {
        println!("Forks: {}", forks);
    }

    if let Some(issues) = repo.open_issues_count {
        println!("Open Issues: {}", issues);
    }

    if let Some(created) = &repo.created_at {
        println!("Created: {}", created);
    }

    println!("Last Updated: {}", repo.updated_at);
}

#[tokio::main]
async fn main() -> Result<()> {
    unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    }

    let cli = Cli::parse();

    match &cli.command {
        Commands::Fetch {
            username,
            force,
            token,
        } => {
            fetch_stars(username, *force, token).await?;
        }
        Commands::Search {
            username,
            language,
            query,
            semantic,
        } => {
            // Ensure database exists for semantic search
            if *semantic {
                ensure_db_exists(username)?;

                println!("Performing semantic vector search for: {}", query);

                // Check if SQLite database exists
                let db_path = get_db_path(username)?;
                if !db_path.exists() {
                    return Err(anyhow!(
                        "Vector database not found for user: {}. Run 'fetch' first.",
                        username
                    ));
                }

                let results = search_repos_semantic(username, language, query)?;

                if results.is_empty() {
                    println!("No repositories found matching your query.");
                } else {
                    println!("Found {} repositories with semantic search:", results.len());
                    println!(
                        "{:<4} {:<40} {:<15} {:<8}",
                        "No.", "Repository", "Language", "Stars"
                    );
                    println!("{}", "-".repeat(80));

                    for (i, repo) in results.iter().enumerate() {
                        println!(
                            "{:<4} {:<40} {:<15} {:<8}",
                            i + 1,
                            repo.full_name,
                            repo.language.as_deref().unwrap_or("N/A"),
                            repo.stargazers_count
                        );
                    }
                }
            } else {
                // Traditional keyword search
                let cache = load_cache(username)?.ok_or_else(|| {
                    anyhow!("No cache found for user: {}. Run 'fetch' first.", username)
                })?;

                let results = search_repos(&cache, language, query);
                display_repos(&results);
            }
        }
        Commands::List { username } => {
            let cache = load_cache(username)?.ok_or_else(|| {
                anyhow!("No cache found for user: {}. Run 'fetch' first.", username)
            })?;

            let all_repos: Vec<&StarredRepo> = cache.repos.iter().collect();
            display_repos(&all_repos);
        }
        Commands::Info { username, number } => {
            let cache = load_cache(username)?.ok_or_else(|| {
                anyhow!("No cache found for user: {}. Run 'fetch' first.", username)
            })?;

            if *number == 0 || *number > cache.repos.len() {
                return Err(anyhow!(
                    "Invalid repository number. Must be between 1 and {}.",
                    cache.repos.len()
                ));
            }

            let repo = &cache.repos[*number - 1];
            display_repo_info(repo);
        }
    }

    Ok(())
}
