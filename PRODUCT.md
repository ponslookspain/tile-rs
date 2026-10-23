# Product

<!-- impeccable:product-schema 1 -->

## Platform

Native Windows desktop (Windows 10/11, x64). The settings launcher is a Slint window
(software renderer, winit backend) running in its own process; it is not a web surface.

## Users

An ordinary Windows user who has never used i3, Hyprland or another tiling manager. They
configure with the mouse, rarely open `config.toml`, and visit the settings window occasionally:
on first start, to change a layout or a shortcut, to make a stubborn application behave, or to
install an update. (Confirmed by the user, 2026-10-21.)

## Product Purpose

tile-rs is a small tiling window manager for Windows 10/11 in a single `.exe`: windows arrange
themselves in a layout, move and resize by keyboard or mouse, across 9 workspaces per monitor.
The settings launcher is where a user sees what tile-rs is doing and changes how it behaves
without editing TOML. Success: every common change is possible from the launcher, and the user
understands what a setting does before saving it.

## Positioning

Tiling for people who do not live in a terminal: live mouse resizing with neighbours following,
windows that open straight into their tile, a graphical launcher, an installer with auto-update.
Komorebi and GlazeWM are configured through JSON/YAML; tile-rs is configured here.

## Capabilities and Constraints

- Launcher pages: General, Layout, Workspaces, Window behaviour, Hotkeys, Applications,
  Advanced, Updates. Changes apply with an explicit Save; an unsaved-changes indicator exists.
- Hotkey editor records combinations by pressing them; application rules are made from a picker
  of running windows; logic lives in `src/ui/launcher_model.rs`, the UI only binds to it.
- Slint constraints: no web fonts or CSS; icons must be bundled images (SVG supported);
  `drop-shadow-*` properties are available on rectangles; minimum window 1000 × 640.
- The Slint Royalty-Free license requires the `AboutSlint` widget on a reachable About screen.
- UI language: Russian. A language switch is planned, not built.
- Light theme only for now (confirmed 2026-10-21); a dark theme may follow.

## Brand Commitments

- Accent colour: blue `#008cff` (the existing `accent_color` default), confirmed by the user.
- The user supplied a binding visual reference: a light, soft file-manager dashboard
  (lavender-grey canvas, white rounded cards, soft shadows, icon + label sidebar, stat cards with
  ring indicators, outline icons).

## Evidence on Hand

No screenshots, users, metrics or testimonials exist. Launcher data is real: version from
`Cargo.toml`, configuration values, hotkey bindings, rules, running windows, update state.

## Product Principles

1. Every common change is reachable without TOML.
2. Show what will happen before it happens (previews, plain-language labels).
3. Quiet tool: open, change, save, close; never compete with the user's own windows.
4. Honest state: never show a status the launcher cannot actually read.
