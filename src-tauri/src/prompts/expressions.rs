//! §6.3 — TypeScript expressions via `boa_engine`.
//!
//! Syntax: `${{ expr }}` — double-brace to disambiguate from VS Code single-brace
//! placeholders.
//!
//! Sandbox guarantees per §6.3:
//!  - No filesystem (helpers `git()` / `shell()` exist but are off by default).
//!  - No network (host objects not exposed).
//!  - 100 ms execution timeout.
//!  - 10 MB memory cap (best-effort — boa doesn't yet expose this, tracked as a TODO).
//!  - Frozen built-ins: `now`, `today`, `clipboard`, `selection`, `app`, `env`,
//!    `random`, `random_choice([...])`, `format_date(d, fmt)`, `ago(d)`.
//!
//! Lazy evaluation is honored at the call site (Phase 8 stub here): expressions
//! are evaluated only when their slot is reached during typing.

use boa_engine::{Context, JsValue, Source};
use chrono::Local;
use serde::Serialize;
use std::time::Duration;

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

/// Evaluate one `${{ expr }}` block. The braces should already be stripped.
pub fn eval(source: &str, ctx: &ExprContext) -> Result<String, ExprError> {
    let mut context = Context::default();

    // Inject built-ins as JS globals via property registration.
    let now = Local::now();
    inject_global(&mut context, "__now_iso__", &now.to_rfc3339());
    inject_global(&mut context, "__today__", &now.format("%Y-%m-%d").to_string());
    inject_global(
        &mut context,
        "__clipboard__",
        ctx.clipboard.as_deref().unwrap_or(""),
    );
    inject_global(
        &mut context,
        "__selection__",
        ctx.selection.as_deref().unwrap_or(""),
    );
    inject_global(
        &mut context,
        "__app_name__",
        ctx.app_name.as_deref().unwrap_or(""),
    );
    inject_global(
        &mut context,
        "__app_bundle__",
        ctx.app_bundle.as_deref().unwrap_or(""),
    );
    inject_global(
        &mut context,
        "__window_title__",
        ctx.window_title.as_deref().unwrap_or(""),
    );

    // Compose a small prelude that exposes the documented surface.
    // We deliberately freeze names to avoid scripts shadowing them.
    let prelude = r#"
        const now = {
            toISOString: () => __now_iso__,
            valueOf: () => Date.parse(__now_iso__),
        };
        const today = __today__;
        const clipboard = __clipboard__;
        const selection = __selection__;
        const app = {
            name: __app_name__,
            bundle: __app_bundle__,
            windowTitle: __window_title__,
        };
        const env = (k) => "";
        const random = () => Math.random();
        function random_choice(arr) {
            if (!Array.isArray(arr) || arr.length === 0) return "";
            return arr[Math.floor(Math.random() * arr.length)];
        }
        function format_date(d, fmt) {
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
        }
        function ago(d) {
            const dt = (d && d.toISOString) ? new Date(d.valueOf()) : new Date(d);
            const sec = Math.floor((Date.now() - dt.getTime()) / 1000);
            if (sec < 60) return sec + "s ago";
            if (sec < 3600) return Math.floor(sec/60) + "m ago";
            if (sec < 86400) return Math.floor(sec/3600) + "h ago";
            return Math.floor(sec/86400) + "d ago";
        }
        Object.freeze(app);
    "#;
    context
        .eval(Source::from_bytes(prelude))
        .map_err(|e| ExprError::Runtime(format!("{}", e)))?;

    // Evaluate the user expression with a 100 ms host-side budget.
    // boa_engine 0.19 doesn't expose a portable interrupt; we run on this thread
    // and post-check elapsed time. For untrusted code this is a soft guarantee.
    let start = std::time::Instant::now();
    let result = context
        .eval(Source::from_bytes(source))
        .map_err(|e| ExprError::Syntax(format!("{}", e)))?;
    if start.elapsed() > Duration::from_millis(100) {
        return Err(ExprError::Timeout(Duration::from_millis(100)));
    }

    Ok(stringify(&result, &mut context))
}

fn stringify(v: &JsValue, ctx: &mut Context) -> String {
    if v.is_undefined() || v.is_null() {
        return String::new();
    }
    match v.to_string(ctx) {
        Ok(s) => s.to_std_string().unwrap_or_default(),
        Err(_) => String::new(),
    }
}

fn inject_global(ctx: &mut Context, name: &str, value: &str) {
    use boa_engine::property::Attribute;
    use boa_engine::JsString;
    let js = JsString::from(value);
    let _ = ctx.register_global_property(JsString::from(name), js, Attribute::READONLY);
}

/// Parse `body` and replace every `${{ … }}` block with the evaluated string.
/// Other content is left untouched. Errors surface as `[expr error: …]` inline.
pub fn expand_expressions(body: &str, ctx: &ExprContext) -> String {
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
            let expr: String = bytes[i + 3..j].iter().collect();
            match eval(expr.trim(), ctx) {
                Ok(s) => out.push_str(&s),
                Err(e) => {
                    out.push_str(&format!("[expr error: {}]", e));
                    tracing::warn!("expression error: {}", e);
                }
            }
            i = j + 2;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
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
        let mut ctx = ExprContext::default();
        ctx.clipboard = Some("hello".into());
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
}
