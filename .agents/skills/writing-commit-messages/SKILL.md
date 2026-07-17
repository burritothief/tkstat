---
name: writing-commit-messages
description: >-
  Writes Git commit messages. Activates when the user asks to write
  a commit message, draft a commit message, or similar.
---

# Writing Commit Messages

Write commit messages that follow commit style guidelines for the project.

## Format

```
<subsystem>: <summary>

<reference issues/PRs/etc.>

<long form description>
```

## Rules

### Subject line

- **Subsystem prefix**: Use a short, lowercase identifier for the
  area of code changed. Determine this from the file paths in the
  diff. Prefer one of the crate's modules: `db`, `render`, `ingest`,
  `domain`, `cli`, `config`. For changes to `Cargo.toml`, CI, or the
  release workflow, use `build`. Use nested subsystems with `/` when
  helpful and exclusive (e.g., `render/heatmap`, `db/query`,
  `ingest/parser`, `domain/pricing`).
- **Summary**: Lowercase start (not capitalized), imperative mood,
  no trailing period. Keep it concise—ideally under 60 characters
  total for the whole subject line.

### References

- If the change relates to a GitHub issue, PR, or discussion, list
  the relevant numbers on their own lines after the subject, separated
  by a blank line. E.g. `#1234`
- If there are no references, omit this section entirely (no blank
  line).

### Long form description

- Describe **what changed**, **what the previous behavior was**,
  and **how the new behavior works** at a high level.
- Use plain prose, not bullet points. Wrap lines at ~72 characters.
- Focus on the _why_ and _how_ rather than restating the diff.
- Keep the tone direct and technical without no filler phrases.
- Don't exceed a handful of paragraphs; less is more.

### Trailer

- End every commit message with the required attribution trailer,
  separated from the body by a blank line:

  ```
  Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
  ```

## Workflow

- Run `git diff` (and `git status`) to see what changes are present
  since the last commit.
- Identify the subsystem from the changed file paths.
- Identify any referenced issues/PRs from the diff context or
  branch name.
- Run `cargo test` and `cargo clippy -- -D warnings` before
  committing; both must pass with zero failures and zero warnings.
- Draft the commit message following the format above.
- Apply the commit.
- Don't push the commit; leave that to the user.
