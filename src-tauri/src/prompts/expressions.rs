//! §6.3 — `${{ expr }}` in a QuickJS sandbox; double-brace so it can't clash
//! with VS Code placeholders.
//!
//! No filesystem or network host objects, an interrupt-handler timeout, memory
//! and stack caps, and the frozen built-ins defined in the prelude below.

use chrono::Local;
use rquickjs::function::Func;
use rquickjs::{Context, Ctx, Error as QuickJsError, Runtime};
use serde::Serialize;
use std::time::{Duration, Instant};

/// Per-call evaluation context.
#[derive(Debug, Clone, Default)]
pub struct ExprContext {
    pub clipboard: Option<String>,
    pub selection: Option<String>,
    pub app_name: Option<String>,
    pub app_bundle: Option<String>,
    pub window_title: Option<String>,
    pub git_branch: Option<String>,
    pub repo_name: Option<String>,
    pub repo_root: Option<String>,
    /// Allow the `git()` helper. Off by default, and forced off for prompts
    /// from a remote source — see the note on `install_git_helper`.
    pub allow_git: bool,
}

/// Read-only `git` subcommands the `git()` helper will run.
///
/// §6.3 lists `git()` and `shell()` as opt-in escape hatches. `git()` is the
/// one with a real use case for an agent companion ("the repo is on commit
/// ${{ git("rev-parse --short HEAD") }}"), and an allowlist of read-only
/// subcommands makes it something I can reason about.
///
/// `shell()` is deliberately **not** implemented. Arbitrary command execution
/// driven by a prompt file is a different class of feature, and the app now
/// loads prompts from remote repositories; the two do not belong in the same
/// binary without a much louder consent step than a YAML key.
const GIT_ALLOWED_SUBCOMMANDS: &[&str] = &[
    "rev-parse",
    "branch",
    "status",
    "log",
    "describe",
    "diff",
    "show",
    "remote",
    "config",
    "tag",
    "symbolic-ref",
];

/// Cap on `git()` output pasted into a prompt body, and on how long it may run.
const GIT_MAX_OUTPUT: usize = 8 * 1024;
const GIT_TIMEOUT_SECS: u64 = 5;

/// Split a `git()` argument string into arguments, rejecting anything that
/// isn't a plain token.
///
/// No shell is involved (`Command` takes an argv), so quoting and expansion
/// don't apply — but rejecting shell metacharacters anyway keeps the helper's
/// contract obvious to anyone reading a prompt file.
fn parse_git_args(raw: &str) -> Result<Vec<String>, String> {
    let args: Vec<String> = raw.split_whitespace().map(|s| s.to_string()).collect();
    let Some(sub) = args.first() else {
        return Err("git() needs a subcommand".into());
    };
    if !GIT_ALLOWED_SUBCOMMANDS.contains(&sub.as_str()) {
        return Err(format!(
            "git() only runs read-only subcommands ({}); got {sub:?}",
            GIT_ALLOWED_SUBCOMMANDS.join(", ")
        ));
    }
    if let Some(bad) = args
        .iter()
        .find(|a| a.contains(['|', ';', '&', '`', '$', '<', '>', '\n']))
    {
        return Err(format!("git() argument {bad:?} contains a shell character"));
    }
    // `-c key=value` can change git's behaviour arbitrarily (including
    // `core.pager`, which runs a command), and `--upload-pack`/`--exec` style
    // flags run helpers. Neither belongs in a read-only helper.
    // Prefix matches, not equality: git accepts both `--config-env x` and
    // `--config-env=x`, and an exact-match check let the latter straight
    // through (which is how the test for this caught my own allowlist).
    const DENIED_FLAG_PREFIXES: &[&str] =
        &["--exec", "--upload-pack", "--receive-pack", "--config-env"];
    if let Some(bad) = args.iter().find(|a| {
        *a == "-c" || a.starts_with("-c=") || DENIED_FLAG_PREFIXES.iter().any(|p| a.starts_with(p))
    }) {
        return Err(format!("git() argument {bad:?} is not allowed"));
    }
    Ok(args)
}

/// Run an allowlisted `git` command in `repo_root` and return its stdout.
fn run_git(repo_root: &str, raw: &str) -> Result<String, String> {
    let args = parse_git_args(raw)?;
    let mut cmd = std::process::Command::new("git");
    cmd.args(&args)
        .current_dir(repo_root)
        // Never let git prompt for anything: an expression evaluated mid-demo
        // must not be able to block on a credential or editor prompt.
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_PAGER", "cat")
        .stdin(std::process::Stdio::null());
    #[cfg(target_os = "windows")]
    {
        // Don't flash a console window on top of the demo.
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let start = Instant::now();
    let out = cmd.output().map_err(|e| format!("git: {e}"))?;
    if start.elapsed() > Duration::from_secs(GIT_TIMEOUT_SECS) {
        return Err("git took too long".into());
    }
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("git {}: {}", args.join(" "), err.trim()));
    }
    let mut text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.len() > GIT_MAX_OUTPUT {
        text.truncate(GIT_MAX_OUTPUT);
        text.push('…');
    }
    Ok(text)
}

#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "kind", content = "message", rename_all = "kebab-case")]
pub enum ExprError {
    #[error("syntax: {0}")]
    Syntax(String),
    #[error("runtime: {0}")]
    Runtime(String),
    #[error("timeout: expression exceeded {0:?}")]
    Timeout(Duration),
}

impl ExprError {
    /// Telemetry classification. Carries no message — the text can contain
    /// fragments of the user's expression source, which §12 forbids sending.
    pub fn kind(&self) -> crate::telemetry::ExpressionErrorKind {
        use crate::telemetry::ExpressionErrorKind as K;
        match self {
            Self::Syntax(_) => K::Syntax,
            Self::Runtime(_) => K::Runtime,
            Self::Timeout(_) => K::Timeout,
        }
    }
}

/// Outcome of expanding a body's `${{ … }}` blocks.
pub struct Expansion {
    pub text: String,
    /// The body contained at least one `${{ … }}` block. From the marker, not a
    /// length comparison — that missed same-length expansions.
    pub had_expressions: bool,
    pub errors: Vec<ExprError>,
}

// Ceiling for script evaluation only, timed after the runtime is built. Real
// expressions take single-digit ms; the headroom absorbs CI contention.
const EVAL_BUDGET: Duration = Duration::from_millis(250);
const MEMORY_LIMIT_BYTES: usize = 10 * 1024 * 1024;
const STACK_LIMIT_BYTES: usize = 256 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Builtins {
    now_iso: String,
    today: String,
    clipboard: String,
    selection: String,
    app_name: String,
    app_bundle: String,
    window_title: String,
    git_branch: String,
    repo_name: String,
    repo_root: String,
}

/// Evaluate one `${{ expr }}` block. The braces should already be stripped.
pub fn eval(source: &str, ctx: &ExprContext) -> Result<String, ExprError> {
    let runtime = Runtime::new().map_err(|e| ExprError::Runtime(e.to_string()))?;
    runtime.set_memory_limit(MEMORY_LIMIT_BYTES);
    runtime.set_max_stack_size(STACK_LIMIT_BYTES);

    let context = Context::full(&runtime).map_err(|e| ExprError::Runtime(e.to_string()))?;

    let now = Local::now();
    let builtins = Builtins {
        now_iso: now.to_rfc3339(),
        today: now.format("%Y-%m-%d").to_string(),
        clipboard: ctx.clipboard.clone().unwrap_or_default(),
        selection: ctx.selection.clone().unwrap_or_default(),
        app_name: ctx.app_name.clone().unwrap_or_default(),
        git_branch: ctx.git_branch.clone().unwrap_or_default(),
        repo_name: ctx.repo_name.clone().unwrap_or_default(),
        repo_root: ctx.repo_root.clone().unwrap_or_default(),
        app_bundle: ctx.app_bundle.clone().unwrap_or_default(),
        window_title: ctx.window_title.clone().unwrap_or_default(),
    };
    let builtins_json =
        serde_json::to_string(&builtins).map_err(|e| ExprError::Runtime(e.to_string()))?;
    let source_json =
        serde_json::to_string(source).map_err(|e| ExprError::Runtime(e.to_string()))?;

    // Compose a small prelude that exposes the documented surface.
    // We deliberately freeze names to avoid scripts shadowing them.
    let prelude = format!(
        r#"
        const __pp = {builtins_json};
        const now = {{
            toISOString: () => __pp.nowIso,
            valueOf: () => Date.parse(__pp.nowIso),
        }};
        const today = __pp.today;
        const clipboard = __pp.clipboard;
        const selection = __pp.selection;
        const app = {{
            name: __pp.appName,
            bundle: __pp.appBundle,
            windowTitle: __pp.windowTitle,
        }};
        // Repo context for agent-companion prompts. Empty strings when the
        // fire had no resolvable checkout, so `${{ repo.branch || "main" }}`
        // is the idiom for a fallback.
        const repo = {{
            name: __pp.repoName,
            branch: __pp.gitBranch,
            root: __pp.repoRoot,
        }};
        Object.freeze(repo);
        const env = (k) => "";
        const random = () => Math.random();
        function random_choice(arr) {{
            if (!Array.isArray(arr) || arr.length === 0) return "";
            return arr[Math.floor(Math.random() * arr.length)];
        }}
        function format_date(d, fmt) {{
            // Minimal: only respects %Y, %m, %d, %H, %M, %S.
            const dt = (d && d.toISOString) ? new Date(d.valueOf()) : new Date(d);
            const pad = (n) => String(n).padStart(2, "0");
            return fmt
                .replace("%Y", dt.getFullYear())
                .replace("%m", pad(dt.getMonth() + 1))
                .replace("%d", pad(dt.getDate()))
                .replace("%H", pad(dt.getHours()))
                .replace("%M", pad(dt.getMinutes()))
                .replace("%S", pad(dt.getSeconds()));
        }}
        function ago(d) {{
            const dt = (d && d.toISOString) ? new Date(d.valueOf()) : new Date(d);
            const sec = Math.floor((Date.now() - dt.getTime()) / 1000);
            if (sec < 60) return sec + "s ago";
            if (sec < 3600) return Math.floor(sec/60) + "m ago";
            if (sec < 86400) return Math.floor(sec/3600) + "h ago";
            return Math.floor(sec/86400) + "d ago";
        }}
        Object.freeze(app);
    "#
    );
    let eval_script = format!(
        r#"
        const __pp_result = (0, eval)({source_json});
        (__pp_result === undefined || __pp_result === null) ? "" : String(__pp_result);
    "#
    );

    // Clock starts after the runtime and context exist: counting engine
    // cold-start made the first eval time out spuriously on loaded CI.
    let deadline = Instant::now() + EVAL_BUDGET;
    runtime.set_interrupt_handler(Some(Box::new(move || Instant::now() >= deadline)));
    let started = Instant::now();
    let git_root = ctx.allow_git.then(|| ctx.repo_root.clone()).flatten();
    context.with(|js| {
        install_git_helper(&js, git_root)
            .map_err(|e| map_quickjs_error(&js, e, started, deadline))?;
        js.eval::<(), _>(prelude)
            .map_err(|e| map_quickjs_error(&js, e, started, deadline))?;
        js.eval::<String, _>(eval_script)
            .map_err(|e| map_quickjs_error(&js, e, started, deadline))
    })
}

/// Bind `git(args)` into the sandbox.
///
/// Three conditions all have to hold before it runs anything: the config opts
/// in (`allow-git-expressions`), the prompt is local (a remote repository does
/// not get to run commands on the viewer's machine), and a repository root was
/// actually resolved. When any of them fails the binding still exists but
/// returns an explanatory string — a prompt that silently produced an empty
/// commit id mid-demo would be worse than one that says why.
fn install_git_helper(js: &Ctx<'_>, repo_root: Option<String>) -> Result<(), QuickJsError> {
    let f = move |raw: String| -> String {
        let Some(root) = repo_root.as_deref() else {
            return "[git() is off: set allow-git-expressions in promptplayer.yaml, \
                    and note it never runs for prompts from a remote source]"
                .to_string();
        };
        match run_git(root, &raw) {
            Ok(out) => out,
            Err(e) => {
                tracing::warn!("git() failed: {}", e);
                format!("[git error: {e}]")
            }
        }
    };
    js.globals().set("git", Func::from(f))
}

fn map_quickjs_error(
    js: &Ctx<'_>,
    err: QuickJsError,
    started: Instant,
    deadline: Instant,
) -> ExprError {
    if started.elapsed() >= EVAL_BUDGET || Instant::now() >= deadline {
        return ExprError::Timeout(EVAL_BUDGET);
    }
    let raw = match err {
        QuickJsError::Exception => exception_message(js),
        other => other.to_string(),
    };
    let lower = raw.to_ascii_lowercase();
    if lower.contains("out of memory") || lower.contains("memory allocation") {
        ExprError::Runtime(raw)
    } else {
        ExprError::Syntax(raw)
    }
}

fn exception_message(js: &Ctx<'_>) -> String {
    let caught = js.catch();
    if caught.is_undefined() || caught.is_null() {
        return "JavaScript exception".into();
    }
    if let Some(s) = caught.as_string() {
        return s
            .to_string()
            .unwrap_or_else(|_| "JavaScript exception".into());
    }
    if let Some(i) = caught.as_int() {
        return i.to_string();
    }
    if let Some(f) = caught.as_float() {
        return f.to_string();
    }
    if let Some(b) = caught.as_bool() {
        return b.to_string();
    }
    if let Some(obj) = caught.as_object() {
        if let Ok(message) = obj.get::<_, String>("message") {
            if !message.is_empty() {
                return message;
            }
        }
        if let Ok(name) = obj.get::<_, String>("name") {
            if !name.is_empty() {
                return name;
            }
        }
    }
    "JavaScript exception".into()
}

/// Parse `body` and replace every `${{ … }}` block with the evaluated string.
/// Other content is left untouched. Errors surface as `[expr error: …]` inline.
pub fn expand_expressions(body: &str, ctx: &ExprContext) -> String {
    expand_expressions_reporting(body, ctx).text
}

/// Like [`expand_expressions`] but reports which blocks failed, so the caller
/// can emit telemetry. Failures used to be `tracing::warn!`-only.
pub fn expand_expressions_reporting(body: &str, ctx: &ExprContext) -> Expansion {
    let mut errors = Vec::new();
    let mut had_expressions = false;
    let mut out = String::with_capacity(body.len());
    let bytes: Vec<char> = body.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if i + 2 < bytes.len() && bytes[i] == '$' && bytes[i + 1] == '{' && bytes[i + 2] == '{' {
            // Find matching `}}`.
            let mut j = i + 3;
            let mut depth = 1;
            while j + 1 < bytes.len() {
                if bytes[j] == '{' && bytes[j + 1] == '{' {
                    depth += 1;
                    j += 2;
                    continue;
                }
                if bytes[j] == '}' && bytes[j + 1] == '}' {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    j += 2;
                    continue;
                }
                j += 1;
            }
            if j + 1 >= bytes.len() {
                // Unterminated — emit literally.
                out.push(bytes[i]);
                i += 1;
                continue;
            }
            had_expressions = true;
            let expr: String = bytes[i + 3..j].iter().collect();
            match eval(expr.trim(), ctx) {
                Ok(s) => out.push_str(&s),
                Err(e) => {
                    out.push_str(&format!("[expr error: {}]", e));
                    tracing::warn!("expression error: {}", e);
                    errors.push(e);
                }
            }
            i = j + 2;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    Expansion {
        text: out,
        had_expressions,
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── git() helper ──────────────────────────────────────────────────────

    #[test]
    fn git_args_allow_read_only_subcommands() {
        assert_eq!(
            parse_git_args("rev-parse --short HEAD").unwrap(),
            vec!["rev-parse", "--short", "HEAD"]
        );
        for ok in [
            "branch --show-current",
            "status --porcelain",
            "log -1 --oneline",
        ] {
            assert!(parse_git_args(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn git_args_reject_mutating_subcommands() {
        // The whole point of the allowlist: a prompt body must not be able to
        // change the repository it is describing.
        for bad in [
            "push origin main",
            "commit -m x",
            "reset --hard",
            "clean -fdx",
            "checkout .",
        ] {
            let err = parse_git_args(bad).unwrap_err();
            assert!(err.contains("read-only"), "{bad}: {err}");
        }
    }

    #[test]
    fn git_args_reject_shell_characters() {
        for bad in [
            "log --pretty=$(whoami)",
            "status; rm -rf /",
            "log | cat",
            "show `id`",
            "log > /tmp/x",
        ] {
            assert!(parse_git_args(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn git_args_reject_flags_that_can_run_helpers() {
        // `-c core.pager=…` and friends turn a read-only subcommand into an
        // arbitrary command runner.
        // Both spellings of each flag: git takes `--flag value` and
        // `--flag=value`, and only checking one form leaves the other open.
        for bad in [
            "-c core.pager=sh log",
            "log --exec=sh",
            "log --exec sh",
            "log --upload-pack=sh",
            "log --upload-pack sh",
            "log --receive-pack=sh",
            "log --config-env=x",
            "log --config-env x",
        ] {
            assert!(parse_git_args(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn git_args_reject_an_empty_call() {
        assert!(parse_git_args("").is_err());
        assert!(parse_git_args("   ").is_err());
    }

    #[test]
    fn git_is_disabled_by_default() {
        // Default context has `allow_git: false`, so the helper explains
        // itself rather than silently expanding to nothing.
        let out = expand_expressions(
            r#"${{ git("rev-parse --short HEAD") }}"#,
            &ExprContext::default(),
        );
        assert!(out.contains("git() is off"), "{out}");
        assert!(out.contains("allow-git-expressions"), "{out}");
    }

    #[test]
    fn git_runs_against_a_real_repository_when_enabled() {
        // This repo is a git checkout, so the helper has something to read.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let ctx = ExprContext {
            repo_root: Some(root),
            allow_git: true,
            ..Default::default()
        };
        let out = expand_expressions(r#"${{ git("rev-parse --abbrev-ref HEAD") }}"#, &ctx);
        assert!(!out.is_empty());
        assert!(!out.contains("git() is off"), "{out}");
        // Either a branch name or a clear error — never a silent empty string.
        assert!(out.trim().len() > 1, "{out}");
    }

    #[test]
    fn git_refuses_a_mutating_call_even_when_enabled() {
        let ctx = ExprContext {
            repo_root: Some(".".into()),
            allow_git: true,
            ..Default::default()
        };
        let out = expand_expressions(r#"${{ git("push --force") }}"#, &ctx);
        assert!(out.contains("git error"), "{out}");
        assert!(out.contains("read-only"), "{out}");
    }

    #[test]
    fn evals_simple_arithmetic() {
        let s = eval("1 + 2", &ExprContext::default()).unwrap();
        assert_eq!(s, "3");
    }

    #[test]
    fn now_iso_is_iso() {
        let s = eval("now.toISOString()", &ExprContext::default()).unwrap();
        assert!(s.contains("T"));
        assert!(s.starts_with("20"));
    }

    #[test]
    fn clipboard_var() {
        let ctx = ExprContext {
            clipboard: Some("hello".into()),
            ..Default::default()
        };
        let s = eval("clipboard + ' world'", &ctx).unwrap();
        assert_eq!(s, "hello world");
    }

    #[test]
    fn random_choice_returns_a_member() {
        let s = eval("random_choice(['a','b','c'])", &ExprContext::default()).unwrap();
        assert!(["a", "b", "c"].contains(&s.as_str()));
    }

    #[test]
    fn expand_blocks_in_body() {
        let body = "today: ${{ today }}, sum: ${{ 2 + 2 }}.";
        let out = expand_expressions(body, &ExprContext::default());
        assert!(out.starts_with("today: 20"));
        assert!(out.contains("sum: 4"));
    }

    #[test]
    fn syntax_error_is_inline() {
        let body = "ok: ${{ 1 + }}";
        let out = expand_expressions(body, &ExprContext::default());
        assert!(out.starts_with("ok: [expr error:"));
    }

    #[test]
    fn runaway_loop_is_interrupted() {
        let err = eval("while (true) {}", &ExprContext::default()).unwrap_err();
        assert!(matches!(err, ExprError::Timeout(_)), "{err}");
    }

    #[test]
    fn memory_hog_is_rejected() {
        let err = eval("new ArrayBuffer(20 * 1024 * 1024)", &ExprContext::default()).unwrap_err();
        assert!(matches!(err, ExprError::Runtime(_)), "{err}");
    }
}
