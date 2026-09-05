//! §6.3 — `${{ expr }}` in a QuickJS sandbox; double-brace so it can't clash
//! with VS Code placeholders.
//!
//! No filesystem or network host objects, an interrupt-handler timeout, memory
//! and stack caps, and the frozen built-ins defined in the prelude below.

use chrono::Local;
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
    /// If true, allow `git()` and `shell()` helpers. Off by default.
    pub allow_shell: bool,
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
    context.with(|js| {
        js.eval::<(), _>(prelude)
            .map_err(|e| map_quickjs_error(&js, e, started, deadline))?;
        js.eval::<String, _>(eval_script)
            .map_err(|e| map_quickjs_error(&js, e, started, deadline))
    })
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
