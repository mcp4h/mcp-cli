# mcp-cli

Rust-based MCP server for running CLI scripts with strict command safety controls.

## Build

```bash
cargo build
```

## Run

```bash
cargo run
```

## Setup

On startup the server ensures a `rules.txt` file exists in `.mcp-cli`.

## Configuration

Configuration sources are merged in this order (SEP-1596):

1. Environment/CLI defaults
2. `--config` / `MCP_CONFIG` JSON
3. `initialize` params via `capabilities.experimental.configuration`

Supported CLI flags:

- `--root <path>`: default root (default: CWD)
- `--allow-root <path>`: additional root (repeatable)
- `--allow-escape`: allow paths outside configured roots
- `--dynamic-scopes`: allow execution when scopes are only known at runtime
- `--rules <path>`: rules file path (default: `.mcp-cli/rules.txt`)
- `--config <path>`: config JSON file
- `--print-config-schema`: print configuration schema and exit

Configuration fields:

- `rules`: inline rules in the same DSL format as the rules file
- `dynamic_scopes`: allow execution when scopes are only known at runtime

Example:

```json
{
  "rules": [
    "execute:cli:safe:gh -> gh repo [view,list]",
    "disable execute:cli:safe:gh -> gh repo [view,list]"
  ]
}
```

Environment variables:

- `MCP_ROOT`: same as `--root`
- `MCP_ALLOWED_ROOTS`: comma-separated list for `--allow-root`
- `MCP_ALLOW_ESCAPE`: `1|true|yes`
- `MCP_DYNAMIC_SCOPES`: `1|true|yes`
- `MCP_RULES`: same as `--rules`
- `MCP_RULES_INLINE`: inline rules in the same DSL format as the rules file
- `MCP_CONFIG`: same as `--config`

## Rules file

Rules live in `.mcp-cli/rules.txt`. The file is additive: when the bundled defaults expand, new rules are appended without removing or rewriting user edits.
You can also supply inline rules through configuration (`rules`) or `MCP_RULES_INLINE`.
Rule precedence is first match wins, ordered as: inline config rules, then rules in the file, then appended defaults.

Scope decisions use `_meta.allowed_scopes` and `_meta.denied_scopes` on each tool call.
If a scope is missing or denied, the call is rejected (unless `dynamic_scopes` is true), and the response includes `_meta.requested_scopes`.
Denied scopes are compounded: denying `execute:cli:safe` denies any `execute:cli:safe:*` scope.

To disable a default rule, prefix it with `disable`:

```
disable execute:cli:safe:gh -> gh repo [view,list]
```

### Rule format

```
<scope> -> <pattern>
```

- `scope` is typically `execute:cli:safe:<tool>` or `execute:cli:unsafe:<tool>`.
- `pattern` starts with the tool name and may include extra tokens.
- Token matching is order-insensitive by default.
- Lists: `[a,b]` means allow-list, `[!a,!b]` means deny-list.
- Wildcard: `*` means any token.
- Subcommand inheritance: use `execute:cli:inherit` with `{subcommand}` to inherit scope from a wrapped command.

Examples:

```
execute:cli:safe:gh -> gh repo [view,list]
execute:cli:unsafe:gh -> gh repo [create,delete]
execute:cli:inherit -> xargs {subcommand}
execute:cli:unsafe:xargs -> xargs
```

Token order matters when you include explicit lists. For example, given `gh repo [view,list]`:

```
gh repo view file.txt   # allowed
gh repo file.txt view   # denied
```

## Default rules

Safe defaults (read-only commands):

- `execute:cli:safe:ls` -> list directory contents
- `execute:cli:safe:pwd` -> print working directory
- `execute:cli:safe:whoami` -> show current user
- `execute:cli:safe:id` -> show user/group ids
- `execute:cli:safe:uname` -> system info
- `execute:cli:safe:date` -> current date/time
- `execute:cli:safe:time` -> timing wrapper
- `execute:cli:safe:env` -> environment listing
- `execute:cli:safe:printenv` -> environment listing
- `execute:cli:safe:which` -> locate binaries
- `execute:cli:safe:type` -> shell type lookup
- `execute:cli:safe:command` -> shell command lookup
- `execute:cli:safe:test` -> test expression
- `execute:cli:safe:stat` -> file metadata
- `execute:cli:safe:file` -> file type detection
- `execute:cli:safe:head` -> read file head
- `execute:cli:safe:tail` -> read file tail
- `execute:cli:safe:less` -> pager
- `execute:cli:safe:more` -> pager
- `execute:cli:safe:du` -> disk usage
- `execute:cli:safe:df` -> filesystem usage
- `execute:cli:safe:lsblk` -> list block devices
- `execute:cli:safe:mount` -> list mounts
- `execute:cli:safe:ps` -> process listing
- `execute:cli:safe:top` -> process monitor
- `execute:cli:safe:wc` -> count lines/words
- `execute:cli:safe:cut` -> select columns
- `execute:cli:safe:sort` -> sort text
- `execute:cli:safe:uniq` -> unique filtering
- `execute:cli:safe:tr` -> translate characters
- `execute:cli:safe:grep` -> search text
- `execute:cli:safe:find` -> search files (read-only flags)
- `execute:cli:safe:sed` -> stream edit (no in-place)
- `execute:cli:safe:awk` -> text processing
- `execute:cli:safe:rg` -> ripgrep search
- `execute:cli:safe:jar` -> list jar contents
- `execute:cli:safe:tar` -> list tar contents
- `execute:cli:safe:unzip` -> list zip contents
- `execute:cli:safe:fd` -> file finder
- `execute:cli:safe:cat` -> read files
- `execute:cli:safe:basename` -> path basename
- `execute:cli:safe:dirname` -> path dirname
- `execute:cli:safe:realpath` -> resolve absolute path
- `execute:cli:safe:readlink` -> resolve symlink
- `execute:cli:safe:sha256sum` -> hash
- `execute:cli:safe:shasum` -> hash
- `execute:cli:safe:md5sum` -> hash
- `execute:cli:safe:cksum` -> checksum
- `execute:cli:safe:diff` -> compare files
- `execute:cli:safe:cmp` -> compare files
- `execute:cli:safe:comm` -> compare sorted files
- `execute:cli:safe:nl` -> number lines
- `execute:cli:safe:fmt` -> format text
- `execute:cli:safe:column` -> format columns
- `execute:cli:safe:od` -> octal dump
- `execute:cli:safe:hexdump` -> hex dump
- `execute:cli:safe:strings` -> extract strings
- `execute:cli:safe:jq` -> JSON processing
- `execute:cli:safe:yq` -> YAML processing
- `execute:cli:safe:cd` -> change directory
- `execute:cli:safe:git` -> read-only git commands
- `execute:cli:safe:gh` -> read-only GitHub CLI commands
- `execute:cli:safe:docker` -> read-only Docker queries
- `execute:cli:safe:kubectl` -> read-only Kubernetes queries
- `execute:cli:safe:terraform` -> read-only Terraform queries
- `execute:cli:safe:helm` -> read-only Helm queries
- `execute:cli:safe:npm` -> read-only npm queries
- `execute:cli:safe:yarn` -> read-only Yarn queries
- `execute:cli:safe:pnpm` -> read-only pnpm queries
- `execute:cli:safe:pip` -> read-only pip queries

Unsafe defaults (explicit scope required):

- `execute:cli:unsafe:xargs` -> command wrapper
- `execute:cli:unsafe:rm` -> remove files/directories
- `execute:cli:unsafe:mv` -> move/rename
- `execute:cli:unsafe:cp` -> copy
- `execute:cli:unsafe:mkdir` -> create directories
- `execute:cli:unsafe:rmdir` -> remove directories
- `execute:cli:unsafe:touch` -> update timestamps/create
- `execute:cli:unsafe:chmod` -> change permissions
- `execute:cli:unsafe:chown` -> change ownership
- `execute:cli:unsafe:ln` -> link files
- `execute:cli:unsafe:sudo` -> privilege escalation
- `execute:cli:unsafe:bash` -> shell execution
- `execute:cli:unsafe:sh` -> shell execution
- `execute:cli:unsafe:zsh` -> shell execution
- `execute:cli:unsafe:fish` -> shell execution
- `execute:cli:unsafe:python` -> scripting
- `execute:cli:unsafe:python3` -> scripting
- `execute:cli:unsafe:perl` -> scripting
- `execute:cli:unsafe:ruby` -> scripting
- `execute:cli:unsafe:node` -> scripting
- `execute:cli:unsafe:npm` -> package manager
- `execute:cli:unsafe:yarn` -> package manager
- `execute:cli:unsafe:pnpm` -> package manager
- `execute:cli:unsafe:cargo` -> build tool
- `execute:cli:unsafe:go` -> build tool
- `execute:cli:unsafe:dotnet` -> build tool
- `execute:cli:unsafe:mvn` -> build tool
- `execute:cli:unsafe:mvnw` -> build tool
- `execute:cli:unsafe:gradle` -> build tool
- `execute:cli:unsafe:gradlew` -> build tool
- `execute:cli:unsafe:git` -> VCS
- `execute:cli:unsafe:gh` -> GitHub CLI
- `execute:cli:unsafe:docker` -> container management
- `execute:cli:unsafe:kubectl` -> cluster management
- `execute:cli:unsafe:terraform` -> infrastructure management
- `execute:cli:unsafe:helm` -> Kubernetes package manager
- `execute:cli:unsafe:aws` -> cloud CLI
- `execute:cli:unsafe:gcloud` -> cloud CLI
- `execute:cli:unsafe:az` -> cloud CLI
- `execute:cli:unsafe:psql` -> database client
- `execute:cli:unsafe:mysql` -> database client
- `execute:cli:unsafe:sqlite3` -> database client
- `execute:cli:unsafe:curl` -> network access
- `execute:cli:unsafe:wget` -> network access
- `execute:cli:unsafe:ssh` -> network access
- `execute:cli:unsafe:scp` -> network access
- `execute:cli:unsafe:rsync` -> network access
- `execute:cli:unsafe:tar` -> archive handling
- `execute:cli:unsafe:zip` -> archive handling
- `execute:cli:unsafe:unzip` -> archive handling
- `execute:cli:unsafe:make` -> build tool
