use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};
use dirs::cache_dir;
use fastembed::{EmbeddingModel, InitOptions, TextEmbedding};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue, LINK, USER_AGENT};
use rusqlite::{Connection, ffi::sqlite3_auto_extension, params};
use serde::{Deserialize, Serialize};
use sqlite_vec::sqlite3_vec_init;
use std::fs::create_dir_all;
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
        /// GitHub username(s) whose stars to search (comma separated)
        #[arg(short, long, value_parser = parse_usernames)]
        username: Option<Vec<String>>,

        /// Programming language(s) to filter by (comma separated)
        #[arg(long, value_parser = parse_languages)]
        language: Option<Vec<String>>,

        /// Search terms (searches across name, description, and other fields)
        #[arg(trailing_var_arg = true)]
        terms: Vec<String>,

        /// Maximum number of results to return
        #[arg(short, long, default_value = "30")]
        limit: usize,
    },
    /// List all cached stars for a user
    List {
        /// GitHub username(s) whose stars to list (comma separated)
        #[arg(short, long, value_parser = parse_usernames)]
        username: Option<Vec<String>>,

        /// Maximum number of results to return
        #[arg(short, long, default_value = "30")]
        limit: usize,
    },
    /// Show detailed information about a specific repository
    Info {
        /// Repository in format user/repo
        repo: String,
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

// Get the cache directory path for the application
fn get_cache_dir() -> Result<PathBuf> {
    let mut path = cache_dir().ok_or_else(|| anyhow!("Failed to determine cache directory"))?;
    path.push("gh-stars");
    create_dir_all(&path)?;
    Ok(path)
}

// Get the path to the SQLite database (one DB for all users)
fn get_db_path() -> Result<PathBuf> {
    let mut db_path = get_cache_dir()?;
    db_path.push("stars.db");
    Ok(db_path)
}

// Initialize SQLite database with vector extension
fn init_db() -> Result<Connection> {
    let db_path = get_db_path()?;
    let conn = Connection::open(&db_path)?;

    // Create tables if they don't exist
    conn.execute(
        "CREATE TABLE IF NOT EXISTS users (
            username TEXT PRIMARY KEY,
            last_updated INTEGER NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS repos (
            id INTEGER,
            username TEXT NOT NULL,
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
            json TEXT NOT NULL,
            PRIMARY KEY (id, username),
            FOREIGN KEY (username) REFERENCES users(username)
        )",
        [],
    )?;

    // Updated to use vec0 virtual table
    conn.execute(
        "CREATE VIRTUAL TABLE IF NOT EXISTS repo_vectors USING vec0(
            embedding float[384]
        )",
        [],
    )?;

    Ok(conn)
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

    // Otherwise try to get token from gh_token crate
    gh_token::get().ok()
}

async fn fetch_stars(
    username: &str,
    force: bool,
    token: &Option<String>,
) -> Result<Vec<StarredRepo>> {
    // Open database connection
    let conn = init_db()?;

    // Check if we need to refresh the data
    if !force {
        let refresh_needed = match conn.query_row(
            "SELECT last_updated FROM users WHERE username = ?",
            params![username],
            |row| {
                let last_updated: i64 = row.get(0)?;
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs() as i64;
                Ok(now - last_updated > 86400) // Refresh if older than 1 day
            },
        ) {
            Ok(need_refresh) => need_refresh,
            Err(rusqlite::Error::QueryReturnedNoRows) => true, // No data, need to fetch
            Err(e) => return Err(e.into()),
        };

        if !refresh_needed {
            println!("Using cached data (less than 1 day old)");

            // Fetch cached repos from database
            let mut stmt = conn.prepare("SELECT json FROM repos WHERE username = ?")?;

            let repos_iter = stmt.query_map(params![username], |row| {
                let json: String = row.get(0)?;
                let repo: StarredRepo = serde_json::from_str(&json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(repo)
            })?;

            let mut repos = Vec::new();
            for repo in repos_iter {
                repos.push(repo?);
            }

            return Ok(repos);
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

    // Save to database
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_secs();

    // Update the database
    let db_spinner = ProgressBar::new_spinner();
    db_spinner.set_style(
        ProgressStyle::default_spinner()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏")
            .template("{spinner} {msg}")
            .unwrap(),
    );
    db_spinner.enable_steady_tick(std::time::Duration::from_millis(100));
    db_spinner.set_message("Storing repos and generating embeddings in database...");

    store_repos_in_db(username, &all_repos, now as i64)?;

    db_spinner.finish_with_message(format!("Database updated for user {}", username));

    Ok(all_repos)
}

// Helper function to parse comma-separated languages
fn parse_languages(s: &str) -> Result<Vec<String>> {
    Ok(s.split(',')
        .map(|lang| lang.trim().to_string())
        .filter(|lang| !lang.is_empty())
        .collect())
}

// Helper function to parse comma-separated usernames
fn parse_usernames(s: &str) -> Result<Vec<String>> {
    Ok(s.split(',')
        .map(|username| username.trim().to_string())
        .filter(|username| !username.is_empty())
        .collect())
}

// Store repositories and their embeddings in the database
fn store_repos_in_db(username: &str, repos: &[StarredRepo], timestamp: i64) -> Result<()> {
    // Create a progress bar for the embedding process
    let progress = ProgressBar::new(repos.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("##-"),
    );
    progress.set_message("Processing repositories");

    let mut conn = init_db()?;

    // Begin transaction
    let tx = conn.transaction()?;

    // Update or insert user
    tx.execute(
        "INSERT OR REPLACE INTO users (username, last_updated) VALUES (?, ?)",
        params![username, timestamp],
    )?;

    // Get all repo IDs for this user before deleting repos
    let repo_ids: Vec<i64> = {
        let mut stmt = tx.prepare("SELECT id FROM repos WHERE username = ?")?;
        stmt.query_map(params![username], |row| row.get(0))?
            .collect::<Result<Vec<i64>, _>>()?
    };

    // Clear existing data for this user
    tx.execute("DELETE FROM repos WHERE username = ?", params![username])?;

    // Clear existing vectors for this user's repos
    if !repo_ids.is_empty() {
        // For each ID, delete the corresponding vector
        for id in repo_ids {
            tx.execute("DELETE FROM repo_vectors WHERE rowid = ?", params![id])?;
        }
    }

    // Initialize the embedder
    let embedder = TextEmbedding::try_new(
        InitOptions::new(EmbeddingModel::AllMiniLML6V2).with_show_download_progress(true),
    )
    .map_err(|e| anyhow!("Failed to initialize embedder: {}", e))?;

    for (i, repo) in repos.iter().enumerate() {
        // Update progress bar
        progress.set_position(i as u64);
        if i % 10 == 0 || i == repos.len() - 1 {
            progress.set_message(format!("Processed {}/{} repositories", i + 1, repos.len()));
        }

        // Insert repo data
        tx.execute(
            "INSERT INTO repos
            (id, username, full_name, name, owner, html_url, description, language, stars, forks, open_issues, updated_at, created_at, json)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            params![
                repo.id,
                username,
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

        // Create text for embedding (combine name and description)
        let embed_text = format!(
            "{} {} {}",
            repo.name,
            repo.language.as_deref().unwrap_or(""),
            repo.description.as_deref().unwrap_or("")
        );

        // Generate embedding
        let embedding = embedder
            .embed(vec![embed_text], None)
            .map_err(|e| anyhow!("Embedding failed: {}", e))?;

        // Convert f32 vector to bytes for SQLite (safe version)
        let embedding_bytes: Vec<u8> = embedding[0].iter().flat_map(|&f| f.to_le_bytes()).collect();

        // Insert embedding
        tx.execute(
            "INSERT INTO repo_vectors(rowid, embedding) VALUES (?, ?)",
            params![repo.id, embedding_bytes],
        )?;
    }

    tx.commit()?;
    progress.finish_with_message("Repositories stored in database");

    Ok(())
}

// Combined search function that uses both semantic and keyword search
fn search_repos(
    username: &str,
    languages: &Option<Vec<String>>,
    query: &str,
    limit: usize,
) -> Result<Vec<StarredRepo>> {
    let conn = init_db()?;

    // If query is empty, just list repos with language filter
    if query.is_empty() {
        let mut sql = "SELECT json FROM repos WHERE username = ?".to_string();
        let mut params: Vec<&dyn rusqlite::ToSql> = vec![&username as &dyn rusqlite::ToSql];

        // Add language filter if needed
        if let Some(langs) = languages {
            if !langs.is_empty() {
                let placeholders: Vec<String> = (0..langs.len()).map(|_| "?".to_string()).collect();
                sql.push_str(&format!(" AND language IN ({})", placeholders.join(",")));

                for lang in langs {
                    params.push(lang as &dyn rusqlite::ToSql);
                }
            }
        }

        sql.push_str(&format!(" ORDER BY stars DESC LIMIT {}", limit));

        let mut stmt = conn.prepare(&sql)?;

        let repos_iter = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            let json: String = row.get(0)?;
            let repo: StarredRepo = serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok(repo)
        })?;

        let mut repos = Vec::new();
        for repo in repos_iter {
            repos.push(repo?);
        }

        return Ok(repos);
    }

    // Prepare language filter if needed
    let language_filter = match languages {
        Some(langs) if !langs.is_empty() => {
            let placeholders: Vec<String> = (0..langs.len()).map(|_| "?".to_string()).collect();
            format!(" AND r.language IN ({})", placeholders.join(","))
        }
        _ => String::new(),
    };

    // Format query for LIKE operations
    let query_lower = format!("%{}%", query.to_lowercase());

    // 1. Keyword search
    let keyword_sql = format!(
        "SELECT r.*, 1 AS search_type,
        (CASE
            WHEN LOWER(r.name) LIKE ? THEN 3
            WHEN LOWER(r.full_name) LIKE ? THEN 2
            WHEN LOWER(r.description) LIKE ? THEN 1
            ELSE 0
        END) AS score
        FROM repos r
        WHERE r.username = ?{}
        AND (LOWER(r.name) LIKE ? OR LOWER(r.full_name) LIKE ? OR LOWER(r.description) LIKE ?)
        ORDER BY score DESC, r.stars DESC
        LIMIT {}",
        language_filter, limit
    );

    // Build parameters for query using vec macro
    let mut keyword_params: Vec<&dyn rusqlite::ToSql> = vec![
        // Add the LIKE parameters
        &query_lower as &dyn rusqlite::ToSql,
        &query_lower as &dyn rusqlite::ToSql,
        &query_lower as &dyn rusqlite::ToSql,
        // Add username
        &username as &dyn rusqlite::ToSql,
    ];

    // Add language parameters
    if let Some(langs) = languages {
        for lang in langs {
            keyword_params.push(lang as &dyn rusqlite::ToSql);
        }
    }

    // Add the trailing LIKE params for the OR conditions
    keyword_params.push(&query_lower as &dyn rusqlite::ToSql);
    keyword_params.push(&query_lower as &dyn rusqlite::ToSql);
    keyword_params.push(&query_lower as &dyn rusqlite::ToSql);

    // Execute keyword search
    let mut keyword_stmt = conn.prepare(&keyword_sql)?;

    let mut results = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();

    // Add keyword search results
    let keyword_rows =
        keyword_stmt.query_map(rusqlite::params_from_iter(keyword_params.iter()), |row| {
            let json: String = row.get("json")?;
            let score: i32 = row.get("score")?;
            let repo: StarredRepo = serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;
            Ok((repo, 0, score)) // 0 = keyword search
        })?;

    for row_result in keyword_rows {
        let (repo, search_type, score) = row_result?;
        if !seen_ids.contains(&repo.id) {
            seen_ids.insert(repo.id);
            results.push((repo, search_type, score));
        }
    }

    // 2. Vector search if query isn't too short
    if query.len() >= 3 {
        // Initialize the embedder with the same cache dir as the database
        let cache_dir = get_cache_dir()?;

        let embedder = TextEmbedding::try_new(
            InitOptions::new(EmbeddingModel::AllMiniLML6V2)
                .with_show_download_progress(true)
                .with_cache_dir(cache_dir),
        )
        .map_err(|e| anyhow!("Failed to initialize embedder: {}", e))?;

        // Generate embedding for the query
        let query_embedding = embedder
            .embed(vec![query.to_string()], None)
            .map_err(|e| anyhow!("Embedding query failed: {}", e))?;

        // Convert f32 vector to bytes for SQLite (safe version)
        let query_embedding_bytes: Vec<u8> = query_embedding[0]
            .iter()
            .flat_map(|&f| f.to_le_bytes())
            .collect();

        // Build the vector search query
        let vector_sql = format!(
            "SELECT r.*, 2 AS search_type, v.distance AS score
            FROM repos r
            JOIN (
                SELECT rowid, distance
                FROM repo_vectors
                WHERE embedding MATCH ?
                ORDER BY distance
                LIMIT {}
            ) v ON r.id = v.rowid
            WHERE r.username = ?{}
            ORDER BY v.distance ASC",
            limit, language_filter
        );

        // Build vector search parameters without cloning
        let mut vector_params: Vec<&dyn rusqlite::ToSql> = Vec::new();

        // Add embedding parameter
        vector_params.push(&query_embedding_bytes as &dyn rusqlite::ToSql);

        // Add username
        vector_params.push(&username as &dyn rusqlite::ToSql);

        // Add language parameters
        if let Some(langs) = languages {
            for lang in langs {
                vector_params.push(lang as &dyn rusqlite::ToSql);
            }
        }

        // Execute vector search
        let mut vector_stmt = conn.prepare(&vector_sql)?;

        let vector_rows =
            vector_stmt.query_map(rusqlite::params_from_iter(vector_params.iter()), |row| {
                let json: String = row.get("json")?;
                let score: f64 = row.get("score")?;
                let repo: StarredRepo = serde_json::from_str(&json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                // Convert distance score to an integer for ranking
                let int_score = ((1.0 - score) * 100.0) as i32;
                Ok((repo, 1, int_score)) // 1 = vector search
            })?;

        // Add vector search results
        for row_result in vector_rows {
            let (repo, search_type, score) = row_result?;
            if !seen_ids.contains(&repo.id) {
                seen_ids.insert(repo.id);
                results.push((repo, search_type, score));
            }
        }
    }

    // Sort by score descending
    results.sort_by(|a, b| {
        // First compare by search type (keyword first, then vector)
        let type_compare = a.1.cmp(&b.1);
        if type_compare != std::cmp::Ordering::Equal {
            return type_compare;
        }

        // Then by score
        let score_compare = b.2.cmp(&a.2);
        if score_compare != std::cmp::Ordering::Equal {
            return score_compare;
        }

        // Finally by stars
        b.0.stargazers_count.cmp(&a.0.stargazers_count)
    });

    // Limit results to the requested amount
    let results = results
        .into_iter()
        .take(limit)
        .map(|(repo, _, _)| repo)
        .collect();

    Ok(results)
}

fn display_repos(repos: &[StarredRepo]) {
    if repos.is_empty() {
        println!("No repositories found.");
        return;
    }

    println!("Found {} repositories:", repos.len());
    println!(
        "{:<4} {:<60} {:<15} {:<8}",
        "No.", "Repository", "Language", "Stars"
    );
    println!("{}", "-".repeat(100));

    for (i, repo) in repos.iter().enumerate() {
        println!(
            "{:<4} {:<60} {:<15} {:<8}",
            i + 1,
            repo.full_name,
            repo.language.as_deref().unwrap_or("N/A"),
            repo.stargazers_count
        );
    }

    println!("\nUse 'gh-stars info user/repo' to see more details about a repository.");
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
        sqlite3_auto_extension(Some(std::mem::transmute::<
            *const (),
            unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32,
        >(sqlite3_vec_init as *const ())));
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
            terms,
            limit,
        } => {
            // Join all search terms into a single query string, or use empty string if no terms provided
            let query = if terms.is_empty() {
                String::new()
            } else {
                terms.join(" ")
            };
            let usernames = match username {
                Some(users) => users.clone(),
                None => {
                    // If no username is provided, get all cached users
                    let conn = init_db()?;
                    let mut stmt = conn.prepare("SELECT username FROM users")?;
                    let users_iter = stmt.query_map([], |row| row.get::<_, String>(0))?;
                    let mut users = Vec::new();
                    for user in users_iter {
                        users.push(user?);
                    }

                    if users.is_empty() {
                        return Err(anyhow!(
                            "No cached users found. Fetch stars for a user first using the fetch command."
                        ));
                    }
                    users
                }
            };

            println!(
                "Searching repositories for user(s): {} (limit: {})",
                usernames.join(", "),
                limit
            );

            let mut all_results = Vec::new();
            for username in &usernames {
                let results = search_repos(username, language, &query, *limit)?;
                all_results.extend(results);
            }

            // Sort by stars and limit to the requested number
            all_results.sort_by(|a, b| b.stargazers_count.cmp(&a.stargazers_count));
            let limited_results = all_results.into_iter().take(*limit).collect::<Vec<_>>();

            display_repos(&limited_results);
        }
        Commands::List { username, limit } => {
            let usernames = match username {
                Some(users) => users.clone(),
                None => {
                    // If no username is provided, get all cached users
                    let conn = init_db()?;
                    let mut stmt = conn.prepare("SELECT username FROM users")?;
                    let users_iter = stmt.query_map([], |row| row.get::<_, String>(0))?;
                    let mut users = Vec::new();
                    for user in users_iter {
                        users.push(user?);
                    }

                    if users.is_empty() {
                        return Err(anyhow!(
                            "No cached users found. Fetch stars for a user first using the fetch command."
                        ));
                    }
                    users
                }
            };

            println!(
                "Listing repositories for user(s): {} (limit: {})",
                usernames.join(", "),
                limit
            );

            let mut all_results = Vec::new();
            for username in &usernames {
                // Use the search function with empty query to list repos
                let results = search_repos(username, &None, "", *limit)?;
                all_results.extend(results);
            }

            // Sort by stars and limit to the requested number
            all_results.sort_by(|a, b| b.stargazers_count.cmp(&a.stargazers_count));
            let limited_results = all_results.into_iter().take(*limit).collect::<Vec<_>>();

            display_repos(&limited_results);
        }
        Commands::Info { repo } => {
            // Parse the repo string in format "user/repo"
            let parts: Vec<&str> = repo.split('/').collect();
            if parts.len() != 2 {
                return Err(anyhow!(
                    "Invalid repository format. Expected format: user/repo"
                ));
            }

            let username = parts[0];
            let repo_name = parts[1];

            // Open database connection
            let conn = init_db()?;

            // Try to find the repository by full_name first (this is what's displayed in the list)
            let query = "SELECT json FROM repos WHERE full_name = ?";
            match conn.query_row(query, params![repo], |row| {
                let json: String = row.get(0)?;
                let repo: StarredRepo = serde_json::from_str(&json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;
                Ok(repo)
            }) {
                Ok(repo) => {
                    display_repo_info(&repo);
                }
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    // If not found by full_name, try with username and name
                    let fallback_query = "SELECT json FROM repos WHERE username = ? AND name = ?";
                    match conn.query_row(fallback_query, params![username, repo_name], |row| {
                        let json: String = row.get(0)?;
                        let repo: StarredRepo = serde_json::from_str(&json).map_err(|e| {
                            rusqlite::Error::FromSqlConversionFailure(
                                0,
                                rusqlite::types::Type::Text,
                                Box::new(e),
                            )
                        })?;
                        Ok(repo)
                    }) {
                        Ok(repo) => {
                            display_repo_info(&repo);
                        }
                        Err(rusqlite::Error::QueryReturnedNoRows) => {
                            return Err(anyhow!("Repository {} not found in cache", repo));
                        }
                        Err(e) => {
                            return Err(anyhow!("Database error: {}", e));
                        }
                    }
                }
                Err(e) => {
                    return Err(anyhow!("Database error: {}", e));
                }
            }
        }
    }

    Ok(())
}
