---
version: 1
slug: "ui-launcher-slint"
primary_target: "ui/launcher.slint"
related_targets: []
---

# Settings launcher

Scope: `ui/launcher.slint` (native Slint window, Windows). Mode: Operate.
Audience: ordinary Windows users configuring tile-rs with the mouse. Task: change layout, gaps, hotkeys, app rules, install updates, then save.

## Direction contract

THESIS: A light, soft settings dashboard pinned by the user's reference (file-manager dashboard): it refuses the dense dark form-list the launcher used to be.
OWN-WORLD: Lavender-grey canvas #eef1f8, white 18px cards with soft offset shadows, white icon+label sidebar, pill buttons, outline icons (authored SVG, 1.75 stroke), single blue accent #008cff with #0070e0 for filled controls, Segoe UI.
STORY: The user sees the current layout and a real summary at a glance, changes a setting with a preview, sees "unsaved changes", saves.
FIRST VIEWPORT: Sidebar left (logo mark = live layout miniature, 8 nav items, version + quit card at bottom). Main: page title + subtitle, Save pill top right. General page: four layout cards (chosen one filled blue, white tiles) over a motion/gaps card; right column "Сводка" with a hotkey coverage ring, rule and workspace counts, and an update card.
FORM: Pinned by the user's reference image (no concept-seed roll; brief-pinned direction). Signature move: the live layout miniature that follows layout, gap and master count.
FINISH: unreviewed and undocumented is unfinished; this build ends with the finish review, the verdict, DESIGN.md, and every shipping raster carrying its provenance
