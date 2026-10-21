\# Codex — Balanced Economy



\## Objective

Minimize total token consumption while maintaining

code quality, correctness and maintainability.



\## Routing



\### Trivial tasks

Examples: text edits, CSS tweaks, renaming,

small fixes and simple questions.



\- Handle directly.

\- Do not spawn subagents.

\- Do not inspect unrelated files.



\### Exploration

Examples: locating components, finding dependencies,

understanding existing implementations.



\- Delegate to explorer when independent exploration

&#x20; would avoid significant work in the main context.

\- Provide specific paths or search targets.

\- Request a concise report.



\### Development

Examples: new components, features, API integration,

state management and significant bug fixes.



\- Handle directly when one agent is sufficient.

\- Delegate independent implementation to developer

&#x20; only when useful.

\- Avoid multiple agents editing the same files.



\### Architecture

Examples: major architectural decisions,

complex debugging and cross-system changes.



\- Use architect only when simpler analysis

&#x20; is insufficient.

\- Request a focused recommendation.

\- Let the main agent implement the solution.



\## Efficiency rules



1\. Never spawn agents just to classify a task.

2\. Prefer zero subagents for small changes.

3\. Prefer one subagent for focused research.

4\. Use two only for independent work.

5\. Never delegate the same investigation twice.

6\. Reuse findings from completed agents.

7\. Read only necessary files.

8\. Avoid unnecessary dependencies.

9\. Avoid unrelated refactoring.

10\. Do not generate verbose reports.



\## Frontend standards



\- Prefer TypeScript.

\- Reuse design tokens and UI components.

\- Follow existing project conventions.

\- Build responsive and accessible interfaces.

\- Avoid unnecessary UI abstractions.

\- Use existing styling conventions.



\## Verification



\- Run focused checks for changed code.

\- Do not run the full test suite for trivial changes.

\- Report any tests not run.

\- Stop when requirements are met.



\## Response format



\- Summary: 1-3 sentences.

\- Changed files: short list.

\- Tests: result or not run.

\- Risks: only when relevant.



