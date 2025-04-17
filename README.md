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
gh-stars list <username>
```

### Search repositories
```bash
# Basic keyword search
gh-stars search <username> "search query"

# Search with language filtering
gh-stars search <username> --language=rust,go "search query"

# Semantic vector search
gh-stars search <username> --semantic "modern web framework with state management"

# Combine language filtering with semantic search
gh-stars search <username> --language=rust --semantic "async runtime"
```

### View repository details
```bash
gh-stars info <username> <number>
```
The `<number>` is the repository number shown in the list/search results.

## Examples
```bash
# Fetch and cache stars for user "octocat"
gh-stars fetch octocat

# Fetch with authentication to avoid rate limits
gh-stars fetch octocat --token your_github_token_here

# List all stars
gh-stars list octocat

# Search for Rust or Go projects
gh-stars search octocat --language=rust,go

# Full text search
gh-stars search octocat "web framework"

# Semantic vector search for projects similar to the concept
gh-stars search octocat --semantic "modern web framework with state management"

# Filter Rust projects and use semantic search
gh-stars search octocat --language=rust --semantic "async runtime"

# View details for the first repository in the list
gh-stars info octocat 1
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
