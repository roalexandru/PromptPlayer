# Example prompts

These ship with the app for first-run onboarding. They're loaded from
`~/Library/Application Support/PromptPlayer/prompts/` (Mac) or
`~/.config/promptplayer/prompts/` (Linux/Win) at startup.

Override the directory with `PROMPT_PLAYER_PROMPTS=/some/path` for testing.

`06-agent-review.pp.md` is the coding-agent example: it uses `$GIT_BRANCH` /
`$REPO_NAME`, a `${1|a,b,c|}` choice the palette asks about before firing, and
`newline-mode: backslash-enter` so its paragraphs survive a terminal prompt
that would otherwise submit on Shift+Enter. It ships disabled — enable it in
the library window when you want it live.
