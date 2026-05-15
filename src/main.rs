use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use shell_words::split;
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tree_sitter::{Language, Parser, TreeCursor};

#[derive(Debug, Deserialize)]
struct Request {
    id: Value,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Serialize)]
struct Response {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorBody>,
    #[serde(skip_serializing_if = "Option::is_none")]
    _meta: Option<Value>,
}

#[derive(Serialize)]
struct ErrorBody {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    _meta: Option<Value>,
}

impl Response {
    fn ok(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
            _meta: None,
        }
    }

    fn ok_with_meta(id: Value, result: Value, meta: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
            _meta: Some(meta),
        }
    }

    fn err(id: Value, code: i64, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
                _meta: None,
            }),
            _meta: None,
        }
    }

    fn err_with_meta(id: Value, code: i64, message: impl Into<String>, meta: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(ErrorBody {
                code,
                message: message.into(),
                _meta: Some(meta),
            }),
            _meta: None,
        }
    }
}

#[derive(Clone)]
struct RootConfig {
    path: PathBuf,
    default: bool,
}

#[derive(Clone)]
struct Config {
    roots: Vec<RootConfig>,
    default_root: PathBuf,
    allow_escape: bool,
    dynamic_scopes: bool,
    rules_path: PathBuf,
    rules: Vec<String>,
}

#[derive(Debug, Clone)]
struct Rule {
    scope: String,
    pattern: Pattern,
    #[allow(dead_code)]
    raw: String,
}

#[derive(Debug, Clone)]
struct Pattern {
    tool: String,
    tokens: Vec<TokenMatcher>,
    has_subcommand: bool,
}

#[derive(Debug, Clone)]
enum TokenMatcher {
    Exact(String),
    Allow(Vec<String>),
    Deny(Vec<String>),
    Wildcard,
    Subcommand,
}

#[derive(Debug, Clone)]
struct CommandTokens {
    raw: String,
    tool: Option<String>,
    tokens: Vec<String>,
    has_unsafe_nodes: bool,
}

#[allow(dead_code)]
#[derive(Debug)]
struct CommandIntent {
    raw: String,
    tool: String,
    cwd: Option<String>,
    scope: String,
    status: String,
    reason: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut config = load_config()?;
    ensure_rules_file(&config.rules_path)?;
    let mut rules = load_rules_with_inline(&config.rules_path, &config.rules)?;
    let language = load_bash_language();

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = BufReader::new(stdin).lines();
    let mut writer = io::BufWriter::new(stdout);

    while let Some(line) = reader.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(&line) {
            Ok(req) => req,
            Err(err) => {
                let resp = Response::err(Value::Null, -32700, err.to_string());
                write_response(&mut writer, resp).await?;
                continue;
            }
        };
        if req.method == "initialize" {
            if let Err(err) = apply_initialize_config(&mut config, &req) {
                let resp = Response::err(req.id.clone(), -32602, err.to_string());
                write_response(&mut writer, resp).await?;
                continue;
            }
            ensure_rules_file(&config.rules_path)?;
            rules = load_rules_with_inline(&config.rules_path, &config.rules)?;
        }
        let resp = handle_request(&config, &rules, &language, req).await;
        write_response(&mut writer, resp).await?;
    }

    Ok(())
}

async fn write_response(writer: &mut io::BufWriter<io::Stdout>, resp: Response) -> Result<()> {
    let payload = serde_json::to_string(&resp)?;
    writer.write_all(payload.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;
    Ok(())
}

async fn handle_request(
    config: &Config,
    rules: &[Rule],
    language: &Language,
    req: Request,
) -> Response {
    match req.method.as_str() {
        "initialize" => Response::ok(
            req.id,
            json!({
                "serverInfo": {
                    "name": "mcp-cli",
                    "version": "0.1.0"
                },
                "configSchema": config_schema(),
                "capabilities": {
                    "tools": {
                        "list": true,
                        "call": true
                    },
                    "experimental": {
                        "policy": true
                    },
                    "_meta": {
                        "server": "mcp-cli",
                        "vendor": "celerex"
                    }
                }
            }),
        ),
        "tools/list" => {
            let tool = tool_definition();
            Response::ok(req.id, json!({ "tools": [tool] }))
        }
        "tools/call" => {
            let name = req
                .params
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("");
            if name != "run_script" {
                return Response::err(req.id, -32602, "unknown tool");
            }
            let args = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let meta = req
                .params
                .get("_meta")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match run_script(config, rules, language, &args, &meta).await {
                Ok(outcome) => {
                    if let Some(meta) = outcome.meta {
                        Response::ok_with_meta(req.id, outcome.value, meta)
                    } else {
                        Response::ok(req.id, outcome.value)
                    }
                }
                Err(err) => {
                    if let Some(scopes) = err.downcast_ref::<RequestedScopesError>() {
                        Response::err_with_meta(
                            req.id,
                            -32000,
                            err.to_string(),
            json!({ "requested_scopes": scopes.scopes }),
                        )
                    } else {
                        Response::err(req.id, -32000, err.to_string())
                    }
                }
            }
        }
        _ => Response::err(req.id, -32601, "method not found"),
    }
}

struct ToolOutcome {
    value: Value,
    meta: Option<Value>,
}


#[derive(Debug)]
struct RequestedScopesError {
    scopes: Vec<String>,
}

impl std::fmt::Display for RequestedScopesError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "missing required scopes")
    }
}

impl std::error::Error for RequestedScopesError {}

fn tool_definition() -> Value {
    json!({
        "name": "run_script",
        "description": "Run a bash script with command safety checks",
        "annotations": {
            "dynamic_scopes": ["execute:cli:safe:*", "execute:cli:unsafe:*"],
            "group": "cli",
            "intentTemplate": "Run script [in {cwd}]",
            "inputTemplate": "```bash\n{script}\n```",
            "outputTemplate": "[Stdout:\n```ansi\n{stdout}\n```\n][Stderr:\n```ansi\n{stderr}\n```]"
        },
        "inputSchema": {
            "type": "object",
            "properties": {
                "script": { "type": "string", "description": "Bash script to run. Multi-line supported." },
                "cwd": { "type": "string", "description": "Working directory for the script." },
                "timeout_ms": { "type": "integer", "minimum": 0, "description": "Execution timeout in milliseconds." }
            },
            "required": ["script"],
            "additionalProperties": false
        },
        "outputSchema": {
            "type": "object",
            "properties": {
                "exitCode": { "type": ["integer", "null"] },
                "stdout": { "type": "string" },
                "stderr": { "type": "string" }
            },
            "required": ["stdout", "stderr"]
        }
    })
}

async fn run_script(
    config: &Config,
    rules: &[Rule],
    language: &Language,
    args: &Value,
    meta: &Value,
) -> Result<ToolOutcome> {
    let script = args
        .get("script")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("script is required"))?;
    let cwd = resolve_cwd(args.get("cwd").and_then(Value::as_str), config)?;
    let timeout_ms = args.get("timeout_ms").and_then(Value::as_u64);

    let dynamic_scopes = meta
        .get("dynamic_scopes")
        .or_else(|| meta.get("dynamicScopes"))
        .and_then(Value::as_bool)
        .unwrap_or(config.dynamic_scopes);
    let allowed_scopes = parse_scope_list(meta.get("allowed_scopes").or_else(|| meta.get("allowedScopes")));
    let denied_scopes = parse_scope_list(meta.get("denied_scopes").or_else(|| meta.get("deniedScopes")));

    let mut parser = Parser::new();
    parser.set_language(language)?;
    let tree = parser
        .parse(script, None)
        .ok_or_else(|| anyhow!("failed to parse script"))?;
    let mut spans = Vec::new();
    collect_command_spans(tree.root_node(), &mut spans);
    spans.sort_by_key(|span| span.start);
    let mut seen = HashSet::new();
    spans.retain(|span| seen.insert((span.start, span.end)));

    let mut cwd_state = CwdState::new(cwd.clone());
    let mut intents = Vec::new();
    let mut requested_scopes = HashSet::new();
    let mut has_blocked_command = false;

    for span in spans {
        let raw = script
            .get(span.start..span.end)
            .unwrap_or("")
            .trim()
            .to_string();
        if raw.is_empty() {
            continue;
        }
        let tokens = extract_command_tokens(&tree, script, span.start, span.end);
        if tokens.tool.as_deref() == Some("cd") {
            if !tokens.has_unsafe_nodes {
                update_cwd_from_cd(&mut cwd_state, &tokens, &config.default_root);
            } else {
                cwd_state.mark_unknown();
            }
        }
        let (intent, missing_scopes, blocked) = classify_command(
            &tokens,
            &cwd_state,
            config,
            rules,
            &allowed_scopes,
            &denied_scopes,
        );
        for scope in missing_scopes {
            requested_scopes.insert(scope);
        }
        if blocked {
            has_blocked_command = true;
        }
        intents.push(intent);
    }

    let requested_scopes: Vec<String> = requested_scopes.into_iter().collect();
    if has_blocked_command {
        return Err(RequestedScopesError {
            scopes: requested_scopes,
        }
        .into());
    }
    if !requested_scopes.is_empty() && !dynamic_scopes {
        return Err(RequestedScopesError {
            scopes: requested_scopes,
        }
        .into());
    }

    let mut cmd = Command::new("bash");
    cmd.arg("-lc").arg(script).current_dir(&cwd);
    if let Some(timeout_ms) = timeout_ms {
        cmd.kill_on_drop(true);
        let output = tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            cmd.output(),
        )
        .await
        .map_err(|_| anyhow!("command timeout"))??;
        return Ok(build_tool_outcome(
            intents,
            requested_scopes,
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).to_string(),
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let output = cmd.output().await?;
    Ok(build_tool_outcome(
        intents,
        requested_scopes,
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn build_tool_outcome(
    intents: Vec<CommandIntent>,
    requested_scopes: Vec<String>,
    exit_code: Option<i32>,
    stdout: String,
    stderr: String,
) -> ToolOutcome {
    let safe_count = intents
        .iter()
        .filter(|intent| intent.scope.contains("execute:cli:safe:"))
        .count();
    let unsafe_count = intents.len().saturating_sub(safe_count);
    let summary = format!(
        "Ran {} commands ({} safe, {} unsafe). Exit code {}.",
        intents.len(),
        safe_count,
        unsafe_count,
        exit_code.map(|code| code.to_string()).unwrap_or_else(|| "unknown".to_string())
    );
    let content_text = if stdout.is_empty() {
        stderr.clone()
    } else if stderr.is_empty() {
        stdout.clone()
    } else {
        format!("{}\n--\n{}", stdout, stderr)
    };
    let structured = json!({
        "exitCode": exit_code,
        "stdout": stdout,
        "stderr": stderr
    });
    let meta = if requested_scopes.is_empty() {
        json!({ "displayMessage": summary })
    } else {
        json!({
            "requested_scopes": requested_scopes,
            "displayMessage": summary
        })
    };
    ToolOutcome {
        value: json!({
            "structuredContent": structured,
            "content": [{
                "type": "text",
                "text": content_text
            }],
            "_meta": meta
        }),
        meta: None,
    }
}

fn classify_command(
    tokens: &CommandTokens,
    cwd_state: &CwdState,
    config: &Config,
    rules: &[Rule],
    allowed_scopes: &HashSet<String>,
    denied_scopes: &HashSet<String>,
) -> (CommandIntent, Vec<String>, bool) {
    let raw = tokens.raw.clone();
    let tool = tokens
        .tool
        .clone()
        .unwrap_or_else(|| "unknown".to_string());
    let cwd = cwd_state.current_cwd_string();
    let mut missing_scopes = Vec::new();
    let blocked;

    if tokens.has_unsafe_nodes {
        let scope = format!("execute:cli:unsafe:{}", tool);
        let scope = append_scope_suffix(&scope, cwd_state, config);
        let allowed = is_scope_allowed(&scope, allowed_scopes, denied_scopes);
        let denied = is_scope_denied(&scope, denied_scopes);
        if !allowed && !denied {
            missing_scopes.push(scope.clone());
        }
        let intent = CommandIntent {
            raw,
            tool,
            cwd,
            scope,
            status: if denied || !allowed {
                "blocked".to_string()
            } else {
                "unsafe".to_string()
            },
            reason: Some(if denied {
                "scope denied".to_string()
            } else if !allowed {
                "missing required scope".to_string()
            } else {
                "dynamic tokens in command".to_string()
            }),
        };
        return (intent, missing_scopes, denied || !allowed);
    }

    let matched = match_rule(tokens, rules);
    let (scope, reason) = match matched {
        Some(MatchedRule::Direct { scope }) => (scope, None),
        Some(MatchedRule::Inherited { tokens: sub_tokens }) => {
            let sub_tool = sub_tokens.first().cloned().unwrap_or_else(|| "unknown".to_string());
            let sub_command = CommandTokens {
                raw: sub_tokens.join(" "),
                tool: Some(sub_tool.clone()),
                tokens: sub_tokens.clone(),
                has_unsafe_nodes: false,
            };
            let sub_match = match_rule(&sub_command, rules);
            match sub_match {
                Some(MatchedRule::Direct { scope }) => (scope, None),
                _ => (
                    format!("execute:cli:unsafe:{}", sub_tool),
                    Some("subcommand not whitelisted".to_string()),
                ),
            }
        }
        None => (
            format!("execute:cli:unsafe:{}", tool),
            Some("command not in whitelist".to_string()),
        ),
    };

    let scope = append_scope_suffix(&scope, cwd_state, config);
    let denied = is_scope_denied(&scope, denied_scopes);
    let allowed = is_scope_allowed(&scope, allowed_scopes, denied_scopes);
    blocked = denied || !allowed;
    if !denied && !allowed {
        missing_scopes.push(scope.clone());
    }

    let reason = if blocked {
        if denied {
            Some("scope denied".to_string())
        } else if !allowed {
            Some("missing required scope".to_string())
        } else {
            reason
        }
    } else {
        reason
    };
    let intent = CommandIntent {
        raw,
        tool,
        cwd,
        scope,
        status: if blocked { "blocked".to_string() } else { "allowed".to_string() },
        reason,
    };
    (intent, missing_scopes, blocked)
}

fn append_scope_suffix(scope: &str, cwd_state: &CwdState, config: &Config) -> String {
    if let Some(cwd) = cwd_state.current_cwd() {
        if is_within_roots(cwd, config) || config.allow_escape {
            scope.to_string()
        } else {
            format!("{}:{}", scope, cwd.to_string_lossy())
        }
    } else {
        scope.to_string()
    }
}

enum MatchedRule {
    Direct { scope: String },
    Inherited { tokens: Vec<String> },
}

fn match_rule(tokens: &CommandTokens, rules: &[Rule]) -> Option<MatchedRule> {
    let tool = tokens.tool.as_ref()?;
    let mut best: Option<(usize, MatchedRule)> = None;
    for rule in rules {
        if rule.pattern.tool != *tool {
            continue;
        }
        let match_result = if rule.pattern.has_subcommand {
            match_rule_sequence(tokens, rule)
        } else {
            match_rule_set(tokens, rule)
        };
        if let Some(matched) = match_result {
            let score = rule.pattern.tokens.len();
            if best.as_ref().map(|(best_score, _)| score > *best_score).unwrap_or(true) {
                best = Some((score, matched));
            }
        }
    }
    best.map(|(_, rule)| rule)
}

fn match_rule_set(tokens: &CommandTokens, rule: &Rule) -> Option<MatchedRule> {
    let command_tokens = &tokens.tokens;
    for matcher in &rule.pattern.tokens {
        match matcher {
            TokenMatcher::Exact(value) => {
                if !command_tokens.contains(value) {
                    return None;
                }
            }
            TokenMatcher::Allow(values) => {
                if !values.iter().any(|value| command_tokens.contains(value)) {
                    return None;
                }
            }
            TokenMatcher::Deny(values) => {
                if values.iter().any(|value| command_tokens.contains(value)) {
                    return None;
                }
            }
            TokenMatcher::Wildcard => {}
            TokenMatcher::Subcommand => {
                return None;
            }
        }
    }
    if rule.scope == "execute:cli:inherit" {
        return None;
    }
    Some(MatchedRule::Direct {
        scope: rule.scope.clone(),
    })
}

fn match_rule_sequence(tokens: &CommandTokens, rule: &Rule) -> Option<MatchedRule> {
    let mut index = 0usize;
    let command_tokens = &tokens.tokens;
    let mut subcommand_tokens: Option<Vec<String>> = None;
    for matcher in &rule.pattern.tokens {
        match matcher {
            TokenMatcher::Exact(value) => {
                if let Some(pos) = command_tokens[index..].iter().position(|t| t == value) {
                    index += pos + 1;
                } else {
                    return None;
                }
            }
            TokenMatcher::Allow(values) => {
                if let Some(pos) = command_tokens[index..]
                    .iter()
                    .position(|t| values.contains(t))
                {
                    index += pos + 1;
                } else {
                    return None;
                }
            }
            TokenMatcher::Deny(values) => {
                if command_tokens.iter().any(|t| values.contains(t)) {
                    return None;
                }
            }
            TokenMatcher::Wildcard => {}
            TokenMatcher::Subcommand => {
                if index >= command_tokens.len() {
                    return None;
                }
                subcommand_tokens = Some(command_tokens[index..].to_vec());
                break;
            }
        }
    }
    if rule.scope == "execute:cli:inherit" {
        return subcommand_tokens.map(|tokens| MatchedRule::Inherited { tokens });
    }
    Some(MatchedRule::Direct {
        scope: rule.scope.clone(),
    })
}

fn collect_command_spans(node: tree_sitter::Node, spans: &mut Vec<Span>) {
    let mut cursor = node.walk();
    collect_command_spans_inner(&mut cursor, spans);
}

fn collect_command_spans_inner(cursor: &mut TreeCursor, spans: &mut Vec<Span>) {
    loop {
        let node = cursor.node();
        if node.kind() == "command" || node.kind() == "simple_command" {
            spans.push(Span {
                start: node.start_byte(),
                end: node.end_byte(),
            });
        }
        if cursor.goto_first_child() {
            collect_command_spans_inner(cursor, spans);
            cursor.goto_parent();
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
}

#[derive(Clone, Copy)]
struct Span {
    start: usize,
    end: usize,
}

fn extract_command_tokens(tree: &tree_sitter::Tree, source: &str, start: usize, end: usize) -> CommandTokens {
    let raw = source.get(start..end).unwrap_or("").trim().to_string();
    let tokens = split(&raw).unwrap_or_default();
    let has_unsafe_nodes =
        contains_unsafe_nodes(tree.root_node(), start, end) || contains_redirect_tokens(&tokens);
    let tool = tokens.first().cloned();
    CommandTokens {
        raw,
        tool,
        tokens,
        has_unsafe_nodes,
    }
}

fn contains_redirect_tokens(tokens: &[String]) -> bool {
    let redirect_tokens = [
        ">",
        ">>",
        "<",
        "<<",
        "<<<",
        "2>",
        "2>>",
        "1>",
        "1>>",
        "&>",
        "&>>",
        ">|",
    ];
    tokens
        .iter()
        .any(|token| redirect_tokens.iter().any(|entry| token == entry))
}

fn contains_unsafe_nodes(root: tree_sitter::Node, start: usize, end: usize) -> bool {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if node.end_byte() < start || node.start_byte() > end {
            continue;
        }
        let kind = node.kind();
        if matches!(
            kind,
            "command_substitution"
                | "process_substitution"
                | "parameter_expansion"
                | "arithmetic_expansion"
                | "brace_expansion"
                | "heredoc_body"
                | "heredoc_redirect"
                | "variable_assignment"
        ) {
            return true;
        }
        if node.child_count() > 0 {
            let mut inner = node.walk();
            if inner.goto_first_child() {
                loop {
                    stack.push(inner.node());
                    if !inner.goto_next_sibling() {
                        break;
                    }
                }
            }
        }
    }
    false
}

struct CwdState {
    cwd: Option<PathBuf>,
    #[allow(dead_code)]
    stack: Vec<PathBuf>,
}

impl CwdState {
    fn new(cwd: PathBuf) -> Self {
        Self {
            cwd: Some(cwd),
            stack: Vec::new(),
        }
    }

    fn mark_unknown(&mut self) {
        self.cwd = None;
    }

    fn current_cwd(&self) -> Option<&PathBuf> {
        self.cwd.as_ref()
    }

    fn current_cwd_string(&self) -> Option<String> {
        self.cwd
            .as_ref()
            .map(|cwd| cwd.to_string_lossy().to_string())
    }
}

fn update_cwd_from_cd(state: &mut CwdState, tokens: &CommandTokens, default_root: &PathBuf) {
    let args: Vec<&String> = tokens.tokens.iter().skip(1).collect();
    if args.is_empty() {
        state.cwd = Some(default_root.clone());
        return;
    }
    let target = args[0].as_str();
    if target == "-" || target.contains('~') {
        state.mark_unknown();
        return;
    }
    let next = if Path::new(target).is_absolute() {
        PathBuf::from(target)
    } else if let Some(current) = state.cwd.as_ref() {
        current.join(target)
    } else {
        default_root.join(target)
    };
    state.cwd = Some(next);
}

fn resolve_cwd(cwd: Option<&str>, config: &Config) -> Result<PathBuf> {
    let mut resolved = if let Some(value) = cwd {
        let candidate = PathBuf::from(value);
        if candidate.is_absolute() {
            candidate
        } else {
            config.default_root.join(candidate)
        }
    } else {
        config.default_root.clone()
    };
    resolved = resolved
        .canonicalize()
        .map_err(|_| anyhow!("cwd not found: {}", resolved.display()))?;
    if !config.allow_escape && !is_within_roots(&resolved, config) {
        return Err(anyhow!("cwd must be within allowed roots"));
    }
    Ok(resolved)
}

fn is_within_roots(path: &PathBuf, config: &Config) -> bool {
    let canon = path.canonicalize().unwrap_or_else(|_| path.clone());
    config
        .roots
        .iter()
        .any(|root| canon.starts_with(&root.path))
}

fn parse_scope_list(value: Option<&Value>) -> HashSet<String> {
    let mut scopes = HashSet::new();
    if let Some(Value::String(scope)) = value {
        scopes.insert(scope.to_string());
    } else if let Some(Value::Array(items)) = value {
        for item in items {
            if let Some(scope) = item.as_str() {
                scopes.insert(scope.to_string());
            }
        }
    }
    scopes
}

fn is_scope_allowed(scope: &str, allowed_scopes: &HashSet<String>, denied_scopes: &HashSet<String>) -> bool {
    if is_scope_denied(scope, denied_scopes) {
        return false;
    }
    if allowed_scopes.is_empty() {
        return false;
    }
    allowed_scopes.iter().any(|allowed| scope_matches(scope, allowed))
}

fn is_scope_denied(scope: &str, denied_scopes: &HashSet<String>) -> bool {
    denied_scopes.iter().any(|denied| scope_matches(scope, denied))
}

fn scope_matches(scope: &str, entry: &str) -> bool {
    if entry.ends_with(":*") {
        let prefix = entry.trim_end_matches(":*");
        return scope == prefix || scope.starts_with(&format!("{}:", prefix));
    }
    scope == entry || scope.starts_with(&format!("{}:", entry))
}

fn load_rules_with_inline(path: &Path, inline: &[String]) -> Result<Vec<Rule>> {
    let mut content = String::new();
    append_inline_rules(&mut content, inline);
    if !content.is_empty() && !content.ends_with('\n') {
        content.push('\n');
    }
    content.push_str(&fs::read_to_string(path)?);
    let disabled = collect_disabled_rule_lines(&content);
    let mut rules = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with("disable ") {
            continue;
        }
        if disabled.contains(trimmed) {
            continue;
        }
        if let Some(rule) = parse_rule_line(trimmed) {
            rules.push(rule);
        }
    }
    Ok(rules)
}

fn append_inline_rules(content: &mut String, inline: &[String]) {
    for line in inline {
        if line.trim().is_empty() {
            continue;
        }
        if !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(line);
        if !line.ends_with('\n') {
            content.push('\n');
        }
    }
}

fn parse_inline_rules(value: &str) -> Vec<String> {
    value
        .lines()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

fn parse_rule_line(line: &str) -> Option<Rule> {
    let parts: Vec<&str> = line.splitn(2, "->").collect();
    if parts.len() != 2 {
        return None;
    }
    let scope = parts[0].trim().to_string();
    let pattern_raw = parts[1].trim();
    let tokens: Vec<&str> = pattern_raw.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let tool = tokens[0].to_string();
    let mut matchers = Vec::new();
    let mut has_subcommand = false;
    for token in tokens.iter().skip(1) {
        if *token == "*" {
            matchers.push(TokenMatcher::Wildcard);
            continue;
        }
        if *token == "{subcommand}" {
            matchers.push(TokenMatcher::Subcommand);
            has_subcommand = true;
            continue;
        }
        if token.starts_with('[') && token.ends_with(']') {
            let inner = token.trim_start_matches('[').trim_end_matches(']');
            let items: Vec<String> = inner
                .split(',')
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
                .collect();
            if items.iter().all(|item| item.starts_with('!')) {
                let deny: Vec<String> = items
                    .into_iter()
                    .map(|item| item.trim_start_matches('!').to_string())
                    .collect();
                matchers.push(TokenMatcher::Deny(deny));
            } else {
                matchers.push(TokenMatcher::Allow(items));
            }
            continue;
        }
        matchers.push(TokenMatcher::Exact((*token).to_string()));
    }
    Some(Rule {
        scope: scope.clone(),
        pattern: Pattern {
            tool,
            tokens: matchers,
            has_subcommand,
        },
        raw: format!("{} -> {}", scope, pattern_raw),
    })
}

fn ensure_rules_file(path: &Path) -> Result<()> {
    let default_rules = include_str!("../assets/rules.default.txt");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if !path.exists() {
        fs::write(path, default_rules)?;
        return Ok(());
    }
    let mut current = fs::read_to_string(path)?;
    let existing_lines = collect_rule_lines(&current);
    let disabled_lines = collect_disabled_rule_lines(&current);
    let mut additions = Vec::new();
    for line in default_rules.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if existing_lines.contains(trimmed) || disabled_lines.contains(trimmed) {
            continue;
        }
        additions.push(trimmed.to_string());
    }
    if !additions.is_empty() {
        if !current.ends_with('\n') {
            current.push('\n');
        }
        for line in additions {
            current.push_str(&line);
            current.push('\n');
        }
        fs::write(path, current)?;
    }
    Ok(())
}

fn collect_rule_lines(content: &str) -> HashSet<String> {
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#') && !line.starts_with("disable "))
        .map(|line| line.to_string())
        .collect()
}

fn collect_disabled_rule_lines(content: &str) -> HashSet<String> {
    content
        .lines()
        .map(|line| line.trim())
        .filter(|line| line.starts_with("disable "))
        .map(|line| line.trim_start_matches("disable ").trim().to_string())
        .collect()
}

fn load_bash_language() -> Language {
    tree_sitter_bash::LANGUAGE.into()
}

fn load_config() -> Result<Config> {
    let cwd = env::current_dir()?;
    let mut roots = vec![RootConfig {
        path: cwd.clone(),
        default: true,
    }];
    let mut allow_escape = false;
    let mut dynamic_scopes = true;
    let mut rules_path = cwd.join(".mcp-cli").join("rules.txt");
    let mut rules: Vec<String> = Vec::new();

    if let Ok(value) = env::var("MCP_ROOT") {
        roots = vec![RootConfig {
            path: PathBuf::from(value),
            default: true,
        }];
    }
    if let Ok(value) = env::var("MCP_ALLOWED_ROOTS") {
        for path in value.split(',').map(|item| item.trim()).filter(|item| !item.is_empty()) {
            roots.push(RootConfig {
                path: PathBuf::from(path),
                default: false,
            });
        }
    }
    if let Ok(value) = env::var("MCP_ALLOW_ESCAPE") {
        allow_escape = matches!(value.as_str(), "1" | "true" | "yes");
    }
    if let Ok(value) = env::var("MCP_DYNAMIC_SCOPES") {
        dynamic_scopes = matches!(value.as_str(), "1" | "true" | "yes");
    }
    if let Ok(value) = env::var("MCP_RULES") {
        rules_path = PathBuf::from(value);
    }
    if let Ok(value) = env::var("MCP_RULES_INLINE") {
        rules.extend(parse_inline_rules(&value));
    }
    let mut config_path: Option<PathBuf> = None;
    let mut print_schema = false;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => {
                if let Some(value) = args.next() {
                    roots = vec![RootConfig {
                        path: PathBuf::from(value),
                        default: true,
                    }];
                }
            }
            "--allow-root" => {
                if let Some(value) = args.next() {
                    roots.push(RootConfig {
                        path: PathBuf::from(value),
                        default: false,
                    });
                }
            }
            "--allow-escape" => {
                allow_escape = true;
            }
            "--dynamic-scopes" => {
                dynamic_scopes = true;
            }
            "--rules" => {
                if let Some(value) = args.next() {
                    rules_path = PathBuf::from(value);
                }
            }
            "--config" => {
                if let Some(value) = args.next() {
                    config_path = Some(PathBuf::from(value));
                }
            }
            "--print-config-schema" => {
                print_schema = true;
            }
            _ => {}
        }
    }

    let config = finalize_config(
        roots,
        allow_escape,
        dynamic_scopes,
        rules_path,
        rules,
    )?;

    if let Some(path) = config_path.or_else(|| env::var("MCP_CONFIG").ok().map(PathBuf::from)) {
        let value = load_config_value(&path)?;
        let config = apply_config_override(config, &value)?;
        if print_schema {
            let payload = serde_json::to_string_pretty(&config_schema())?;
            println!("{}", payload);
        }
        return Ok(config);
    }
    if print_schema {
        let payload = serde_json::to_string_pretty(&config_schema())?;
        println!("{}", payload);
    }
    Ok(config)
}

fn apply_initialize_config(config: &mut Config, req: &Request) -> Result<()> {
    let Some(value) = req
        .params
        .get("capabilities")
        .and_then(|caps| caps.get("experimental"))
        .and_then(|exp| exp.get("configuration"))
    else {
        return Ok(());
    };
    let updated = apply_config_override(config.clone(), value)?;
    *config = updated;
    Ok(())
}

fn load_config_value(path: &Path) -> Result<Value> {
    let content = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&content)?;
    Ok(value)
}

fn apply_config_override(mut base: Config, value: &Value) -> Result<Config> {
    let obj = value
        .as_object()
        .ok_or_else(|| anyhow!("config must be an object"))?;
    for (key, value) in obj {
        match key.as_str() {
            "roots" => {
                let inputs = parse_root_inputs(value)?;
                base.roots = inputs;
            }
            "allow_escape" => {
                if !value.is_null() {
                    base.allow_escape = value
                        .as_bool()
                        .ok_or_else(|| anyhow!("allow_escape must be boolean"))?;
                }
            }
            "dynamic_scopes" => {
                if !value.is_null() {
                    base.dynamic_scopes = value
                        .as_bool()
                        .ok_or_else(|| anyhow!("dynamic_scopes must be boolean"))?;
                }
            }
            "rules_path" => {
                if !value.is_null() {
                    base.rules_path = PathBuf::from(
                        value
                            .as_str()
                            .ok_or_else(|| anyhow!("rules_path must be string"))?,
                    );
                }
            }
            "rules" => {
                if value.is_null() {
                    base.rules.clear();
                } else if let Some(items) = value.as_array() {
                    base.rules = items
                        .iter()
                        .filter_map(|item| item.as_str().map(|value| value.to_string()))
                        .collect();
                } else {
                    return Err(anyhow!("rules must be array of strings"));
                }
            }
            _ => return Err(anyhow!("unknown config key: {}", key)),
        }
    }
    finalize_config(
        base.roots.clone(),
        base.allow_escape,
        base.dynamic_scopes,
        base.rules_path.clone(),
        base.rules.clone(),
    )
}

fn parse_root_inputs(value: &Value) -> Result<Vec<RootConfig>> {
    let items = value
        .as_array()
        .ok_or_else(|| anyhow!("roots must be an array"))?;
    let mut roots = Vec::new();
    for item in items {
        let obj = item
            .as_object()
            .ok_or_else(|| anyhow!("root entries must be objects"))?;
        let path = obj
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("root path is required"))?;
        let default = obj
            .get("default")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        roots.push(RootConfig {
            path: PathBuf::from(path),
            default,
        });
    }
    if roots.is_empty() {
        return Err(anyhow!("roots must not be empty"));
    }
    Ok(roots)
}

fn finalize_config(
    mut roots: Vec<RootConfig>,
    allow_escape: bool,
    dynamic_scopes: bool,
    rules_path: PathBuf,
    rules: Vec<String>,
) -> Result<Config> {
    let default_index = roots
        .iter()
        .position(|root| root.default)
        .unwrap_or(0);
    for (index, root) in roots.iter_mut().enumerate() {
        root.default = index == default_index;
        root.path = root
            .path
            .canonicalize()
            .unwrap_or_else(|_| root.path.clone());
    }
    let default_root = roots[default_index].path.clone();
    Ok(Config {
        roots,
        default_root,
        allow_escape,
        dynamic_scopes,
        rules_path,
        rules,
    })
}

fn config_schema() -> Value {
    json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "title": "mcp-cli configuration",
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "roots": {
                "type": "array",
                "minItems": 1,
                "description": "Allowed roots. The default root is used for relative paths.",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "path": { "type": "string", "description": "Absolute or root-relative path." },
                        "default": { "type": "boolean", "description": "True when this is the default root." }
                    },
                    "required": ["path"]
                }
            },
            "allow_escape": {
                "type": "boolean",
                "description": "Allow paths outside configured roots.",
                "scope": "any"
            },
            "dynamic_scopes": {
                "type": "boolean",
                "description": "Allow execution when scopes are only known at runtime. Default: true.",
                "scope": "any"
            },
            "rules_path": {
                "type": "string",
                "description": "Path to the command rules file.",
                "scope": "configuration"
            },
            "rules": {
                "type": "array",
                "description": "Inline command rules in the same DSL format as the rules file.",
                "items": { "type": "string" },
                "scope": "any"
            }
        },
        "required": ["roots"]
    })
}
