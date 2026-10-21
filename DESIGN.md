---
name: tile-rs
description: Settings launcher for a tiling window manager on Windows; a light, soft dashboard in one blue.
colors:
  accent: "#008cff"
  accent-strong: "#0070e0"
  accent-strong-hover: "#0062c7"
  accent-soft: "#e6f2ff"
  accent-soft-hover: "#d6eaff"
  accent-ink: "#005fc0"
  accent-hover-line: "#b9d9fb"
  focus-on-accent: "#9ccdff"
  canvas: "#eef1f8"
  surface: "#ffffff"
  sunken: "#f4f6fb"
  line: "#e3e8f2"
  card-line: "#e8ecf4"
  switch-off: "#cdd4e2"
  ink: "#1d2433"
  ink-2: "#556075"
  muted: "#687287"
  danger: "#d93d42"
  danger-soft: "#fdeeee"
  warn-ink: "#8a5300"
  warn-soft: "#fff3dc"
  success-ink: "#137a47"
  success-soft: "#e3f6ec"
typography:
  headline:
    fontFamily: "Segoe UI, system-ui, sans-serif"
    fontSize: "24px"
    fontWeight: 700
  section:
    fontFamily: "Segoe UI, system-ui, sans-serif"
    fontSize: "16px"
    fontWeight: 600
  title:
    fontFamily: "Segoe UI, system-ui, sans-serif"
    fontSize: "15px"
    fontWeight: 600
  body:
    fontFamily: "Segoe UI, system-ui, sans-serif"
    fontSize: "13px"
    fontWeight: 400
  body-strong:
    fontFamily: "Segoe UI, system-ui, sans-serif"
    fontSize: "13px"
    fontWeight: 600
  caption:
    fontFamily: "Segoe UI, system-ui, sans-serif"
    fontSize: "12px"
    fontWeight: 400
rounded:
  swatch: "10px"
  field: "10px"
  control: "12px"
  tile: "14px"
  panel: "16px"
  card: "18px"
  dialog: "24px"
  pill: "9999px"
spacing:
  hairline: "4px"
  tight: "8px"
  row: "12px"
  card-gap: "14px"
  card-pad: "18px"
  column-gap: "22px"
  page-pad: "32px"
components:
  button-primary:
    backgroundColor: "{colors.accent-strong}"
    textColor: "{colors.surface}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.pill}"
    height: "38px"
  button-primary-hover:
    backgroundColor: "{colors.accent-strong-hover}"
  button-secondary:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.accent-ink}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.pill}"
    height: "38px"
  button-secondary-hover:
    backgroundColor: "{colors.accent-soft}"
  button-ghost:
    textColor: "{colors.ink-2}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.pill}"
    height: "38px"
  button-danger:
    textColor: "{colors.danger}"
    typography: "{typography.body-strong}"
    rounded: "{rounded.pill}"
    height: "32px"
  card:
    backgroundColor: "{colors.surface}"
    rounded: "{rounded.card}"
    padding: "18px"
  nav-item:
    textColor: "{colors.ink-2}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    height: "42px"
  nav-item-selected:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.accent-ink}"
  text-field:
    backgroundColor: "{colors.sunken}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.field}"
    height: "36px"
  text-field-focus:
    backgroundColor: "{colors.surface}"
  select:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.ink}"
    typography: "{typography.body}"
    rounded: "{rounded.control}"
    height: "38px"
  segmented:
    backgroundColor: "{colors.sunken}"
    textColor: "{colors.ink-2}"
    typography: "{typography.caption}"
    rounded: "{rounded.pill}"
    height: "40px"
  segmented-selected:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.accent-ink}"
  switch-on:
    backgroundColor: "{colors.accent}"
    rounded: "{rounded.pill}"
    width: "42px"
    height: "24px"
  switch-off:
    backgroundColor: "{colors.switch-off}"
  icon-badge:
    backgroundColor: "{colors.accent-soft}"
    textColor: "{colors.accent}"
    rounded: "{rounded.control}"
    size: "36px"
  unsaved-pill:
    backgroundColor: "{colors.warn-soft}"
    textColor: "{colors.warn-ink}"
    typography: "{typography.caption}"
    rounded: "{rounded.pill}"
    height: "32px"
---

# Design System: tile-rs

## Overview

**Creative North Star: "The Soft Control Room"**

The launcher is a light, soft settings dashboard: a lavender-grey canvas holding white, generously rounded cards that float on low, diffuse shadows. A white icon-and-label sidebar anchors the left; the main column carries a page title, a one-line subtitle, and the Save pill at top right. Everything a user changes is shown before it is saved, and the system's one expressive device is the live layout miniature, a four-tile diagram that follows the chosen layout, gap and master count, used as the logo mark, the layout picker and the welcome badge.

Density is calm, not sparse: 13px body in Segoe UI, 44 to 54px setting rows, 18px card padding. One blue does all the signalling; neutrals are cool and slightly violet so white cards read as raised without borders doing the work. The world was pinned by the user's own reference (a soft file-manager dashboard) and deliberately refuses the dense dark form-list the launcher used to be. Light theme only for now.

**Key Characteristics:**
- Lavender-grey canvas, white 18px cards, soft offset shadows.
- One blue accent family; filled controls use the stronger blue.
- Pill buttons, rounded-square icon badges, outline icons on a 24px grid.
- The live layout miniature as the signature and brand mark.
- Russian UI text in Segoe UI; nothing smaller than 12px.

## Colors

A cool, near-white neutral field with a single saturated blue carrying selection, action and data.

### Primary
- **Signal Blue** (accent): the product's brand blue, confirmed by the user. Switch tracks when on, ring arcs, progress fills, the active workspace dot, icon tints inside icon badges, the update badge dot, and the 2px focus ring on light surfaces.
- **Deep Press Blue** (accent-strong): every filled control. Primary pills, the selected layout card, the welcome badge. Darkens to accent-strong-hover on hover and press.
- **Ice Wash** (accent-soft): selected nav item, icon-badge tiles, secondary-button hover, the update card fill, ring tracks, popup selected option.
- **Ink Blue** (accent-ink): text and icons that sit on Ice Wash or on white as an accent (selected nav label, secondary-button label, selected segment, selected option).
- **Pale Rim** (accent-hover-line): the 1px border that bluish-tints on hover for secondary buttons, selects and unselected layout cards.
- **Focus on Blue** (focus-on-accent): the focus ring drawn on top of filled blue surfaces, where Signal Blue would vanish.

### Neutral
- **Lavender Mist** (canvas): the window background behind the main column.
- **Paper White** (surface): cards, sidebar, popups, dialogs, secondary buttons.
- **Sunken Grey** (sunken): wells inside cards (text fields, steppers, segmented tracks, preview frames, sidebar version panel) and hover fill for rows and nav items.
- **Hairline** (line): dividers, sidebar edge, field and button borders at rest.
- **Card Rim** (card-line): the 1px edge on white cards.
- **Switch Off** (switch-off): the off-state switch track.
- **Night Ink** (ink): titles, labels, values.
- **Slate** (ink-2): subtitles, secondary copy, ghost-button labels, nav labels at rest.
- **Muted Slate** (muted): hints under labels, captions, resting icons, placeholders.

### Status
- **Danger** and **Danger Wash**: destructive ghost buttons, delete icon buttons on hover, key conflicts ("Уже занято"), update errors.
- **Warn Ink** and **Warn Wash**: only the "unsaved changes" pill.
- **Success Ink** and **Success Wash**: the up-to-date state of the update status tile (wash tile, ink icon).

### Named Rules
**The One Blue Rule.** Blue is the only hue that means "chosen", "act" or "data". No second accent; status colours appear only as soft-wash plus ink pairs and only for real status.

**The Filled-Means-Strong Rule.** A blue surface that carries white text uses accent-strong, never accent; accent is for strokes, dots, arcs and tints.

## Typography

**Display Font:** Segoe UI (system)
**Body Font:** Segoe UI (system)

**Character:** The Windows system face, set in a short ramp of weights 400, 600 and 700. Hierarchy comes from size and weight, never from colour or case.

### Hierarchy
- **Headline** (700, 24px): the page title in the main header. The welcome dialog title steps down to 22px and the update page title to 20px, same weight.
- **Section** (600, 16px): section labels that sit directly on the canvas above a card group, and ring/stat values.
- **Title** (600, 15px): card titles beside an icon badge, the stepper value. Layout-card names and hotkey group names use 14px/600.
- **Body** (400, 13px): setting labels, field values, nav labels, status line. Body-strong (600, 13px) for button labels and stat-card titles.
- **Caption** (400, 12px): hints under labels, captions, keycaps (600), segmented options.

### Named Rules
**The 12px Floor Rule.** No text below 12px anywhere in the launcher.

**The Two-Line Setting Rule.** Every setting is a 13px ink label with an optional 12px muted hint beneath it; the control sits right-aligned on the same row.

## Layout

A fixed 236px white sidebar (16px side padding, 42px nav items, 4px apart; version-and-quit panel pinned to the bottom) beside a fluid main column with 32px side padding and 26px top padding. The header row is 52px: title and subtitle stack left, the unsaved pill and Save pill right. Below it a single scroll view holds the page, with 18px between blocks.

Pages are composed of section labels on the canvas followed by cards; the General page runs a fluid left column beside a fixed 256px summary column with a 22px gutter. Inside cards: 18px padding, 4px between setting rows, rows 44px (label only) or 54px (label plus hint), 10px inner row padding so hover fills breathe. Card stacks are 14px apart; tile rows (layout cards) 12px apart. The window minimum is 1000 x 640; the main column flexes, the sidebar and summary column do not.

## Elevation & Depth

Hybrid: tonal layering does most of the work (canvas, then white surface, then sunken wells inside), and a soft, downward-offset, cool-tinted shadow lifts every white card off the canvas. Shadows are ambient, never structural lines. Interactive emphasis adds elevation: the selected layout card and primary pill cast a blue-tinted shadow.

### Shadow Vocabulary
- **Card** (offset-y 6px, blur 22px, shadow #22325a1a): every white card and unselected layout card.
- **Selected tile** (offset-y 10px, blur 24px, #0070e04d): the chosen layout card only.
- **Primary pill** (offset-y 4px, blur 12px, #0070e040): filled primary buttons.
- **Raised segment** (offset-y 2px, blur 6px, shadow): the chosen segment inside a segmented track; the visible workspace-indicator pill.
- **Popup** (offset-y 10px, blur 28px, #22325a26): the select drop-down.
- **Dialog** (offset-y 18px, blur 48px, #1d243340) over a #1d243373 scrim: the welcome card.
- **Thumb** (offset-y 1px, blur 3px, #1d243340): switch knob.

### Named Rules
**The Soft Lift Rule.** Every shadow is blurred at least twice its offset and tinted with the cool ink, never pure black. Depth reads as light, not as outline.

## Shapes

Generous, nested rounding: the outer container is always the roundest. Cards 18px, sidebar panel and header pills 16px, icon tiles and preview wells 14px, controls and nav items 12px, fields and swatches 10px, keycaps 8px, preview tiles 5px; the welcome dialog 24px. Every button, switch, segmented track, stepper and status pill is a full pill (radius = half height). Icons are authored outline SVGs on a 24px grid, 1.75 stroke, round caps and joins, rendered at 18px (16px inside buttons, 22px in feature tiles) and tinted at runtime.

## Components

### Buttons
Soft, confident pills.
- **Shape:** full pill; 38px tall, 32px compact.
- **Primary:** Deep Press Blue fill, white 13px/600 label with an optional 16px leading icon, blue-tinted lift; hover darkens.
- **Secondary:** white with a 1px Hairline border and Ink Blue label; hover washes to Ice Wash with a Pale Rim border.
- **Ghost / Danger ghost:** transparent; hover fills Sunken Grey (ghost) or Danger Wash (danger), danger label and icon in Danger.
- **Save:** the header Save pill is secondary and reads "Сохранено" when clean, turns primary and reads "Сохранить" when dirty.
- **Focus:** a 2px inset ring, Signal Blue on light pills, Focus on Blue on filled ones. Disabled is 50% opacity.
- **Icon button:** 34px circle, transparent, muted icon; hover Ice Wash with Ink Blue icon (Danger Wash and Danger for destructive).

### Cards / Containers
- **Corner Style:** 18px.
- **Background:** Paper White; the feature card (updates) may take Ice Wash with a pale-blue rim.
- **Shadow Strategy:** Card shadow (see Elevation).
- **Border:** 1px Card Rim.
- **Internal Padding:** 18px.
- **Card title:** a 36px Ice Wash rounded-square icon badge with a Signal Blue outline icon, then a 15px/600 title and optional 12px subtitle.
- **Stat card:** 96px tall, 13px/600 title and 12px caption left, a 64px Ring (6px stroke, Ice Wash track, Signal Blue arc, real ratio only) or a 64px Ice Wash value disc right.

### Inputs / Fields
- **Text field:** 36px, 10px radius, Sunken Grey fill, 1px Hairline border; hover darkens the border; focus turns the fill white with a 2px Signal Blue border. Selection is a pale blue.
- **Select:** 38px white, 12px radius, 1px border that turns Pale Rim on hover, muted chevron; opens a white 14px-radius popup with 38px options, the current one in Ice Wash and Ink Blue.
- **Switch:** 42 x 24 pill, Signal Blue when on, Switch Off when off, white 18px knob that slides on a soft overshoot curve. Always used inside a ToggleRow, whose whole row toggles and hovers Sunken Grey.
- **Segmented:** Sunken Grey pill track; the chosen segment is a raised white pill with Ink Blue 12px/600 text.
- **Stepper:** 124 x 38 Sunken Grey pill with minus and plus icon buttons around a 15px/600 value.
- **Swatch:** 34px rounded square showing the parsed hex, 1px translucent edge.
- **Keycap:** 28px tall, 8px radius, white with a 1px border and 12px/600 key name; conflicts turn Danger Wash and Danger.
- **Error:** inline Danger text beside the control; no red borders on fields.

### Navigation
- **Sidebar item:** 42px, 12px radius, 18px outline icon plus 13px label. Rest: muted icon, Slate label. Hover: Sunken Grey fill. Selected: Ice Wash fill, Signal Blue icon, Ink Blue 600 label. An 8px Signal Blue dot marks an available update. Focus draws a 2px blue edge.

### Unsaved Pill
A 32px Warn Wash pill with an amber dot and 12px Warn Ink "Есть несохранённые изменения", shown left of Save only while changes are pending.

### Live Layout Miniature (signature)
Four rounded tiles (5px) arranged exactly as the selected layout arranges windows (spiral, master and stack, grid, columns), following the configured gap and master count and animating between layouts over 260ms on an in-out curve. On light ground the tiles step from Signal Blue to paler blues inside a Sunken Grey well; on a filled blue card they turn white at descending opacity. It is the sidebar logo mark (32px), the layout picker preview (92px tall), and the welcome badge. It is the one place the system draws a picture.

### Workspace Dots
A 28px cell per workspace: grey dot when empty, light blue when occupied, Signal Blue with a pale halo when active.

## Do's and Don'ts

### Do:
- **Do** put every setting in a white 18px card on the lavender canvas, with a 16px/600 section label sitting on the canvas above it.
- **Do** use Deep Press Blue for anything filled and white-labelled, Signal Blue for strokes, dots and arcs, Ice Wash for selection.
- **Do** pair every label with a plain-language 12px muted hint when the effect is not obvious.
- **Do** reuse the live layout miniature wherever a layout is shown; never substitute a static picture.
- **Do** gate every animation on the motion setting (AppTheme.motion); 140ms for state colour, 160 to 260ms for movement.
- **Do** draw new icons as 24px outline SVGs, 1.75 stroke, round caps and joins, tinted at runtime.

### Don't:
- **Don't** introduce a second accent hue or use status colours for decoration.
- **Don't** set text below 12px.
- **Don't** use black or hard-edged shadows on cards; keep the soft, cool, blurred lift.
- **Don't** use square buttons; actions are pills.
- **Don't** show a status, count or ring the launcher cannot actually read.
- **Don't** build a dense dark form-list; this world is light, soft and carded.
