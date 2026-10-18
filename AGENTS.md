\# Token-efficient workflow



\- Inspect only files relevant to the requested task.

\- Search filenames and symbols before opening file contents.

\- Avoid reading entire directories, generated files, and large logs.

\- Make minimal targeted changes; preserve existing UI and architecture.

\- Do not modify unrelated components or dependencies.

\- Prefer focused checks and tests; run full builds only when justified.

\- Use MCP tools only when needed; avoid unnecessary tool calls.

\- Keep final reports concise: changed files, tests, remaining issues.

\- Never skip essential verification to save tokens.

\- Ask before destructive actions or installing dependencies.
