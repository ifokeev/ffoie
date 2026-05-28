---
name: commit
description: "Create conventional commit messages without co-author labels. Use when committing code changes or formatting git history."
user_invocable: true
metadata:
  version: "1.0.0"
---

# Conventional Commit

Create conventional commit messages following the Conventional Commits specification. Never add co-author labels.

## Format

```
<type>(<scope>): <subject>

<body>
```

## Types

| Type | Purpose |
|---|---|
| `feat` | New feature |
| `fix` | Bug fix |
| `refactor` | Code restructuring (no behavior change) |
| `docs` | Documentation only |
| `style` | Formatting, whitespace (no code change) |
| `test` | Adding or fixing tests |
| `chore` | Build, CI, tooling, dependencies |
| `perf` | Performance improvement |

## Scope (optional)

Use the affected area of the codebase as scope. Check the project structure to determine appropriate scopes.

## Rules

1. **No co-author labels** — never add `Co-Authored-By` lines
2. Subject line: imperative mood, lowercase, no period, max 72 chars
3. Body: explain **why**, not what. Wrap at 80 chars
4. One logical change per commit — split unrelated changes

## Steps

1. Run `git status` and `git diff --cached` to see staged changes
2. If nothing is staged, stage the relevant files (prefer specific files over `git add -A`)
3. Analyze the changes and determine the appropriate type and scope
4. Draft a concise commit message
5. Create the commit using a HEREDOC for proper formatting:
   ```bash
   git commit -m "$(cat <<'EOF'
   type(scope): subject line here

   Optional body explaining why this change was made.
   EOF
   )"
   ```
6. Run `git status` after to verify

## Examples

```
feat(frontend): add group chat with realtime subscriptions

feat(scheduler): add cron and interval task execution

fix(mcp): correct next_run calculation for once tasks

refactor(container): merge mcp server into main binary

chore: add Makefile and .env.example
```
