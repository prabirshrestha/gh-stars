# GitHub Stars CLI
A command-line tool to fetch, cache, and search GitHub stars for any user, with powerful semantic search capabilities.

## Features
- Fetch and cache starred repositories for any GitHub user
- Automatically handles pagination to get all stars
- Cache results locally for quick access
- Traditional keyword search across repository fields
- Advanced semantic vector search using embeddings
- Multi-language filtering support
- View detailed information about specific repositories
- GitHub API authentication support to avoid rate limits
- Search across multiple users' starred repositories

## Installation
1. Make sure you have Rust and Cargo installed
2. Clone this repository
3. Build and install:
```bash
cargo install --path .
```

### Vector Search Requirements
For semantic vector search functionality, you need:
1. Install the sqlite-vec extension (https://github.com/asg017/sqlite-vec)
2. Make sure rusqlite is compiled with the `loadable_extension` feature:
   ```toml
   # In Cargo.toml
   rusqlite = { version = "0.34", features = ["bundled", "loadable_extension"] }
   ```
3. Uncomment the extension loading code in the `init_db` function

## Authentication
The tool supports GitHub API authentication to avoid rate limits:

1. **Environment Variable**: Set the `GITHUB_TOKEN` environment variable with your GitHub personal access token:
   ```bash
   export GITHUB_TOKEN=your_github_token_here
   ```

2. **Command Line**: Provide your token directly via the command line:
   ```bash
   gh-stars fetch <username> --token your_github_token_here
   ```

The command line option takes precedence over the environment variable.

### Creating a GitHub Token
1. Go to your GitHub Settings > Developer settings > Personal access tokens
2. Create a new token with the `public_repo` scope (or `repo` for private repositories)
3. Copy the generated token and use it with gh-stars

## Usage
### Fetch stars for a GitHub user
```bash
# First-time fetch
gh-stars fetch <username>

# Force refresh of existing cache
gh-stars fetch <username> --force

# Fetch using a GitHub token
gh-stars fetch <username> --token your_github_token_here
```

### List all starred repositories
```bash
# List for specific user(s)
gh-stars list --username=<username>

# List for multiple users
gh-stars list --username=user1,user2,user3

# List all cached users' stars (if no username specified)
gh-stars list

# Limit results
gh-stars list --username=<username> --limit 100
```

### Search repositories
```bash
# Basic keyword search for specific user(s)
gh-stars search --username=<username> search query

# Search across multiple users
gh-stars search --username=user1,user2,user3 search query

# Search across all cached users (if no username specified)
gh-stars search search query

# Search with language filtering
gh-stars search --username=<username> --language=rust,go search query

# Limit search results
gh-stars search --username=<username> --limit 100 search query

# Multi-word search terms don't need quotes anymore
gh-stars search chat gpt
```

### View repository details
```bash
gh-stars info user/repo
```
Use the format `user/repo` such as `octocat/Hello-World`.

## Examples
```bash
# Fetch and cache stars for user "octocat"
gh-stars fetch octocat

# Fetch with authentication to avoid rate limits
gh-stars fetch octocat --token your_github_token_here

# List all stars for octocat
gh-stars list --username=octocat

# List stars for multiple users
gh-stars list --username=octocat,rust-lang

# List stars from all cached users
gh-stars list

# Search across all octocat's stars
gh-stars search --username=octocat web framework

# Search across multiple users' stars
gh-stars search --username=octocat,rust-lang web framework

# Search all cached users' stars
gh-stars search web framework

# Search for Rust or Go projects
gh-stars search --username=octocat --language=rust,go

# View details for a specific repository
gh-stars info octocat/Hello-World
```

## Cache Location
Stars are cached in your system's cache directory:
- **Linux**: `~/.cache/gh-stars/`
- **macOS**: `~/Library/Caches/gh-stars/`
- **Windows**: `C:\Users\<username>\AppData\Local\Cache\gh-stars\`

Two cache formats are used:
1. JSON file: `<username>.json` - For regular keyword search
2. SQLite database: `<username>.db` - For semantic vector search with embeddings

## How It Works
The tool uses:
- GitHub's REST API to fetch starred repositories
- FastEmbed for generating embeddings of repository metadata
- SQLite with the sqlite-vec extension for vector similarity search
- Rusqlite for database operations
- Clap for command-line argument parsing

### Search Types
1. **Keyword Search**: Performs traditional text matching on repository names, descriptions, and other metadata.
2. **Semantic Search**: Uses text embeddings to find repositories that are conceptually similar to your query, even if they don't contain the exact keywords.

## Rate Limits
Without authentication, GitHub API limits requests to 60 per hour per IP address. With authentication, this increases to 5,000 requests per hour. For users with many starred repositories, authentication is recommended to avoid hitting rate limits.

## Troubleshooting
If you encounter issues with vector search:
- Run with `--force` to regenerate the database if needed

If you encounter GitHub API rate limits:
- Use authentication via the `--token` flag or `GITHUB_TOKEN` environment variable
- Wait until the rate limit resets (usually one hour)
